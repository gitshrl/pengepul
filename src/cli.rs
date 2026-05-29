use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::Value;

use crate::config::{Config, load_config, selected_config_path};
use crate::providers::{ProviderRegistry, build_registry};
use crate::types::ProviderId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInstallRequest {
    pub config_path: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub start: bool,
    pub enable: bool,
}

pub trait CliRuntime {
    /// Start the HTTP relay.
    ///
    /// # Errors
    ///
    /// Returns an error if the server cannot be started.
    fn run_server(&mut self, config: &Config, registry: &ProviderRegistry) -> Result<()>;

    /// Fetch local relay health.
    ///
    /// # Errors
    ///
    /// Returns an error if the admin request fails.
    fn health(&mut self, base_url: &str) -> Result<Value>;

    /// Fetch runtime account state.
    ///
    /// # Errors
    ///
    /// Returns an error if the admin request fails.
    fn accounts(&mut self, base_url: &str, api_key: &str) -> Result<Value>;

    /// Reload runtime account state.
    ///
    /// # Errors
    ///
    /// Returns an error if the admin request fails.
    fn reload_accounts(&mut self, base_url: &str, api_key: &str) -> Result<Value>;

    /// Install the user service.
    ///
    /// # Errors
    ///
    /// Returns an error if service installation fails.
    fn install_service(&mut self, request: ServiceInstallRequest) -> Result<PathBuf>;

    /// Start the installed user service.
    ///
    /// # Errors
    ///
    /// Returns an error if the service manager command fails.
    fn start_service(&mut self) -> Result<()>;

    /// Stop the installed user service.
    ///
    /// # Errors
    ///
    /// Returns an error if the service manager command fails.
    fn stop_service(&mut self) -> Result<()>;

    /// Restart the installed user service.
    ///
    /// # Errors
    ///
    /// Returns an error if the service manager command fails.
    fn restart_service(&mut self) -> Result<()>;

    /// Return service manager status text.
    ///
    /// # Errors
    ///
    /// Returns an error if the service manager command fails.
    fn service_status(&mut self) -> Result<String>;

    /// Uninstall the user service.
    ///
    /// # Errors
    ///
    /// Returns an error if the service cannot be stopped, disabled, or removed.
    fn uninstall_service(&mut self) -> Result<PathBuf>;

    /// Authorize and save an upstream account.
    ///
    /// # Errors
    ///
    /// Returns an error if OAuth authorization, token exchange, or token persistence fails.
    fn login(
        &mut self,
        config: &Config,
        provider: ProviderId,
        manual: bool,
        key: Option<&str>,
    ) -> Result<String>;
}

#[derive(Debug, Parser)]
#[command(name = "pengepul", disable_help_subcommand = true)]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// start the API relay
    Serve {
        #[arg(long = "config")]
        command_config: Option<PathBuf>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
    },
    /// authorize an upstream account
    Login {
        #[arg(long = "config")]
        command_config: Option<PathBuf>,
        #[arg(long, default_value = "anthropic", value_parser = ["anthropic", "codex", "opencode-go"])]
        provider: String,
        #[arg(long)]
        manual: bool,
        /// opencode-go API key (defaults to importing it from opencode's auth.json)
        #[arg(long)]
        key: Option<String>,
    },
    /// show local server status
    Status {
        #[arg(long = "config")]
        command_config: Option<PathBuf>,
    },
    /// show loaded provider accounts
    Accounts {
        #[arg(long = "config")]
        command_config: Option<PathBuf>,
        #[arg(long)]
        reload: bool,
    },
    /// inspect config
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// manage the user service
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// show help for a command
    Help {
        #[arg(trailing_var_arg = true)]
        topic: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, Subcommand)]
enum ConfigCommand {
    /// print config path
    Path,
    /// print config YAML
    Show,
    /// print the first configured API key
    ApiKey,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// install user service
    Install {
        #[arg(long = "config")]
        command_config: Option<PathBuf>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        start: bool,
        #[arg(long)]
        enable: bool,
    },
    /// start service
    Start,
    /// stop service
    Stop,
    /// restart service
    Restart,
    /// show service manager status
    Status,
    /// remove user service
    Uninstall,
}

/// Run CLI logic against explicit filesystem roots and an injected runtime.
///
/// # Errors
///
/// Returns an error for invalid command args, invalid config, or runtime failures.
pub fn run_with_env(
    argv: &[&str],
    home: &Path,
    cwd: &Path,
    runtime: &mut impl CliRuntime,
) -> Result<RunOutcome> {
    let mut raw = Vec::with_capacity(argv.len() + 1);
    raw.push("pengepul");
    raw.extend_from_slice(argv);
    let parsed_args = Args::try_parse_from(raw)?;
    let mut output = Output::default();

    match parsed_args.command {
        None => serve(
            parsed_args.config.as_deref(),
            None,
            None,
            home,
            cwd,
            runtime,
        )?,
        Some(Command::Serve {
            command_config,
            host,
            port,
        }) => serve(
            command_config.as_deref().or(parsed_args.config.as_deref()),
            host,
            port,
            home,
            cwd,
            runtime,
        )?,
        Some(Command::Status { command_config }) => {
            status(
                command_config.as_deref().or(parsed_args.config.as_deref()),
                home,
                cwd,
                runtime,
                &mut output,
            )?;
        }
        Some(Command::Accounts {
            command_config,
            reload,
        }) => {
            accounts(
                command_config.as_deref().or(parsed_args.config.as_deref()),
                reload,
                home,
                cwd,
                runtime,
                &mut output,
            )?;
        }
        Some(Command::Config { command }) => {
            config_command(
                command,
                parsed_args.config.as_deref(),
                home,
                cwd,
                &mut output,
            )?;
        }
        Some(Command::Service { command }) => {
            service_command(command, parsed_args.config.as_deref(), runtime, &mut output)?;
        }
        Some(Command::Help { topic }) => {
            output.line(&help_text(&topic)?);
        }
        Some(Command::Login {
            command_config,
            provider,
            manual,
            key,
        }) => {
            login(
                command_config.as_deref().or(parsed_args.config.as_deref()),
                &provider,
                manual,
                key.as_deref(),
                home,
                cwd,
                runtime,
                &mut output,
            )?;
        }
    }

    Ok(RunOutcome {
        code: 0,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn serve(
    config_path: Option<&Path>,
    host: Option<String>,
    port: Option<u16>,
    home: &Path,
    cwd: &Path,
    runtime: &mut impl CliRuntime,
) -> Result<()> {
    let mut config = load_config(config_path, Some(home), cwd)?;
    if let Some(host) = host {
        config.host = host;
    }
    if let Some(port) = port {
        config.port = port;
    }
    let registry = build_registry(&config.auth_dir);
    runtime.run_server(&config, &registry)
}

fn status(
    config_path: Option<&Path>,
    home: &Path,
    cwd: &Path,
    runtime: &mut impl CliRuntime,
    output: &mut Output,
) -> Result<()> {
    let config = load_config(config_path, Some(home), cwd)?;
    let base_url = base_url(&config);
    output.line(&format!(
        "config: {}",
        selected_config_path(config_path, Some(home), cwd).display()
    ));
    output.line(&format!("url: {base_url}"));
    let health = runtime.health(&base_url)?;
    output.line(&format!(
        "server: {}",
        health
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    ));
    let accounts = runtime.accounts(&base_url, &first_api_key(&config)?)?;
    print_account_counts(&accounts, output);
    Ok(())
}

fn accounts(
    config_path: Option<&Path>,
    reload: bool,
    home: &Path,
    cwd: &Path,
    runtime: &mut impl CliRuntime,
    output: &mut Output,
) -> Result<()> {
    let config = load_config(config_path, Some(home), cwd)?;
    let base_url = base_url(&config);
    let api_key = first_api_key(&config)?;
    if reload {
        runtime.reload_accounts(&base_url, &api_key)?;
        output.line("reloaded accounts");
    }
    let accounts = runtime.accounts(&base_url, &api_key)?;
    print_accounts(&accounts, output);
    Ok(())
}

fn config_command(
    command: ConfigCommand,
    config_path: Option<&Path>,
    home: &Path,
    cwd: &Path,
    output: &mut Output,
) -> Result<()> {
    let path = selected_config_path(config_path, Some(home), cwd);
    match command {
        ConfigCommand::Path => output.line(&path.display().to_string()),
        ConfigCommand::ApiKey => {
            let config = load_config(config_path, Some(home), cwd)?;
            output.line(&first_api_key(&config)?);
        }
        ConfigCommand::Show => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            output.line(text.trim_end());
        }
    }
    Ok(())
}

fn service_command(
    command: ServiceCommand,
    root_config_path: Option<&Path>,
    runtime: &mut impl CliRuntime,
    output: &mut Output,
) -> Result<()> {
    match command {
        ServiceCommand::Install {
            command_config,
            host,
            port,
            start,
            enable,
        } => {
            let path = runtime.install_service(ServiceInstallRequest {
                config_path: command_config.or_else(|| root_config_path.map(Path::to_path_buf)),
                host,
                port,
                start,
                enable,
            })?;
            output.line(&format!("installed service: {}", path.display()));
        }
        ServiceCommand::Start => {
            runtime.start_service()?;
            output.line("started service");
        }
        ServiceCommand::Stop => {
            runtime.stop_service()?;
            output.line("stopped service");
        }
        ServiceCommand::Restart => {
            runtime.restart_service()?;
            output.line("restarted service");
        }
        ServiceCommand::Status => output.line(&runtime.service_status()?),
        ServiceCommand::Uninstall => {
            let path = runtime.uninstall_service()?;
            output.line(&format!("uninstalled service: {}", path.display()));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn login(
    config_path: Option<&Path>,
    provider: &str,
    manual: bool,
    key: Option<&str>,
    home: &Path,
    cwd: &Path,
    runtime: &mut impl CliRuntime,
    output: &mut Output,
) -> Result<()> {
    let config = load_config(config_path, Some(home), cwd)?;
    let provider = provider.parse::<ProviderId>().map_err(anyhow::Error::msg)?;
    if provider == ProviderId::OpenCodeGo && manual {
        bail!("--manual is not supported for opencode-go (it uses a static API key, not OAuth)");
    }
    let email = runtime.login(&config, provider, manual, key)?;
    output.line(&format!("saved {provider} account token for {email}"));
    Ok(())
}

fn help_text(topic: &[String]) -> Result<String> {
    let mut command = Args::command();
    for item in topic {
        let Some(next) = command
            .get_subcommands()
            .find(|subcommand| subcommand.get_name() == item)
            .cloned()
        else {
            bail!("unknown help topic: {}", topic.join(" "));
        };
        command = next;
    }
    let bin_name = if topic.is_empty() {
        "pengepul".to_string()
    } else {
        format!("pengepul {}", topic.join(" "))
    };
    command = command.bin_name(bin_name);
    let text = command.render_help().to_string();
    if topic.is_empty() {
        Ok(text)
    } else if let Some(index) = text.find("Usage:") {
        Ok(text[index..].to_string())
    } else {
        Ok(text)
    }
}

fn base_url(config: &Config) -> String {
    let mut host = if config.host.is_empty() || matches!(config.host.as_str(), "0.0.0.0" | "::") {
        "127.0.0.1".to_string()
    } else {
        config.host.clone()
    };
    if host.contains(':') && !host.starts_with('[') {
        host = format!("[{host}]");
    }
    format!("http://{host}:{}", config.port)
}

fn first_api_key(config: &Config) -> Result<String> {
    config
        .api_keys
        .iter()
        .min()
        .cloned()
        .context("config has no API keys")
}

fn print_account_counts(payload: &Value, output: &mut Output) {
    for (provider_id, provider) in providers(payload) {
        let count = provider
            .get("account_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let suffix = if count == 1 { "account" } else { "accounts" };
        output.line(&format!("{provider_id}: {count} {suffix}"));
    }
}

fn print_accounts(payload: &Value, output: &mut Output) {
    for (provider_id, provider) in providers(payload) {
        let count = provider
            .get("account_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let suffix = if count == 1 { "account" } else { "accounts" };
        output.line(&format!("{provider_id}: {count} {suffix}"));
        let Some(accounts) = provider.get("accounts").and_then(Value::as_array) else {
            continue;
        };
        for account in accounts {
            let email = account
                .get("email")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let state = if account
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "available"
            } else {
                "unavailable"
            };
            let failures = account
                .get("failureCount")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let mut line = format!("  {email} {state} failures={failures}");
            if let Some(plan_type) = account.get("planType").and_then(Value::as_str) {
                write!(line, " plan={plan_type}").expect("write to String cannot fail");
            }
            output.line(&line);
        }
    }
}

fn providers(payload: &Value) -> Vec<(&str, &Value)> {
    let Some(providers) = payload.get("providers").and_then(Value::as_object) else {
        return Vec::new();
    };
    providers
        .iter()
        .map(|(provider_id, provider)| (provider_id.as_str(), provider))
        .collect()
}

#[derive(Default)]
struct Output {
    stdout: String,
    stderr: String,
}

impl Output {
    fn line(&mut self, value: &str) {
        self.stdout.push_str(value);
        self.stdout.push('\n');
    }
}
