# Installation

## Requirements

| Item | Requirement |
|---|---|
| Game | Horizon fork `arismoko/horizon`, SDK commit `db7638b3` (game 26.3) |
| Network | Outbound HTTPS/WSS to TikTok. The mod opens no listening port. |
| Players | Clients that join need the same mod build (`side = "both"`); see below. |

## Install

1. Build the package (`cargo horizon-mod build`) or use `cargo xtask deploy` to compile a server that already contains the mod.
2. Start the server. The mod creates `config/tiktok_live/` with `config.yml`, `events/`, `functions/` and `scripts/` and logs that the defaults were installed.
3. Set the creator in `config/tiktok_live/config.yml`:

   ```yaml
   tiktok:
     unique-id: "creator_name"
   ```

   or run `/tiktok creator creator_name` from an operator account or the console.
4. Check `/tiktok status`. When the creator is live, the connection state changes to `connected`.

The mod downloads the TikTok signing bundle on the first connection, verifies its SHA-256 and caches it in `horizon-mod-data/tiktok_live/webmssdk.js`; later starts reuse the cache.

## Clients

The manifest declares `side = "both"` because the fork rejects `side = "server"`. The server hooks do nothing on a client, but a client joining a native-code server must run the same native build (`native-db7638b-…`) with the mod, and a Microsoft session if `online_mode` is on.

## Safe defaults

Live actions ship **disabled**:

```yaml
security:
  execute-live-actions: false
  allow-console-commands: false
```

Use `/tiktok test ...` to exercise the rules with synthetic events, then set `security.execute-live-actions: true` (or run `/tiktok preset install`, which enables it, turns events on and installs the interaction preset).

## Uninstall

Stop the server, remove the mod from the build, and delete `config/tiktok_live/` and `horizon-mod-data/tiktok_live/`.

## Troubleshooting

| Symptom | Check |
|---|---|
| `/tiktok status` says `offline` | The creator is not live; the mod retries every minute. |
| `signing bundle` errors | The server cannot reach TikTok's CDN, or the bundle hash changed (see `signing.bundle-sha256`). |
| Rules do not fire | `security.execute-live-actions` is `false`, or `/tiktok debug on` shows why a rule was skipped. |
| Hot reload ignored | `/tiktok status` shows the last reload error; the previous rules stay active. |
| Client panics at start with `world resources joined before entry` | Race in the fork's client (not in this mod): the renderers resolve before the background world-resource loader finishes. Launch again; if it repeats, start the client first and the server after. Fix for the fork: `tools/patches/fork-client-wait-for-world-rest.patch` (compiles on `db7638b`; needs a game build from a patched checkout to take effect). |
