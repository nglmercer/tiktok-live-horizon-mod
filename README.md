# TikTok LIVE — native mod for Horizon

A server mod that connects one or more TikTok LIVE creators and turns their events (chat, gifts, likes, follows, shares, memberships…) into in-game actions. It is the Horizon port of the Paper plugin [`commad-tiktok-mod`](https://github.com/nglmercer/commad-tiktok-mod): the same YAML rules, the same JSON/`.mcfunction` functions, the same `.ttl` DSL and the same preset.

Status: the plugin's features are fully migrated, plus a command-line administration surface for agents. Verified on a real server connected to a real TikTok LIVE. Actions that act on a connected player (title, sound, items, effects, spawned mobs) are covered by tests but still need a live check with a client.

## What it includes

* A direct connection to TikTok LIVE (embedded signing, no external services), multi-account, reconnection with back-off, and a gift catalog (`fetch-gifts`).
* TikTok chat mirrored into the game chat, with rate limits and sanitizing.
* YAML rules (`config/tiktok_live/events/*.yml`), `.ttl` scripts and JSON/`.mcfunction` functions, with limits, cooldowns, thresholds and hot reload.
* 21 actions: messages, titles, sounds, particles, entities, items, effects, lightning, TNT, controlled commands, cages, shafts, chunk replacement, cobwebs… with block restoration on expiry and when the server stops.
* The `/tiktok` command (operators only): `help status reload validate describe config rules debug chat creator test connect disconnect reconnect functions jobs account preset`.
* Built so that an agent (or a person) can operate it with commands alone: `/tiktok describe` lists everything a rule can contain, `/tiktok config get|keys|set` reads and changes `config.yml` validating before saving, `/tiktok rules add <id> <trigger> <action> key=value…` creates a rule in one line, and `/tiktok validate` checks everything without applying anything. The JSON Schemas for rules and functions are installed in `config/tiktok_live/schemas/`.
* Safe defaults: live actions ship **off** (`security.execute-live-actions: false`) until you turn them on or run `/tiktok preset install`.

## Documentation

| Document | Contents |
|---|---|
| [AGENTS](docs/AGENTS.md) | Guide for agents and operators: configure, validate and create rules with commands, no file editing (also installed at `config/tiktok_live/AGENTS.md`) |
| [INSTALLATION](docs/INSTALLATION.md) | Install and first start |
| [CONFIGURATION](docs/CONFIGURATION.md) | The whole `config.yml` |
| [EVENTS](docs/EVENTS.md) | YAML rules, events and actions |
| [FUNCTIONS](docs/FUNCTIONS.md) | JSON and `.mcfunction` functions |
| [DSL](docs/DSL.md) | `.ttl` scripts |
| [PRESET](docs/PRESET.md) | The interaction preset |
| [SECURITY](docs/SECURITY.md) | Security model and limits |
| [MIGRATION-from-paper](docs/MIGRATION-from-paper.md) | What changes with respect to the Paper plugin |

The rules, functions, DSL, preset and security documents are inherited from the Paper plugin; where behavior differs, `MIGRATION-from-paper.md` wins.

## Build and test

Requires `cargo-horizon-mod` and `horizon-launcher` from the [`arismoko/horizon`](https://github.com/arismoko/horizon) fork, pinned to commit `db7638b3`.

```sh
cd mod
cargo horizon-mod build            # -> mod/target/horizon-mod/tiktok_live.hmod
```

## Development tools

All the tools are a single Rust crate in `tools/`; run them from anywhere in the repository with `cargo xtask <command>`.

| Command | What it does |
|---|---|
| `cargo xtask test [filter]` | Runs `cargo test --lib` on a temporary copy (Cargo would otherwise rewrite `mod/Cargo.lock`) |
| `cargo xtask deploy [server-dir]` | Builds a server that contains the mod, points the server at it and restarts it |
| `cargo xtask cmd "<command>"` | Sends a command to the development server's console |
| `cargo xtask lock` / `cargo xtask lockdiff <engine-lock> <mod-lock>` | Regenerates `Cargo.lock` on top of the game's lock / lists conflicts with it (0 are required) |
| `cargo xtask vendor-signer <checkout> <proto-out>` | Vendors the TikTok signer crates again |
| `cargo xtask embed-assets` | Regenerates `mod/src/defaults.rs` from `tools/defaults/` |

CI is in `.github/workflows/ci.yml`: it builds the package and runs the tests with an SDK regenerated from scratch.

## Requirements and limitations

* It builds against the [`arismoko/horizon`](https://github.com/arismoko/horizon) fork's mod API at the pinned commit; it is not expected to build against other Horizon versions without a rebuild of the SDK.
* TikTok has no official LIVE API. The connection uses the same web protocol and signing bundle as the website, so a change on TikTok's side can break it until the pinned bundle hash (`signing.bundle-sha256`) is updated.
* Clients that join a server running native mods need the same native build, and the Horizon client in the pinned snapshot can panic at startup in a race fixed by [arismoko/horizon#4815](https://github.com/arismoko/horizon/pull/4815) (a patch is in `tools/patches/`).
* The default creator in `config.yml` is the one the original plugin shipped with; change it with `/tiktok creator <name>`.

## License

Own code: MIT (`mod/LICENSE`). The Protobuf schemas in `ttl-live-proto` are AGPL-3.0-only with additional permissions; see `mod/NOTICE-ttl-live-proto`.
