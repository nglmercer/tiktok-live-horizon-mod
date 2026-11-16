//! `cargo xtask cmd <command...>`: sends a command to the development server's console.
use std::process::{Command, ExitCode};
use std::time::Duration;

use crate::util::{default_servers_dir, engine_dir, env_or, run_with_timeout, Result};

pub fn run(args: &[String]) -> Result<ExitCode> {
    if args.is_empty() {
        return Err("usage: cargo xtask cmd \"tiktok status\"".to_owned());
    }
    let server_dir = std::path::PathBuf::from(env_or("SERVER_DIR", default_servers_dir()?.join("server").to_string_lossy()));
    let engine = engine_dir(&server_dir).ok_or_else(|| format!("{}/server.toml has no engine path", server_dir.display()))?;
    let code = run_with_timeout(
        Command::new(format!("{engine}/horizon-server")).arg("--dir").arg(&server_dir).arg("cmd").args(args),
        Duration::from_secs(30),
    )?;
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}
