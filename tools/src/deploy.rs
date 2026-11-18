//! `cargo xtask deploy [server-dir]`: builds the mod into a dedicated server, points the server
//! at the new engine and restarts it.
//!
//! Environment: `LAUNCHER` (the launcher, default `~/.local/horizon-tools/bin/horizon-launcher`),
//! `VERSION` (the game version with a published SDK), `EXPORTS` (where exports live) and
//! `KEEP` (how many old exports to keep, default 2).
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use crate::util::{default_servers_dir, engine_dir, env_or, home, root, exec, timestamp, Result};

pub fn run(args: &[String]) -> Result<ExitCode> {
    let root = root();
    let module = root.join("mod");
    let home = home()?;
    let tools_bin = home.join(".local/horizon-tools/bin");
    let launcher = env_or("LAUNCHER", tools_bin.join("horizon-launcher").to_string_lossy());
    let version = env_or("VERSION", "snapshot-2026-11-15-db7638b");
    let servers = default_servers_dir()?;
    let server_dir = args.first().map_or_else(|| servers.join("server"), PathBuf::from);
    let exports = PathBuf::from(env_or("EXPORTS", servers.to_string_lossy()));
    let keep: usize = env_or("KEEP", "2").parse().map_err(|_| "KEEP must be a number".to_owned())?;
    let path = format!("{}:{}", tools_bin.display(), std::env::var("PATH").unwrap_or_default());

    println!("== checking the package");
    exec(Command::new("cargo").args(["check", "--quiet"]).current_dir(&module).env("PATH", &path))?;

    let out = exports.join(format!("tiktok-live-export-{}", timestamp()));
    println!("== building the server with the mod into {}", out.display());
    exec(Command::new(&launcher)
        .arg("native-build")
        .arg(&module)
        .arg("--server-output")
        .arg(&out)
        .args(["--version", &version, "--trust-native-code"])
        .env("PATH", &path))?;

    println!("== stopping the current server");
    if let Some(engine) = engine_dir(&server_dir) {
        let server = PathBuf::from(&engine).join("horizon-server");
        if server.exists() {
            let _ = Command::new(server).arg("--dir").arg(&server_dir).arg("stop").status();
        }
    }

    println!("== pointing server.toml at the new engine");
    let toml = server_dir.join("server.toml");
    let text = std::fs::read_to_string(&toml).map_err(|e| format!("cannot read {}: {e}", toml.display()))?;
    let engine_line = format!("path = \"{}/horizon-engine\"  # the game program (mod build)", out.display());
    let updated: Vec<String> = text
        .lines()
        .map(|line| if line.starts_with("path = \"") && line.contains("/horizon-engine\"") { engine_line.clone() } else { line.to_owned() })
        .collect();
    std::fs::write(&toml, updated.join("\n") + "\n").map_err(|e| format!("cannot write {}: {e}", toml.display()))?;
    println!("{engine_line}");

    println!("== starting");
    exec(Command::new(out.join("horizon-server")).arg("--dir").arg(&server_dir).arg("start"))?;

    println!("== removing old exports (keeping {keep})");
    let mut old: Vec<PathBuf> = std::fs::read_dir(&exports)
        .map_err(|e| format!("cannot read {}: {e}", exports.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("tiktok-live-export-")))
        .collect();
    old.sort();
    let excess = old.len().saturating_sub(keep);
    for path in old.into_iter().take(excess).filter(|p| *p != out) {
        if std::fs::remove_dir_all(&path).is_ok() {
            println!("removed {}", path.display());
        }
    }
    println!("done: {}", out.display());
    Ok(ExitCode::SUCCESS)
}
