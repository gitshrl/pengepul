use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

pub const SYSTEMD_UNIT_NAME: &str = "pengepul.service";
pub const LAUNCHD_LABEL: &str = "dev.gitshrl.pengepul";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceOptions {
    pub executable: PathBuf,
    pub config_path: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[must_use]
pub fn build_service_command(options: &ServiceOptions) -> Vec<String> {
    let mut command = vec![
        options.executable.to_string_lossy().into_owned(),
        "serve".to_string(),
    ];
    if let Some(config_path) = &options.config_path {
        command.push("--config".to_string());
        command.push(config_path.to_string_lossy().into_owned());
    }
    if let Some(host) = &options.host {
        command.push("--host".to_string());
        command.push(host.clone());
    }
    if let Some(port) = options.port {
        command.push("--port".to_string());
        command.push(port.to_string());
    }
    command
}

#[must_use]
pub fn render_systemd_unit(options: &ServiceOptions) -> String {
    // systemd splits ExecStart on whitespace, so an install path containing a
    // space would otherwise arrive as two arguments. Only such arguments are
    // quoted, leaving the common unit file unadorned.
    let exec_start = build_service_command(options)
        .iter()
        .map(|arg| {
            if arg.contains([' ', '"', '\\']) {
                format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\nDescription=pengepul API relay\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={exec_start}\nRestart=on-failure\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n"
    )
}

/// Render a launchd plist payload.
///
/// # Errors
///
/// Returns an error if plist XML serialization fails.
pub fn render_launchd_plist(
    options: &ServiceOptions,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<String> {
    let mut payload = BTreeMap::new();
    payload.insert(
        "Label".to_string(),
        plist::Value::String(LAUNCHD_LABEL.to_string()),
    );
    payload.insert(
        "ProgramArguments".to_string(),
        plist::Value::Array(
            build_service_command(options)
                .into_iter()
                .map(plist::Value::String)
                .collect(),
        ),
    );
    payload.insert("RunAtLoad".to_string(), plist::Value::Boolean(true));
    payload.insert("KeepAlive".to_string(), plist::Value::Boolean(true));
    payload.insert(
        "StandardOutPath".to_string(),
        plist::Value::String(stdout_path.to_string_lossy().into_owned()),
    );
    payload.insert(
        "StandardErrorPath".to_string(),
        plist::Value::String(stderr_path.to_string_lossy().into_owned()),
    );

    let mut buffer = Vec::new();
    plist::Value::Dictionary(payload.into_iter().collect())
        .to_writer_xml(&mut buffer)
        .context("failed to serialize launchd plist")?;
    String::from_utf8(buffer).context("plist serializer produced non-UTF-8")
}

/// Install a user systemd unit and optionally enable/start it.
///
/// # Errors
///
/// Returns an error when the unit cannot be written or a control command fails.
pub fn install_systemd_service(
    options: &ServiceOptions,
    home: &Path,
    start: bool,
    enable: bool,
    mut runner: impl FnMut(&[String]) -> Result<ExitStatus>,
) -> Result<PathBuf> {
    let path = home.join(".config/systemd/user").join(SYSTEMD_UNIT_NAME);
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    fs::write(&path, render_systemd_unit(options))
        .with_context(|| format!("failed to write {}", path.display()))?;

    run(&mut runner, &["systemctl", "--user", "daemon-reload"])?;
    if enable {
        run(
            &mut runner,
            &["systemctl", "--user", "enable", SYSTEMD_UNIT_NAME],
        )?;
    }
    if start {
        run(
            &mut runner,
            &["systemctl", "--user", "start", SYSTEMD_UNIT_NAME],
        )?;
    }
    Ok(path)
}

/// Install a launchd agent plist and optionally bootstrap it.
///
/// # Errors
///
/// Returns an error when the plist cannot be written or a launchctl command fails.
pub fn install_launchd_service(
    options: &ServiceOptions,
    home: &Path,
    uid: u32,
    start: bool,
    mut runner: impl FnMut(&[String]) -> Result<ExitStatus>,
) -> Result<PathBuf> {
    let path = home
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"));
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let logs = home.join(".pengepul/logs");
    fs::create_dir_all(&logs).with_context(|| format!("failed to create {}", logs.display()))?;
    let payload = render_launchd_plist(
        options,
        &logs.join("service.log"),
        &logs.join("service.err.log"),
    )?;
    fs::write(&path, payload).with_context(|| format!("failed to write {}", path.display()))?;

    if start {
        run(
            &mut runner,
            &[
                "launchctl",
                "bootstrap",
                &format!("gui/{uid}"),
                &path.to_string_lossy(),
            ],
        )?;
    }
    Ok(path)
}

/// Execute a service manager command.
///
/// # Errors
///
/// Returns an error if the command cannot be spawned or exits unsuccessfully.
pub fn run_command(command: &[String]) -> Result<ExitStatus> {
    let Some((program, args)) = command.split_first() else {
        bail!("empty command");
    };
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(status)
}

fn run(runner: &mut impl FnMut(&[String]) -> Result<ExitStatus>, command: &[&str]) -> Result<()> {
    let command = command.iter().map(ToString::to_string).collect::<Vec<_>>();
    let status = runner(&command)?;
    if !status.success() {
        bail!("{} exited with {}", command.join(" "), status);
    }
    Ok(())
}
