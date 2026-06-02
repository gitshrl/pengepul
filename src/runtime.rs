use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::app::create_app;
use crate::cli::{CliRuntime, ServiceInstallRequest};
use crate::config::{Config, DebugMode};
use crate::oauth::{
    ANTHROPIC_REDIRECT_URI, CODEX_CALLBACK_PATH, CODEX_CALLBACK_PORT, exchange_anthropic_code,
    exchange_codex_code, generate_anthropic_auth_url, generate_codex_auth_url,
};
use crate::providers::ProviderRegistry;
use crate::service::{ServiceOptions, run_command};
use crate::tokens::save_token;
use crate::types::{PkceCodes, ProviderId, TokenData};
use crate::utils::{generate_pkce_codes, random_urlsafe, sha256_hex};

/// Install the tracing subscriber for `serve`.
///
/// `RUST_LOG` overrides everything; otherwise the level follows the `debug` config:
/// `off`/`errors` log at info (startup banner + upstream warnings/errors), `verbose` adds
/// per-request debug detail.
fn init_tracing(debug: DebugMode) {
    let default = match debug {
        DebugMode::Off | DebugMode::Errors => "pengepul=info",
        DebugMode::Verbose => "pengepul=debug",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

pub struct RealRuntime {
    runtime: tokio::runtime::Runtime,
}

impl RealRuntime {
    /// Create the concrete async runtime used by the binary.
    ///
    /// # Errors
    ///
    /// Returns an error if the Tokio runtime cannot be built.
    pub fn new() -> Result<Self> {
        Ok(Self {
            runtime: tokio::runtime::Runtime::new().context("failed to create Tokio runtime")?,
        })
    }
}

impl CliRuntime for RealRuntime {
    fn run_server(&mut self, config: &Config, _registry: &ProviderRegistry) -> Result<()> {
        init_tracing(config.debug);
        let bind_addr = server_bind_addr(config);
        let app = create_app(config.clone());
        self.runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind(&bind_addr)
                .await
                .with_context(|| format!("failed to bind {bind_addr}"))?;
            tracing::info!("pengepul listening on {bind_addr}");
            axum::serve(listener, app).await.context("server failed")
        })
    }

    fn health(&mut self, base_url: &str) -> Result<Value> {
        self.runtime
            .block_on(request_json(Method::Get, base_url, "/health", None))
    }

    fn accounts(&mut self, base_url: &str, api_key: &str) -> Result<Value> {
        self.runtime.block_on(request_json(
            Method::Get,
            base_url,
            "/admin/accounts",
            Some(api_key),
        ))
    }

    fn reload_accounts(&mut self, base_url: &str, api_key: &str) -> Result<Value> {
        self.runtime.block_on(request_json(
            Method::Post,
            base_url,
            "/admin/reload",
            Some(api_key),
        ))
    }

    fn install_service(&mut self, request: ServiceInstallRequest) -> Result<PathBuf> {
        let executable = std::env::current_exe().context("failed to resolve current executable")?;
        let home = home_dir()?;
        let options = ServiceOptions {
            executable,
            config_path: request.config_path,
            host: request.host,
            port: request.port,
        };
        install_platform_service(&options, &home, request.start, request.enable)
    }

    fn start_service(&mut self) -> Result<()> {
        control_platform_service("start")
    }

    fn stop_service(&mut self) -> Result<()> {
        control_platform_service("stop")
    }

    fn restart_service(&mut self) -> Result<()> {
        control_platform_service("restart")
    }

    fn service_status(&mut self) -> Result<String> {
        platform_service_status()
    }

    fn uninstall_service(&mut self) -> Result<PathBuf> {
        let home = home_dir()?;
        uninstall_platform_service(&home)
    }

    fn service_logs(&mut self, follow: bool, lines: u32) -> Result<()> {
        platform_service_logs(follow, lines)
    }

    fn login(
        &mut self,
        config: &Config,
        provider: ProviderId,
        manual: bool,
        key: Option<&str>,
    ) -> Result<String> {
        if provider == ProviderId::Opencode {
            return save_opencode_login(config, key);
        }
        let state = random_urlsafe(32);
        let pkce = generate_pkce_codes();
        let auth_url = auth_url(provider, &state, &pkce);
        println!("\nOpen this URL to authorize {provider}:\n\n{auth_url}\n");
        if !manual {
            open_browser(&auth_url);
        }
        let callback = if manual {
            manual_callback()?
        } else {
            let (port, path) = callback_endpoint(provider)?;
            wait_for_callback(port, path, Duration::from_mins(5))?
        };
        let token = self.runtime.block_on(async {
            match provider {
                ProviderId::Anthropic => {
                    exchange_anthropic_code(&callback.code, &callback.state, &state, &pkce).await
                }
                ProviderId::Codex => {
                    exchange_codex_code(&callback.code, &callback.state, &state, &pkce).await
                }
                ProviderId::Opencode => {
                    unreachable!("opencode login is handled before the OAuth flow")
                }
            }
        })?;
        let email = token.email.clone();
        save_token(&config.auth_dir, &token)?;
        Ok(email)
    }
}

#[derive(Clone, Copy)]
enum Method {
    Get,
    Post,
}

async fn request_json(
    method: Method,
    base_url: &str,
    path: &str,
    api_key: Option<&str>,
) -> Result<Value> {
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);
    let client = reqwest::Client::new();
    let request = match method {
        Method::Get => client.get(&url),
        Method::Post => client.post(&url),
    };
    let request = if let Some(api_key) = api_key {
        request.bearer_auth(api_key)
    } else {
        request
    };
    let response = request
        .send()
        .await
        .with_context(|| format!("failed to request {url}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("failed to read response from {url}"))?;
    if !status.is_success() {
        bail!(
            "{} returned {}: {}",
            url,
            status,
            String::from_utf8_lossy(&body)
        );
    }
    serde_json::from_slice(&body).with_context(|| format!("{url} returned invalid JSON"))
}

fn server_bind_addr(config: &Config) -> String {
    let host = if config.host.is_empty() {
        "127.0.0.1"
    } else {
        &config.host
    };
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{}", config.port)
    } else {
        format!("{host}:{}", config.port)
    }
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[derive(Debug, Clone)]
struct CallbackResult {
    code: String,
    state: String,
}

fn auth_url(provider: ProviderId, state: &str, pkce: &PkceCodes) -> String {
    match provider {
        ProviderId::Anthropic => generate_anthropic_auth_url(state, pkce),
        ProviderId::Codex => generate_codex_auth_url(state, pkce),
        ProviderId::Opencode => unreachable!("opencode has no OAuth authorize URL"),
    }
}

fn callback_endpoint(provider: ProviderId) -> Result<(u16, &'static str)> {
    match provider {
        ProviderId::Anthropic => {
            let url = url::Url::parse(ANTHROPIC_REDIRECT_URI)?;
            let port = url.port().context("Anthropic redirect URI has no port")?;
            Ok((port, "/callback"))
        }
        ProviderId::Codex => Ok((CODEX_CALLBACK_PORT, CODEX_CALLBACK_PATH)),
        ProviderId::Opencode => bail!("opencode has no OAuth callback"),
    }
}

/// Resolve the opencode API key and persist it as a degenerate, refresh-less token.
///
/// The key comes from `--key` when provided, otherwise it is imported from opencode's own
/// `auth.json`. The stored token has an empty refresh token and a far-future expiry so the
/// account manager's refresh policy never fires.
fn save_opencode_login(config: &Config, key: Option<&str>) -> Result<String> {
    let key = match key {
        Some(key) if !key.trim().is_empty() => key.trim().to_string(),
        _ => import_opencode_key()
            .context("no opencode key: pass --key or run `opencode auth login` first")?,
    };
    let email = format!("opencode-{}", &sha256_hex(&key)[..8]);
    let token = TokenData {
        access_token: key,
        refresh_token: String::new(),
        email: email.clone(),
        expires_at: "9999-12-31T23:59:59Z".to_string(),
        account_uuid: String::new(),
        provider: ProviderId::Opencode,
        id_token: None,
        last_refresh_at: None,
        plan_type: None,
    };
    save_token(&config.auth_dir, &token)?;
    Ok(email)
}

fn import_opencode_key() -> Result<String> {
    let xdg_data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    import_opencode_key_from(&opencode_auth_json_paths(xdg_data_home, home))
}

/// Read the first opencode key found across `paths`, skipping unreadable/garbage files.
fn import_opencode_key_from(paths: &[PathBuf]) -> Result<String> {
    for path in paths {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(key) = opencode_key_from_auth_json(&value) {
            return Ok(key);
        }
    }
    bail!("opencode key not found in opencode auth.json")
}

/// opencode stores credentials under `$XDG_DATA_HOME/opencode` (preferred) then
/// `$HOME/.local/share/opencode`.
fn opencode_auth_json_paths(xdg_data_home: Option<PathBuf>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(data_home) = xdg_data_home {
        paths.push(data_home.join("opencode/auth.json"));
    }
    if let Some(home) = home {
        paths.push(home.join(".local/share/opencode/auth.json"));
    }
    paths
}

#[must_use]
fn opencode_key_from_auth_json(value: &Value) -> Option<String> {
    // opencode itself stores its credentials under the literal key "opencode-go" — keep this
    // string as-is even though our provider is named "opencode".
    let entry = value.get("opencode-go")?;
    if entry.get("type").and_then(Value::as_str) != Some("api") {
        return None;
    }
    entry
        .get("key")
        .and_then(Value::as_str)
        .filter(|key| !key.is_empty())
        .map(ToOwned::to_owned)
}

fn manual_callback() -> Result<CallbackResult> {
    let value = prompt_line("Paste the full callback URL or authorization code: ")?;
    if value.starts_with("http://") || value.starts_with("https://") {
        return callback_from_url(&value);
    }
    let state = prompt_line("Paste returned state: ")?;
    if value.is_empty() || state.is_empty() {
        bail!("manual login requires code and state");
    }
    Ok(CallbackResult { code: value, state })
}

fn prompt_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout()
        .flush()
        .context("failed to flush stdout")?;
    let mut value = String::new();
    std::io::stdin()
        .read_line(&mut value)
        .context("failed to read stdin")?;
    Ok(value.trim().to_string())
}

fn wait_for_callback(port: u16, callback_path: &str, timeout: Duration) -> Result<CallbackResult> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("failed to listen on 127.0.0.1:{port}"))?;
    listener
        .set_nonblocking(true)
        .context("failed to make callback listener nonblocking")?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => return handle_callback_stream(&mut stream, callback_path),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error).context("failed to accept OAuth callback"),
        }
    }
    bail!("OAuth callback timeout")
}

fn handle_callback_stream(
    stream: &mut std::net::TcpStream,
    callback_path: &str,
) -> Result<CallbackResult> {
    let mut request_line = String::new();
    {
        let mut reader = std::io::BufReader::new(&mut *stream);
        reader
            .read_line(&mut request_line)
            .context("failed to read callback request")?;
    }
    let target = request_line
        .split_whitespace()
        .nth(1)
        .context("callback request is missing target")?;
    let parsed = url::Url::parse(&format!("http://localhost{target}"))
        .context("callback request target is not a URL")?;
    if parsed.path() != callback_path {
        write_http_response(stream, 404, "text/plain", b"not found")?;
        bail!("unexpected OAuth callback path: {}", parsed.path());
    }
    if let Some(error) = parsed
        .query_pairs()
        .find_map(|(key, value)| (key == "error").then_some(value.into_owned()))
    {
        write_http_response(stream, 400, "text/plain", error.as_bytes())?;
        bail!("OAuth error: {error}");
    }
    let code = query_value(&parsed, "code");
    let state = query_value(&parsed, "state");
    let (Some(code), Some(state)) = (code, state) else {
        write_http_response(stream, 400, "text/plain", b"missing code or state")?;
        bail!("callback URL is missing code or state");
    };
    write_http_response(
        stream,
        200,
        "text/html",
        b"<!doctype html><html><body><h1>Login successful</h1><p>You can close this tab and return to the terminal.</p></body></html>",
    )?;
    Ok(CallbackResult { code, state })
}

fn callback_from_url(value: &str) -> Result<CallbackResult> {
    let parsed = url::Url::parse(value).context("callback URL is invalid")?;
    let code = query_value(&parsed, "code");
    let state = query_value(&parsed, "state");
    let (Some(code), Some(state)) = (code, state) else {
        bail!("callback URL is missing code or state");
    };
    Ok(CallbackResult { code, state })
}

fn query_value(url: &url::Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find_map(|(key, value)| (key == name).then_some(value.into_owned()))
}

fn write_http_response(
    stream: &mut std::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let reason = match status {
        400 => "Bad Request",
        404 => "Not Found",
        _ => "OK",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .context("failed to write callback response headers")?;
    stream
        .write_all(body)
        .context("failed to write callback response body")
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = ("open", vec![url]);
    #[cfg(target_os = "linux")]
    let command = ("xdg-open", vec![url]);
    #[cfg(target_os = "windows")]
    let command = ("cmd", vec!["/C", "start", url]);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let command = ("", Vec::<&str>::new());

    if !command.0.is_empty() {
        let _ = std::process::Command::new(command.0)
            .args(command.1)
            .status();
    }
}

#[cfg(target_os = "linux")]
fn install_platform_service(
    options: &ServiceOptions,
    home: &std::path::Path,
    start: bool,
    enable: bool,
) -> Result<PathBuf> {
    crate::service::install_systemd_service(options, home, start, enable, run_command)
}

#[cfg(target_os = "linux")]
fn control_platform_service(action: &str) -> Result<()> {
    run_command(&[
        "systemctl".to_string(),
        "--user".to_string(),
        action.to_string(),
        crate::service::SYSTEMD_UNIT_NAME.to_string(),
    ])?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_service_status() -> Result<String> {
    command_output(&[
        "systemctl",
        "--user",
        "status",
        "--no-pager",
        crate::service::SYSTEMD_UNIT_NAME,
    ])
}

#[cfg(target_os = "linux")]
fn uninstall_platform_service(home: &std::path::Path) -> Result<PathBuf> {
    let path = home
        .join(".config/systemd/user")
        .join(crate::service::SYSTEMD_UNIT_NAME);
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", crate::service::SYSTEMD_UNIT_NAME])
        .status();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", crate::service::SYSTEMD_UNIT_NAME])
        .status();
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    run_command(&[
        "systemctl".to_string(),
        "--user".to_string(),
        "daemon-reload".to_string(),
    ])?;
    Ok(path)
}

#[cfg(target_os = "linux")]
fn platform_service_logs(follow: bool, lines: u32) -> Result<()> {
    let mut command = vec![
        "journalctl".to_string(),
        "--user".to_string(),
        "-u".to_string(),
        crate::service::SYSTEMD_UNIT_NAME.to_string(),
        "-n".to_string(),
        lines.to_string(),
    ];
    if follow {
        command.push("-f".to_string());
    } else {
        command.push("--no-pager".to_string());
    }
    run_log_viewer(&command)
}

#[cfg(target_os = "macos")]
fn install_platform_service(
    options: &ServiceOptions,
    home: &std::path::Path,
    start: bool,
    _enable: bool,
) -> Result<PathBuf> {
    crate::service::install_launchd_service(options, home, current_uid()?, start, run_command)
}

#[cfg(target_os = "macos")]
fn control_platform_service(action: &str) -> Result<()> {
    let uid = current_uid()?;
    let target = format!("gui/{uid}/{}", crate::service::LAUNCHD_LABEL);
    let command = match action {
        "start" => vec!["launchctl", "kickstart", &target],
        "stop" => vec!["launchctl", "bootout", &target],
        "restart" => vec!["launchctl", "kickstart", "-k", &target],
        other => bail!("unknown service action: {other}"),
    };
    run_command(
        &command
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_service_status() -> Result<String> {
    let uid = current_uid()?;
    command_output(&[
        "launchctl",
        "print",
        &format!("gui/{uid}/{}", crate::service::LAUNCHD_LABEL),
    ])
}

#[cfg(target_os = "macos")]
fn uninstall_platform_service(home: &std::path::Path) -> Result<PathBuf> {
    let uid = current_uid()?;
    let target = format!("gui/{uid}/{}", crate::service::LAUNCHD_LABEL);
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &target])
        .status();
    let path = home
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", crate::service::LAUNCHD_LABEL));
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(path)
}

#[cfg(target_os = "macos")]
fn platform_service_logs(follow: bool, lines: u32) -> Result<()> {
    let logs = home_dir()?.join(".pengepul/logs");
    let mut command = vec!["tail".to_string(), "-n".to_string(), lines.to_string()];
    if follow {
        command.push("-f".to_string());
    }
    command.push(logs.join("service.log").to_string_lossy().into_owned());
    command.push(logs.join("service.err.log").to_string_lossy().into_owned());
    run_log_viewer(&command)
}

#[cfg(target_os = "macos")]
fn current_uid() -> Result<u32> {
    let output = std::process::Command::new("id")
        .arg("-u")
        .output()
        .context("failed to run id -u")?;
    if !output.status.success() {
        bail!("id -u exited with {}", output.status);
    }
    String::from_utf8(output.stdout)
        .context("id -u returned non-UTF-8")?
        .trim()
        .parse::<u32>()
        .context("id -u returned invalid uid")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn install_platform_service(
    _options: &ServiceOptions,
    _home: &std::path::Path,
    _start: bool,
    _enable: bool,
) -> Result<PathBuf> {
    bail!("service install is only supported on Linux and macOS")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn control_platform_service(_action: &str) -> Result<()> {
    bail!("service control is only supported on Linux and macOS")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_service_status() -> Result<String> {
    bail!("service status is only supported on Linux and macOS")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn uninstall_platform_service(_home: &std::path::Path) -> Result<PathBuf> {
    bail!("service uninstall is only supported on Linux and macOS")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_service_logs(_follow: bool, _lines: u32) -> Result<()> {
    bail!("service logs are only supported on Linux and macOS")
}

fn run_log_viewer(command: &[String]) -> Result<()> {
    let Some((program, args)) = command.split_first() else {
        bail!("empty log command");
    };
    // Inherit stdio so logs stream to the terminal; the exit code is ignored because
    // `journalctl -f` / `tail -f` exit non-zero when interrupted with Ctrl-C.
    std::process::Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    Ok(())
}

fn command_output(command: &[&str]) -> Result<String> {
    let Some((program, args)) = command.split_first() else {
        bail!("empty command");
    };
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    if !output.status.success() {
        bail!("{program} exited with {}", output.status);
    }
    let stdout = String::from_utf8(output.stdout).context("command stdout was not UTF-8")?;
    if stdout.trim().is_empty() {
        Ok(String::from_utf8(output.stderr)
            .context("command stderr was not UTF-8")?
            .trim_end()
            .to_string())
    } else {
        Ok(stdout.trim_end().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{import_opencode_key_from, opencode_auth_json_paths, opencode_key_from_auth_json};
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn reads_opencode_api_key() {
        // opencode's auth.json uses the literal key "opencode-go" — that's an external contract.
        let auth = json!({
            "zai-coding-plan": {"type": "api", "key": "zai"},
            "opencode-go": {"type": "api", "key": "sk-go"}
        });
        assert_eq!(opencode_key_from_auth_json(&auth).as_deref(), Some("sk-go"));
    }

    #[test]
    fn ignores_missing_non_api_or_empty_entries() {
        assert_eq!(opencode_key_from_auth_json(&json!({})), None);
        assert_eq!(
            opencode_key_from_auth_json(&json!({"opencode-go": {"type": "oauth", "key": "x"}})),
            None
        );
        assert_eq!(
            opencode_key_from_auth_json(&json!({"opencode-go": {"type": "api", "key": ""}})),
            None
        );
    }

    #[test]
    fn auth_json_paths_prefer_xdg_then_home() {
        assert_eq!(
            opencode_auth_json_paths(Some(PathBuf::from("/xdg")), Some(PathBuf::from("/home/u"))),
            vec![
                PathBuf::from("/xdg/opencode/auth.json"),
                PathBuf::from("/home/u/.local/share/opencode/auth.json"),
            ]
        );
        assert_eq!(
            opencode_auth_json_paths(None, Some(PathBuf::from("/home/u"))),
            vec![PathBuf::from("/home/u/.local/share/opencode/auth.json")]
        );
        assert_eq!(opencode_auth_json_paths(None, None), Vec::<PathBuf>::new());
    }

    #[test]
    fn import_skips_unreadable_and_garbage_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("missing.json");
        let garbage = dir.path().join("garbage.json");
        std::fs::write(&garbage, "not json {").expect("write garbage");
        let valid = dir.path().join("valid.json");
        std::fs::write(
            &valid,
            json!({"opencode-go": {"type": "api", "key": "sk-go"}}).to_string(),
        )
        .expect("write valid");

        let key = import_opencode_key_from(&[missing, garbage, valid]).expect("key");
        assert_eq!(key, "sk-go");

        assert!(import_opencode_key_from(&[]).is_err());
    }
}
