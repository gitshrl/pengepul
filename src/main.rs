use std::io::{self, Write as _};
use std::path::PathBuf;

use anyhow::{Context, Result};
use pengepul::cli::run_with_env;
use pengepul::runtime::RealRuntime;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let owned_arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let cli_arguments = owned_arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let home = home_dir()?;
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let mut runtime = RealRuntime::new()?;
    let outcome = run_with_env(&cli_arguments, &home, &cwd, &mut runtime)?;

    io::stdout()
        .write_all(outcome.stdout.as_bytes())
        .context("failed to write stdout")?;
    io::stderr()
        .write_all(outcome.stderr.as_bytes())
        .context("failed to write stderr")?;
    if outcome.code != 0 {
        std::process::exit(outcome.code);
    }
    Ok(())
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}
