use std::process::Command;

#[test]
fn binary_help_runs_cli() {
    let output = Command::new(env!("CARGO_BIN_EXE_pengepul"))
        .arg("help")
        .output()
        .expect("run binary");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    assert!(stdout.contains("Usage: pengepul"));
    assert!(!stdout.contains("not fully wired"));
}
