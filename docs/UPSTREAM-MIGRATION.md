# Migrating off the pinned fork to upstream Horizon

Status (2026-11-17): **blocked on upstream tooling, not on the mod**. The mod is finished and
works; what keeps it on `arismoko/horizon` is the SDK release pipeline, not the API.

This document is the single place to answer three questions that kept coming up:

1. Is the native-mod API in the official repo? **Yes.**
2. Can we build the mod against an official release today? **No** (see *The blocker*).
3. What exactly has to change when that becomes possible? *The TODO* below.

## What is already upstream

The fork's native-mod changes were merged into `horizon-rust/horizon`. Verified against the
nightly source tarball `horizon-source.tar.gz` (commit `9f42064c11efe7ef3e5ed2baad7ddc2a3b36eea0`):

| Piece | Present upstream? | Where |
|---|---|---|
| Native mod API generator | yes | `crates/horizon-mod-api-gen/` |
| Native hooks | yes | `native-hooks.json`, `crates/horizon-client/src/game/native_hooks.rs` |
| SDK-aware build tool | yes | `crates/cargo-horizon-mod/` |
| Launcher support | yes | `native-build`, `native-lock`, Mods page in launcher 0.2.70 |
| `sdk-<commit>` releases with signed SDKs | **no** | only `arismoko/horizon` publishes these |

So "delete the fork" is a tooling problem, not a code problem.

## The blocker

Compiling a native mod needs the SDK for the **exact** Horizon commit it targets. There are two
ways to get one, and both fail for upstream releases today:

1. **Download a signed SDK release.** `cargo horizon-mod update --horizon <commit>` looks for
   `sdk-<commit>` in the tool's own repo. Upstream has **no** `sdk-*` releases/tags at all
   (checked via the GitHub API: 0 of the last 100 releases, 0 matching tags). The fork publishes
   `sdk-db7638b30c…`, `sdk-bade262…`, etc. — that is the only reason this mod builds today.

   ```
   error: game build <upstream-commit> has no signed SDK (release sdk-<upstream-commit>): ... 404
   ```

2. **Ask the launcher to build it.** `native-build --version <nightly>` refuses for the same
   reason:

   ```
   error: The mod tools for nightly-2026-11-13-9f42064 aren't ready yet. Pick another version.
   ```

The upstream tree on `main` has no `sdk-publish.yml` workflow either (the fork does), so nobody is
currently producing those artifacts for upstream commits.

### Upstream work that unblocks this (checked 2026-11-17)

| PR | State | What it does |
|---|---|---|
| [horizon-rust/horizon#16941](https://github.com/horizon-rust/horizon/pull/16941) | **merged 2026-11-16** | "The mod API" — brought `arismoko/horizon`'s mod API upstream. This is why the API is no longer fork-only. |
| [horizon-rust/horizon#17145](https://github.com/horizon-rust/horizon/pull/17145) | closed | Follow-up: "The mod API: #16941 merged with main". |
| [horizon-rust/horizon#17401](https://github.com/horizon-rust/horizon/pull/17401) | **open**, CI mostly green | "Ship the exact-build SDK with signed Nightly metadata and shared wasm runtime" — restores `sdk-publish.yml` and the exact-build SDK producers (head `7156948`, fixes #17391). |

So the code needed for a full migration **exists upstream in an open PR**; what is missing today
is that it has not merged/published yet. Migration stays gated until that PR lands and an
`sdk-<commit>` release exists for an upstream commit. When it does,
`cargo xtask upstream-check <commit>` will report `READY`.

## TODO — migrate when an upstream SDK exists

Do these in order. Each step has a command so this is mechanical, not guesswork.

- [ ] **1. Pick the target.** Record its release tag, commit and mixins commit. Check availability
      first: `cargo xtask upstream-check <commit>` (exit 0 = SDK published, exit 1 = still blocked).
- [ ] **2. Repoint the pins.** Set `horizon`/`mixins` in `mod/horizon-mod.toml` and `HORIZON_REV`
      in `.github/workflows/ci.yml` to that commit. `game = "26.3"` only changes with Minecraft.
- [ ] **3. Regenerate the SDK.** `cd mod && cargo horizon-mod update --horizon <commit>`
      (use a `cargo-horizon-mod` built from the same repo/commit). This rewrites
      `mod/.horizon/sdk/`; commit the result.
- [ ] **4. Fix API drift.** Run `cargo xtask test`. Any compile error is a real API difference
      between `db7638b` and the new commit; fix it in the mod, not by weakening the SDK.
- [ ] **5. Check the lock.** `cargo xtask lock` then `cargo xtask lockdiff <engine.lock> mod/Cargo.lock`
      (0 conflicts required).
- [ ] **6. Build both sides from the same source** (see the runbook). Client and server must have
      the identical mod hash or they will not negotiate payloads.
- [ ] **7. Deploy and verify** with the runbook. Only then update `README.md` and
      `docs/INSTALLATION.md` to drop the fork requirement.
- [ ] **8. Remove the fork** from CI, docs, and any local tooling once a release has been verified.

Until step 1 is possible, keep building against `db7638b`. Do **not** "just try the newest nightly":
it cannot load the mod and the failure looks like a network bug (see the runbook).

## Verification runbook — run this, do not infer

These are the exact checks used on 2026-11-17. Copy them; a green result here is the only proof
that client and server match.

### A. Code and package

```sh
cargo xtask test                 # 69 passed, 0 failed, 2 ignored (live TikTok tests)
cd mod && cargo horizon-mod build
```

### B. Build client and server from the same source

```sh
FL=~/.local/horizon-tools/bin/horizon-launcher    # or the official AppImage's native-build
MOD=/path/to/tiktok-live-horizon-mod/mod

$FL native-build "$MOD" --version <build> --trust-native-code                  # client
$FL native-build "$MOD" --server-output /tmp/out --version <build> --trust-native-code  # server

python3 -c "import json;print(json.load(open('<client>/native-mods.json'))['mods'])"
python3 -c "import json;print(json.load(open('/tmp/out/native-mods.json'))['mods'])"
```

**The two `mods` arrays must be byte-for-byte identical** (same `sha256`). On 2026-11-17 both
were `tiktok_live 0.1.0 sha256:5de15cc99e158bec9a67a906157c382a088926789cc47cfbb6cfde7d0097ee0c`.

### C. Server side

```sh
horizon-server --dir <server-dir> status
horizon-server --dir <server-dir> cmd "tiktok status"
```

Expect: `TPS 20`, `/tiktok status` lists rules and functions, and `Live Actions` reflects config.
The `engine.path` in `server.toml` must point at the **mod build** (`.../horizon-engine` with
`libtiktok_live_mod.so` beside it), and `engine.resources` at a prepared `26.3` folder that
contains `fluid_state_definitions` in `block-behaviours.json` (the upstream nightly game files
do not; a fork-snapshot export does).

If the engine folder lives under a launcher data directory that has `run-leases`, the game exits
78 with `cannot acquire launcher cache leases`. Keep the exported engine in a neutral folder
(e.g. `~/Descargas/horizon-modded`) or launch it through the launcher.

### D. Client side — the check that actually caught the mismatch

Install the freshly built client as a version in the launcher and select it for the instance,
then join the server. In the **client** log (`~/.local/share/horizon/logs/latest.log`) expect:

```
Connecting to <host>, 25565
... logged in / joined the game
```

In the **server** log expect `logged in with entity id …` and `joined the game`.

Failure signatures and their meaning:

| Message | Meaning | Fix |
|---|---|---|
| `Horizon raw protocol: missing response on horizon:payloads` | Client and server are **different native builds** (one lacks the mod). | Rebuild both from one source (step B); check hashes. |
| `Horizon raw protocol version mismatch` | Different `RAW_PROTOCOL`. | Same fix; also re-prepare game files. |
| `incompatible payload … Schema` | Same mod version, different API schema. | Regenerate the SDK for the same commit on both. |
| `Error al iniciar sesión: Status: 429` | Microsoft auth rate limit after repeated launches. | Wait a minute, retry. Not a mod problem. |

### E. Final state last verified

- launcher 0.2.70, instance version `native-db7638b-tiktok`, base `snapshot-2026-11-15-db7638b`
- server `~/.local/share/horizon-launcher/servers/server`, engine
  `~/Descargas/horizon-modded/horizon-engine`, port 25565
- `/tiktok status`: 39 rules, 3 functions, creator `@brushvasquez`
- client connected as `memelser`, no payload errors
