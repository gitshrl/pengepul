use std::fs;
use std::path::Path;
use std::process::ExitStatus;

use pengepul::service::{
    LAUNCHD_LABEL, SYSTEMD_UNIT_NAME, ServiceOptions, install_launchd_service,
    install_systemd_service, render_launchd_plist, render_systemd_unit,
};
use tempfile::tempdir;

#[cfg(unix)]
fn success_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;

    ExitStatus::from_raw(0)
}

#[test]
fn render_systemd_unit_uses_pengepul_serve_with_custom_host_port() {
    let unit = render_systemd_unit(&ServiceOptions {
        executable: "/home/dev/.local/bin/pengepul".into(),
        config_path: Some("/home/dev/.pengepul/config.yaml".into()),
        host: Some("127.0.0.1".to_string()),
        port: Some(8318),
    });

    assert!(unit.contains("Description=pengepul API relay"));
    assert!(unit.contains(
        "ExecStart=/home/dev/.local/bin/pengepul serve --config /home/dev/.pengepul/config.yaml --host 127.0.0.1 --port 8318"
    ));
    assert!(unit.contains("Restart=on-failure"));
}

#[test]
fn render_launchd_plist_uses_program_arguments() {
    let payload = render_launchd_plist(
        &ServiceOptions {
            executable: "/Users/dev/.local/bin/pengepul".into(),
            config_path: Some("/Users/dev/.pengepul/config.yaml".into()),
            host: Some("127.0.0.1".to_string()),
            port: Some(8318),
        },
        Path::new("/Users/dev/.pengepul/logs/service.log"),
        Path::new("/Users/dev/.pengepul/logs/service.err.log"),
    )
    .expect("render plist");

    let plist = plist::Value::from_reader_xml(payload.as_bytes()).expect("parse plist");
    let dict = plist.as_dictionary().expect("plist dictionary");
    assert_eq!(dict["Label"].as_string(), Some(LAUNCHD_LABEL));
    assert_eq!(
        dict["ProgramArguments"].as_array().unwrap(),
        &[
            plist::Value::String("/Users/dev/.local/bin/pengepul".to_string()),
            plist::Value::String("serve".to_string()),
            plist::Value::String("--config".to_string()),
            plist::Value::String("/Users/dev/.pengepul/config.yaml".to_string()),
            plist::Value::String("--host".to_string()),
            plist::Value::String("127.0.0.1".to_string()),
            plist::Value::String("--port".to_string()),
            plist::Value::String("8318".to_string()),
        ]
    );
    assert_eq!(dict["RunAtLoad"].as_boolean(), Some(true));
    assert_eq!(dict["KeepAlive"].as_boolean(), Some(true));
}

#[test]
fn install_systemd_service_writes_unit_and_runs_commands() {
    let tmp = tempdir().expect("tempdir");
    let mut commands = Vec::<Vec<String>>::new();

    let path = install_systemd_service(
        &ServiceOptions {
            executable: "/home/dev/.local/bin/pengepul".into(),
            config_path: None,
            host: Some("127.0.0.1".to_string()),
            port: Some(8318),
        },
        tmp.path(),
        true,
        true,
        |command| {
            commands.push(command.to_vec());
            Ok(success_status())
        },
    )
    .expect("install systemd");

    assert_eq!(
        path,
        tmp.path()
            .join(".config/systemd/user")
            .join(SYSTEMD_UNIT_NAME)
    );
    assert!(
        fs::read_to_string(path)
            .unwrap()
            .contains("ExecStart=/home/dev/.local/bin/pengepul serve --host 127.0.0.1 --port 8318")
    );
    assert_eq!(
        commands,
        [
            ["systemctl", "--user", "daemon-reload"]
                .map(String::from)
                .to_vec(),
            ["systemctl", "--user", "enable", SYSTEMD_UNIT_NAME]
                .map(String::from)
                .to_vec(),
            ["systemctl", "--user", "start", SYSTEMD_UNIT_NAME]
                .map(String::from)
                .to_vec(),
        ]
    );
}

#[test]
fn install_launchd_service_writes_plist_and_bootstraps_when_started() {
    let tmp = tempdir().expect("tempdir");
    let mut commands = Vec::<Vec<String>>::new();

    let path = install_launchd_service(
        &ServiceOptions {
            executable: "/Users/dev/.local/bin/pengepul".into(),
            config_path: None,
            host: None,
            port: None,
        },
        tmp.path(),
        501,
        true,
        |command| {
            commands.push(command.to_vec());
            Ok(success_status())
        },
    )
    .expect("install launchd");

    assert_eq!(
        path,
        tmp.path()
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist"))
    );
    let plist = plist::Value::from_file(&path).expect("parse plist");
    let dict = plist.as_dictionary().expect("plist dictionary");
    assert_eq!(
        dict["ProgramArguments"].as_array().unwrap(),
        &[
            plist::Value::String("/Users/dev/.local/bin/pengepul".to_string()),
            plist::Value::String("serve".to_string()),
        ]
    );
    assert_eq!(
        commands,
        [
            vec![
                "launchctl".to_string(),
                "bootout".to_string(),
                "gui/501/dev.gitshrl.pengepul".to_string(),
            ],
            vec![
                "launchctl".to_string(),
                "bootstrap".to_string(),
                "gui/501".to_string(),
                path.to_string_lossy().into_owned(),
            ],
            vec![
                "launchctl".to_string(),
                "kickstart".to_string(),
                "gui/501/dev.gitshrl.pengepul".to_string(),
            ],
        ]
    );
}

#[test]
fn install_launchd_service_bootstraps_even_without_start() {
    // launchd has no daemon-reload: without a bootstrap here, every later
    // kickstart/bootout/print reports "Could not find service".
    let tmp = tempdir().expect("tempdir");
    let mut commands = Vec::<Vec<String>>::new();

    install_launchd_service(
        &ServiceOptions {
            executable: "/Users/dev/.local/bin/pengepul".into(),
            config_path: None,
            host: None,
            port: None,
        },
        tmp.path(),
        501,
        false,
        |command| {
            commands.push(command.to_vec());
            Ok(success_status())
        },
    )
    .expect("install launchd");

    assert!(
        commands
            .iter()
            .any(|c| c.get(1).map(String::as_str) == Some("bootstrap")),
        "the service must be loaded even when not started: {commands:?}"
    );
    assert!(
        commands
            .iter()
            .all(|c| c.get(1).map(String::as_str) != Some("kickstart")),
        "without --start nothing should be kickstarted: {commands:?}"
    );
}

#[test]
fn systemd_exec_start_quotes_paths_containing_spaces() {
    let unit = render_systemd_unit(&ServiceOptions {
        executable: Path::new("/home/dev/My Apps/pengepul").to_path_buf(),
        config_path: Some(Path::new("/home/dev/My Apps/config.yaml").to_path_buf()),
        host: Some("127.0.0.1".to_string()),
        port: Some(8318),
    });

    // systemd splits on whitespace, so a spaced path must arrive as one argument.
    assert!(
        unit.contains("ExecStart=\"/home/dev/My Apps/pengepul\" serve --config \"/home/dev/My Apps/config.yaml\" --host 127.0.0.1 --port 8318"),
        "spaced paths must be quoted, unspaced args left bare:\n{unit}"
    );
}
