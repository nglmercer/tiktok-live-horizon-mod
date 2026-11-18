//! Development tools for the TikTok LIVE Horizon mod.
//!
//! Run them from anywhere in the repository with `cargo xtask <command>`:
//!
//! | Command | What it does |
//! |---|---|
//! | `test [filter]` | Runs the mod's unit tests in a temporary copy (Cargo would rewrite `mod/Cargo.lock`) |
//! | `deploy [server-dir]` | Builds a dedicated server that contains the mod, points the server at it and restarts it |
//! | `cmd <command...>` | Sends a command to the development server's console |
//! | `lock [engine-lock]` | Regenerates `mod/Cargo.lock` on top of the game's lock and reports conflicts |
//! | `lockdiff <engine-lock> <mod-lock> [--all]` | Lists the packages whose dependency graph conflicts with the game's |
//! | `vendor-signer <signer-checkout> <proto-out-dir>` | Vendors the TikTok signer crates into `mod/src/vendor/` |
//! | `embed-assets` | Regenerates `mod/src/defaults.rs` from `tools/defaults/` |
//! | `upstream-check [commit]` | Is an upstream SDK published for this build? (migration gate) |

mod cmd;
mod deploy;
mod embed;
mod lock;
mod testrun;
mod upstream;
mod util;
mod vendor;

use std::process::ExitCode;

const USAGE: &str = "Usage: cargo xtask <command> [args]

Commands:
  test [filter]                                   run the mod's unit tests in a temporary copy
  deploy [server-dir]                             build the mod into a dedicated server and restart it
  cmd <command...>                                send a command to the development server's console
  lock [engine-lock]                              regenerate mod/Cargo.lock on top of the game's lock
  lockdiff <engine-lock> <mod-lock> [--all]       list dependency-graph conflicts with the game's lock
  vendor-signer <signer-checkout> <proto-out-dir> vendor the TikTok signer crates into mod/src/vendor
  embed-assets                                    regenerate mod/src/defaults.rs from tools/defaults
  upstream-check [commit]                         is an upstream SDK published for this build? (migration gate)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let result = match command.as_str() {
        "test" => testrun::run(rest),
        "deploy" => deploy::run(rest),
        "cmd" => cmd::run(rest),
        "lock" => lock::run(rest),
        "lockdiff" => lock::run_diff(rest),
        "vendor-signer" => vendor::run(rest),
        "embed-assets" => embed::run(),
        "upstream-check" => upstream::run(rest),
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
