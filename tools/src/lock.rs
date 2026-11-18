//! `cargo xtask lock` and `cargo xtask lockdiff`: the mod's `Cargo.lock`.
//!
//! The launcher rejects a lock in which a package with the SAME version as the game's has other
//! exact dependencies (`game or pinned package X moved or changed its dependency graph`). A
//! different version lives next to the game's without trouble. Starting from the game's own lock
//! makes Cargo add only what the mod needs on top.
use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, ExitCode, Stdio};

use crate::util::{copy_dir, home, read, root, write, Result};

type Package = toml::Table;

/// `cargo xtask lock [engine/Cargo.lock]`.
pub fn run(args: &[String]) -> Result<ExitCode> {
    let root = root();
    let module = root.join("mod");
    let engine_lock = match args.first() {
        Some(path) => std::path::PathBuf::from(path),
        None => game_lock()?,
    };
    let work = std::env::temp_dir().join(format!("tiktok-lock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    // An inert copy of the mod: without the SDK (the launcher provides it) and an empty lib.rs.
    let manifest: String = read(&module.join("Cargo.toml"))?
        .lines()
        .filter(|line| !line.starts_with("horizon-mod-api"))
        .map(|line| format!("{line}\n"))
        .collect();
    write(&work.join("Cargo.toml"), &manifest)?;
    copy_dir(&module.join("src/vendor"), &work.join("src/vendor"))?;
    write(&work.join("src/lib.rs"), "")?;
    std::fs::copy(&engine_lock, work.join("Cargo.lock")).map_err(|e| format!("cannot copy {}: {e}", engine_lock.display()))?;

    crate::util::exec(Command::new("cargo").args(["metadata", "--format-version", "1"]).current_dir(&work).stdout(Stdio::null()))?;
    let lock = read(&work.join("Cargo.lock"))?;
    write(&module.join("Cargo.lock"), &lock)?;
    let _ = std::fs::remove_dir_all(&work);
    println!("Cargo.lock written: {} packages", lock.lines().filter(|l| l.starts_with("name")).count());

    diff(&engine_lock.to_string_lossy(), &module.join("Cargo.lock").to_string_lossy(), false)?;
    Ok(ExitCode::SUCCESS)
}

/// The lock of the game's prebuilt server engine, wherever the launcher unpacked it.
fn game_lock() -> Result<std::path::PathBuf> {
    let prebuilt = home()?.join(".local/share/horizon-launcher/native/prebuilt");
    let mut candidates: Vec<_> = std::fs::read_dir(&prebuilt)
        .map_err(|e| format!("cannot read {}: {e}", prebuilt.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with("-server")))
        .map(|p| p.join("engine/Cargo.lock"))
        .filter(|p| p.exists())
        .collect();
    candidates.sort();
    candidates.into_iter().next().ok_or_else(|| format!("no engine Cargo.lock under {}; pass it as an argument", prebuilt.display()))
}

/// `cargo xtask lockdiff <engine-lock> <mod-lock> [--all]`.
pub fn run_diff(args: &[String]) -> Result<ExitCode> {
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let [game, mine] = paths.as_slice() else {
        return Err("usage: cargo xtask lockdiff <engine-lock> <mod-lock> [--all]".to_owned());
    };
    let conflicts = diff(game, mine, args.iter().any(|a| a == "--all"))?;
    Ok(if conflicts == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn packages(path: &str) -> Result<BTreeMap<String, Vec<Package>>> {
    let table: toml::Table = read(std::path::Path::new(path))?.parse().map_err(|e| format!("{path}: {e}"))?;
    let mut by_name: BTreeMap<String, Vec<Package>> = BTreeMap::new();
    for package in table.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
        if let Some(package) = package.as_table() {
            let name = package.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_owned();
            by_name.entry(name).or_default().push(package.clone());
        }
    }
    Ok(by_name)
}

fn version(package: &Package) -> &str {
    package.get("version").and_then(|v| v.as_str()).unwrap_or_default()
}

/// `name` or `name version` entries to `(name, version)`, completing the version when it is unique.
fn expand(index: &BTreeMap<String, Vec<Package>>, package: &Package) -> BTreeSet<(String, String)> {
    package
        .get("dependencies")
        .and_then(|d| d.as_array())
        .into_iter()
        .flatten()
        .filter_map(|d| d.as_str())
        .map(|dependency| {
            let mut parts = dependency.split(' ');
            let name = parts.next().unwrap_or_default().to_owned();
            let version = match parts.next() {
                Some(version) => version.to_owned(),
                None => match index.get(&name).map(Vec::as_slice) {
                    Some([only]) => version(only).to_owned(),
                    _ => "?".to_owned(),
                },
            };
            (name, version)
        })
        .collect()
}

/// Prints the conflicts and returns how many there are.
pub fn diff(game_path: &str, mine_path: &str, show_all: bool) -> Result<usize> {
    let (game, mine) = (packages(game_path)?, packages(mine_path)?);
    let mut conflicts = 0;
    for (name, list) in &mine {
        if name.starts_with("horizon") || name == "tiktok_live_mod" {
            continue;
        }
        for package in list {
            let Some(same) = game.get(name).and_then(|g| g.iter().find(|q| version(q) == version(package))) else {
                if show_all {
                    println!("own   {name} {}", version(package));
                }
                continue;
            };
            let (mine_deps, game_deps) = (expand(&mine, package), expand(&game, same));
            if mine_deps != game_deps {
                conflicts += 1;
                println!("CONFLICT {name} {}", version(package));
                println!("   only mine: {:?}", mine_deps.difference(&game_deps).collect::<Vec<_>>());
                println!("   only game: {:?}", game_deps.difference(&mine_deps).collect::<Vec<_>>());
            }
        }
    }
    println!("{conflicts} conflict(s)");
    Ok(conflicts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(text: &str) -> BTreeMap<String, Vec<Package>> {
        let table: toml::Table = text.parse().unwrap();
        let mut out: BTreeMap<String, Vec<Package>> = BTreeMap::new();
        for p in table["package"].as_array().unwrap() {
            let p = p.as_table().unwrap().clone();
            out.entry(p["name"].as_str().unwrap().to_owned()).or_default().push(p);
        }
        out
    }

    #[test]
    fn versions_are_completed_only_when_unique() {
        let index = lock(
            "[[package]]\nname = \"a\"\nversion = \"1\"\ndependencies = [\"b\", \"c 2\"]\n[[package]]\nname = \"b\"\nversion = \"3\"\n[[package]]\nname = \"c\"\nversion = \"2\"\n[[package]]\nname = \"c\"\nversion = \"4\"\n",
        );
        let deps = expand(&index, &index["a"][0]);
        assert!(deps.contains(&("b".to_owned(), "3".to_owned())));
        assert!(deps.contains(&("c".to_owned(), "2".to_owned())));
    }
}
