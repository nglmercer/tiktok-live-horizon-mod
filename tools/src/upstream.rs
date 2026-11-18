//! `cargo xtask upstream-check [commit]`: is an SDK published for this Horizon build?
//!
//! Native mods compile against the SDK published for an exact Horizon commit. This command asks
//! GitHub whether `sdk-<commit>` exists, so an SDK bump is a yes/no question instead of guesswork.
//! See docs/UPSTREAM-MIGRATION.md for the full procedure.
use std::process::Command;

use crate::util::{read, root, Result};

const REPO: &str = "horizon-rust/horizon";

pub fn run(args: &[String]) -> Result<std::process::ExitCode> {
    let commit = match args.first() {
        Some(c) => c.to_owned(),
        None => pinned_commit()?,
    };
    println!("Target commit: {commit}");
    println!("Local SDK:     {}", local_sdk_commit().unwrap_or_else(|| "(none)".to_owned()));
    println!();
    let status = sdk_status(&commit);
    println!("  {REPO}: {status}");
    println!();
    if status == "available" {
        println!("READY: an SDK exists for this commit.");
        println!("Next: docs/UPSTREAM-MIGRATION.md (pins, SDK, lock, server build, runbook).");
        Ok(std::process::ExitCode::SUCCESS)
    } else {
        println!("BLOCKED: no sdk-<commit> release for {commit}.");
        println!("Keep the current pin; re-run this command when upstream publishes one.");
        Ok(std::process::ExitCode::from(1))
    }
}

fn pinned_commit() -> Result<String> {
    let manifest = read(&root().join("mod/horizon-mod.toml"))?;
    manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("horizon = \""))
        .and_then(|v| v.split('"').next())
        .map(str::to_owned)
        .ok_or_else(|| "no horizon = ... pin in mod/horizon-mod.toml".to_owned())
}

fn local_sdk_commit() -> Option<String> {
    let sdk = read(&root().join("mod/.horizon/sdk/sdk.json")).ok()?;
    let marker = "\"commit\": \"";
    let rest = sdk.split(marker).nth(1)?;
    rest.split('"').next().map(str::to_owned)
}

fn sdk_status(commit: &str) -> String {
    let url = format!("https://api.github.com/repos/{REPO}/releases/tags/sdk-{commit}");
    let output = Command::new("curl")
        .args(["-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "15", &url])
        .output();
    match output {
        Ok(o) if o.status.success() => match String::from_utf8_lossy(&o.stdout).trim() {
            "200" => "available".to_owned(),
            "404" => "not published".to_owned(),
            code => format!("HTTP {code}"),
        },
        Ok(o) => format!("curl failed ({})", o.status),
        Err(e) => format!("curl unavailable ({e})"),
    }
}
