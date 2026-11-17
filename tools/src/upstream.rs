//! `cargo xtask upstream-check [commit]`: is an SDK published for this Horizon build?
//!
//! The mod is pinned to a fork commit because the fork publishes `sdk-<commit>` releases with
//! signed SDKs. The native-mod API is already upstream; the missing piece is that artifact.
//! This command checks GitHub for it, so migration is a yes/no question instead of a guessing
//! game. See docs/UPSTREAM-MIGRATION.md for the full procedure.
use std::process::Command;

use crate::util::{read, root, Result};

const UPSTREAM: &str = "horizon-rust/horizon";
const FORK: &str = "arismoko/horizon";

pub fn run(args: &[String]) -> Result<std::process::ExitCode> {
    let commit = match args.first() {
        Some(c) => c.to_owned(),
        None => pinned_commit()?,
    };
    println!("Target commit: {commit}");
    println!("Local SDK:     {}", local_sdk_commit().unwrap_or_else(|| "(none)".to_owned()));
    println!();

    let upstream = sdk_status(UPSTREAM, &commit);
    let fork = sdk_status(FORK, &commit);
    println!("  {UPSTREAM}: {upstream}");
    println!("  {FORK}: {fork}");
    println!();

    if upstream == "available" {
        println!("READY: an upstream SDK exists for this commit.");
        println!("Next: docs/UPSTREAM-MIGRATION.md, steps 2 to 8.");
        Ok(std::process::ExitCode::SUCCESS)
    } else {
        println!("BLOCKED: upstream publishes no sdk-<commit> release for {commit}.");
        println!("Keep building against the pinned fork. The API is upstream; the SDK release is not.");
        println!("Re-run this command when upstream starts publishing SDKs.");
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

fn sdk_status(repo: &str, commit: &str) -> String {
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/sdk-{commit}");
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
