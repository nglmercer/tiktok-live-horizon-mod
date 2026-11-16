//! Small helpers shared by the tools.
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub type Result<T> = std::result::Result<T, String>;

/// The repository root (the parent of `tools/`).
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("tools/ has a parent").to_path_buf()
}

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| "HOME is not set".to_owned())
}

/// The directory the launcher keeps its servers in.
pub fn default_servers_dir() -> Result<PathBuf> {
    Ok(home()?.join(".local/share/horizon-launcher/servers"))
}

pub fn env_or(name: &str, default: impl Into<String>) -> String {
    std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.into())
}

pub fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

pub fn write(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Runs a command to completion with inherited output and fails on a non-zero exit.
pub fn exec(command: &mut Command) -> Result<()> {
    let status = command.status().map_err(|e| format!("cannot start {:?}: {e}", command.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed ({})", command.get_program(), describe(status)))
    }
}

fn describe(status: ExitStatus) -> String {
    status.code().map_or_else(|| "killed by a signal".to_owned(), |c| format!("exit code {c}"))
}

/// Runs a command with a time limit, inheriting its output. Returns its exit code.
pub fn run_with_timeout(command: &mut Command, limit: Duration) -> Result<i32> {
    command.stdin(Stdio::null());
    let mut child = command.spawn().map_err(|e| format!("cannot start {:?}: {e}", command.get_program()))?;
    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => return Ok(status.code().unwrap_or(1)),
            None if started.elapsed() > limit => {
                let _ = child.kill();
                return Err(format!("timed out after {} s", limit.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Copies a directory tree.
pub fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|e| format!("cannot copy {}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// All files below `dir` whose name ends with `suffix`, sorted by path components.
pub fn files_below(dir: &Path, suffix: &str) -> Result<Vec<PathBuf>> {
    fn walk(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                walk(&path, suffix, out)?;
            } else if path.to_string_lossy().ends_with(suffix) {
                out.push(path);
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, suffix, &mut out)?;
    out.sort_by(|a, b| a.components().cmp(b.components()));
    Ok(out)
}

/// The engine folder a server points at (`path = ".../horizon-engine"` in `server.toml`).
pub fn engine_dir(server_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(server_dir.join("server.toml")).ok()?;
    text.lines().find_map(|line| {
        let value = line.strip_prefix("path = \"")?;
        let value = &value[..value.find('"')?];
        value.strip_suffix("/horizon-engine").map(str::to_owned)
    })
}

/// `YYYYMMDD-HHMMSS` in UTC (the standard library has no calendar).
pub fn timestamp() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}-{:02}{:02}{:02}", rest / 3600, (rest / 60) % 60, rest % 60)
}

/// A Rust raw string literal for `text` with enough `#`s.
pub fn raw_string(text: &str) -> String {
    let mut hashes = String::from("#");
    while text.contains(&format!("\"{hashes}")) {
        hashes.push('#');
    }
    format!("r{hashes}\"{text}\"{hashes}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_strings_grow_their_hashes() {
        assert_eq!(raw_string("a"), "r#\"a\"#");
        assert_eq!(raw_string("a\"#b"), "r##\"a\"#b\"##");
    }

    #[test]
    fn timestamps_have_the_expected_shape() {
        let t = timestamp();
        assert_eq!(t.len(), 15);
        assert_eq!(&t[8..9], "-");
    }
}
