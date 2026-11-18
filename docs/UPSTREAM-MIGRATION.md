# Migration to upstream Horizon — completed 2026-11-18

The mod no longer depends on the `arismoko/horizon` fork. It builds against the published
upstream SDK at commit `0b14f8acfd97fa2c750e42795249dcc981178ca4` (Nightly 2026-11-18, game 26.3).

## What happened

| Date | Event |
|---|---|
| 2026-11-16 | [`horizon-rust/horizon#16941`](https://github.com/horizon-rust/horizon/pull/16941) merged the fork's mod API upstream. |
| 2026-11-17 | [`#17318`](https://github.com/horizon-rust/horizon/pull/17318) merged the fix for the client startup race (`world resources joined before entry`). |
| 2026-11-18 | Upstream published `sdk-0b14f8ac…` and Nightly `0b14f8a`; SDK publication is now available. |

Before publication the mod could only be built locally through a developer install with a locally
generated SDK. Once the SDK was published, the migration was mechanical: no source code of the mod
changed, only the pins and the generated files.

## What changed in this repo

1. `mod/horizon-mod.toml`: `horizon = "0b14f8acfd97fa2c750e42795249dcc981178ca4"`. The mixins
   revision is unchanged (`e59da909…`, still the one in `sdk.json`).
2. `.github/workflows/ci.yml`: `HORIZON_REV` points at the same commit and installs
   `cargo-horizon-mod` from `horizon-rust/horizon` instead of the fork.
3. `mod/.horizon/sdk/`: regenerated with `cargo horizon-mod build` using the published SDK; not
   committed (CI regenerates it).
4. `mod/Cargo.lock`: regenerated from the SDK's engine lock; `cargo xtask lockdiff` reports 0 conflicts.
5. `README.md`, `docs/INSTALLATION.md`: upstream requirements; the startup-race entry now points at
   upstream #17318.

No `.rs` file needed a change: `cargo check` against upstream's API and `cargo xtask test` (69 passed)
were green before any edit.

## Verification runbook — run this before declaring a future version usable

This is the exact procedure used for the migration; keep it for the next SDK bump.

### 1. Gate

```sh
cargo xtask upstream-check <commit>
```

`READY` (exit 0) means an upstream `sdk-<commit>` release exists. `BLOCKED` (exit 1) means keep the
current pin.

### 2. Repoint and regenerate

```sh
# pins: mod/horizon-mod.toml (horizon, mixins) and .github/workflows/ci.yml (HORIZON_REV)
cd mod && cargo horizon-mod update --horizon <commit> && cargo horizon-mod build
cargo xtask lock <sdk>/engine/Cargo.lock
cargo xtask lockdiff <sdk>/engine/Cargo.lock mod/Cargo.lock   # must be 0 conflicts
cargo xtask test                                              # 69 passed, 0 failed
```

### 3. Build both sides from the same source

```sh
L=.../horizon-launcher        # 0.2.72 or newer
$L native-build <repo>/mod --version <version> --trust-native-code
$L native-build <repo>/mod --server-output /tmp/out --version <version> --trust-native-code
```

The two `native-mods.json` must show **the same mod `sha256`** (on 2026-11-18: `c88aaa99…`).
A mismatch means different builds and the client will fail with
`Horizon raw protocol: missing response on horizon:payloads`.

### 4. Prepare game files and deploy

Install and prepare the version (`horizon-launcher install nightly`, `prepare --version <version>`).
Launcher 0.2.72+ refreshes resources missing `fluid_state_definitions` (#18046); on older launchers
run `horizon-prepare` by hand. Point the server's `server.toml` at the exported `horizon-engine` and
at that version's prepared `26.3` resources, then start it.

### 5. Connect and confirm

- Client log: `world resources joined`, `Connecting to …`, `logged in` — no panic, no payload error.
- Server: `list` shows the player; `tiktok status` lists rules/functions.
- `Error al iniciar sesión: Status: 429` is a transient Microsoft auth throttle, not a mod fault.

## Failure signatures (kept for the next version bump)

| Message | Meaning |
|---|---|
| `missing response on horizon:payloads` | Client and server native builds differ (mod hash mismatch). |
| `ProtocolVersion` / `incompatible payload … Schema` | Different game SDK or API schema. |
| `fluid_state_definitions Missing` | Stale resources; prepares with a newer launcher/exporters. |
| `world resources joined before entry` | Pre-#17318 client; relaunch or use the pinned SDK. |
| `mod tools … aren't ready yet` | No published SDK for that commit; migration gate not open. |
