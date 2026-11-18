# Horizon SDK migration — completed 2026-11-18

History: this change was prepared on branch `migrate/upstream-ready` (PR #1).

The mod builds against the published upstream SDK at commit
`0b14f8acfd97fa2c750e42795249dcc981178ca4` (Nightly 2026-11-18, game 26.3).

## Milestones

| Date | Event |
|---|---|
| 2026-11-16 | [`horizon-rust/horizon#16941`](https://github.com/horizon-rust/horizon/pull/16941) merged the mod API upstream. |
| 2026-11-17 | [`#17318`](https://github.com/horizon-rust/horizon/pull/17318) fixed the client startup race (`world resources joined before entry`). |
| 2026-11-18 | `sdk-0b14f8ac…` and Nightly `0b14f8a` were published. |

No `.rs` file changed during the migration: `cargo check` against upstream's API and all tests
passed before any edit. Only pins and generated files moved.

## What changed in this repo

1. `mod/horizon-mod.toml`: `horizon = "0b14f8acfd97fa2c750e42795249dcc981178ca4"`; mixins
   `e59da909…` (unchanged, the value in `sdk.json`).
2. `.github/workflows/ci.yml`: matching `HORIZON_REV`; `cargo-horizon-mod` from `horizon-rust/horizon`.
3. `mod/.horizon/sdk/`: regenerated with `cargo horizon-mod build` (ignored; CI regenerates it).
4. `mod/Cargo.lock`: regenerated from the SDK engine lock; `cargo xtask lockdiff` reports 0 conflicts.
5. `README.md`, `docs/INSTALLATION.md`: upstream requirements.

## Runbook for the next SDK bump

### 1. Gate

```sh
cargo xtask upstream-check <commit>
```

`READY` (exit 0) means `sdk-<commit>` exists. Otherwise keep the current pin.

### 2. Repoint and regenerate

```sh
# pins: mod/horizon-mod.toml (horizon, mixins) and .github/workflows/ci.yml (HORIZON_REV)
cd mod && cargo horizon-mod update --horizon <commit> && cargo horizon-mod build
cargo xtask lock <sdk>/engine/Cargo.lock
cargo xtask lockdiff <sdk>/engine/Cargo.lock mod/Cargo.lock   # 0 conflicts
cargo xtask test                                              # all green
```

### 3. Build both sides from one source

```sh
L=.../horizon-launcher        # 0.2.72 or newer
$L native-build <repo>/mod --version <version> --trust-native-code
$L native-build <repo>/mod --server-output /tmp/out --version <version> --trust-native-code
```

Both `native-mods.json` must show the **same mod `sha256`** (`c88aaa99…` on 2026-11-18). A mismatch
means the client will fail with `Horizon raw protocol: missing response on horizon:payloads`.

### 4. Prepare and deploy

`horizon-launcher install nightly`, then `prepare --version <version>`. Launcher 0.2.72+ refreshes
resources missing `fluid_state_definitions`. Point `server.toml` at the exported `horizon-engine`
and that version's prepared `26.3` resources, then start it.

### 5. Confirm

Client log: `world resources joined`, `Connecting to …`, no panic/payload error. Server: `list`
shows the player and `tiktok status` lists rules/functions. `Status: 429` is a transient Microsoft
auth throttle, not a mod fault.

## Failure signatures

| Message | Meaning |
|---|---|
| `missing response on horizon:payloads` | Client and server builds differ (mod hash mismatch). |
| `ProtocolVersion` / `incompatible payload … Schema` | Different SDK or API schema. |
| `fluid_state_definitions Missing` | Stale resources; re-prepare with the launcher. |
| `world resources joined before entry` | Pre-#17318 client; relaunch or move to the pinned version. |
| `mod tools … aren't ready yet` | No SDK for that commit; the gate is not open. |
