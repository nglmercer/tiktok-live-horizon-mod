# Migrating from the Paper plugin

The rule, function and script formats are unchanged: copy `events/`, `functions/` and `scripts/` from `plugins/TikTokMinecraft/` into `config/tiktok_live/` and run `/tiktok reload`.

## What moves

| Paper plugin | Horizon mod |
|---|---|
| `plugins/TikTokMinecraft/config.yml` | `config/tiktok_live/config.yml` (same sections) |
| `plugins/TikTokMinecraft/{events,functions,scripts}/` | `config/tiktok_live/{events,functions,scripts}/` |
| `plugins/TikTokMinecraft/cache/` | `horizon-mod-data/tiktok_live/` (signing bundle) |
| GraalJS signer | Embedded Rust signer (no Java, no downloads except the bundle) |
| Bukkit permissions | `/tiktok` requires operator level 4 |

## What differs

* **Game API.** Actions use Horizon's server API. Block-changing actions (`block-cage`, `tnt-cage`, `void-shaft`, `chunk-replace`, `place-cobweb`) keep exact restoration and restore everything when the server stops.
* **Commands.** `command` actions and function commands run through the server's command dispatcher under the same policy (`functions.allowed-commands`, `security.allow-console-commands`). Relative coordinates (`~ ~ ~`) are wrapped in `execute at <player> run`.
* **Manifest side.** `side = "both"` instead of server-only.
* **Defaults are conservative.** Live actions are off until enabled.
* **Event age.** Events older than 60 s when they are drained (for example after a server pause) are dropped instead of replayed.
* **`fetch-gifts`.** Downloads the room's gift catalog once per connection and fills `gift.name`, `gift.diamonds` and `gift.streakable` when an event lacks them. A failed download is reported in `/tiktok status` and does not stop the connection.

## Not ported

Anything that depended on Paper/Bukkit internals (third-party claim plugins, Folia) has no equivalent.
