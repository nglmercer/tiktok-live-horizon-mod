//! `cargo xtask test [filter]`: runs the mod's unit tests in a temporary copy.
//!
//! Cargo rewrites `mod/Cargo.lock` while it resolves the SDK, and that lock is managed by
//! `cargo xtask lock`, not by Cargo, so the tests run on a copy of the package.
use std::process::{Command, ExitCode};

use crate::util::{copy_dir, root, Result};

pub fn run(args: &[String]) -> Result<ExitCode> {
    let root = root();
    let work = std::path::PathBuf::from(crate::util::env_or("TMPDIR", "/tmp")).join("tiktok-live-mod-test");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    for file in ["Cargo.toml", "Cargo.lock", "horizon-mod.toml"] {
        std::fs::copy(root.join("mod").join(file), work.join(file)).map_err(|e| format!("cannot copy {file}: {e}"))?;
    }
    copy_dir(&root.join("mod/src"), &work.join("src"))?;
    std::os::unix::fs::symlink(root.join("mod/.horizon"), work.join(".horizon")).map_err(|e| format!("cannot link .horizon: {e}"))?;

    let output = Command::new("cargo")
        .args(["test", "--lib"])
        .args(args)
        .current_dir(&work)
        .output()
        .map_err(|e| format!("cannot start cargo: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&output.stderr), String::from_utf8_lossy(&output.stdout));
    // Keep the test results and the errors; drop the compiler's warning noise.
    let kept: Vec<&str> = text.lines().filter(|line| keep(line)).collect();
    for line in &kept[kept.len().saturating_sub(60)..] {
        println!("{line}");
    }
    Ok(if output.status.success() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn keep(line: &str) -> bool {
    let t = line.trim_start();
    !(t.is_empty()
        || line.starts_with("warning")
        || t.starts_with('|')
        || t.starts_with("-->")
        || t.starts_with('=')
        || t.starts_with("...")
        || t.split('|').next().is_some_and(|n| !n.is_empty() && n.trim().chars().all(|c| c.is_ascii_digit()) && t.contains('|')))
}

#[cfg(test)]
mod tests {
    use super::keep;

    #[test]
    fn noise_is_dropped_and_results_are_kept() {
        assert!(!keep("warning: unused variable"));
        assert!(!keep("   --> src/lib.rs:1:1"));
        assert!(!keep("12 |     let x = 1;"));
        assert!(keep("test result: ok. 1 passed"));
        assert!(keep("error: could not compile"));
    }
}
