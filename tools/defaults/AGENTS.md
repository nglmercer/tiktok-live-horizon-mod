# TikTok LIVE mod — operating guide for agents

This file is installed (and refreshed on every start) next to the configuration so that an
agent, or a person, can run the mod without reading its source. Everything below is verified
by the mod itself: bad input is rejected with a message and the previous working configuration
stays active.

## Where things are

| What | Where |
|---|---|
| Main configuration | `config/tiktok_live/config.yml` |
| Rules (YAML) | `config/tiktok_live/events/*.yml` (`custom.yml` is managed by `/tiktok rules add`) |
| Rules (script) | `config/tiktok_live/scripts/*.ttl` |
| Functions | `config/tiktok_live/functions/**/*.json` and `*.mcfunction` |
| JSON Schemas | `config/tiktok_live/schemas/event-rule.schema.json`, `function.schema.json` |
| Signing bundle cache | `horizon-mod-data/tiktok_live/webmssdk.js` (downloaded and SHA-256 verified) |

Files are hot reloaded: save and wait about a second, or run `/tiktok reload`.

## How to run a command

In game (operator level 4) or from the server console: `/tiktok <command>`. From a shell:

```sh
<export>/horizon-server --dir <server dir> cmd "tiktok status"
```

Every command prints plain text, one fact per line.

## The loop

1. **Look**: `/tiktok status` (connection state, events received, rules matched, live actions on/off).
2. **Learn what is possible**: `/tiktok describe` (see below).
3. **Change**: edit a file, or use `/tiktok config set` / `/tiktok rules add`.
4. **Check**: `/tiktok validate` checks everything without applying it.
5. **Apply**: `/tiktok reload` (the commands that change things already apply and report the result).
6. **Try without TikTok**: `/tiktok test gift Rose 3`, `/tiktok test like 150`, `/tiktok test chat hi`,
   `/tiktok test follow`, `/tiktok test share`.
7. **Confirm**: `/tiktok status` again; `/tiktok debug on` logs every event and rule match.

## Commands

| Command | Does |
|---|---|
| `status` | Pipeline state and counters. |
| `reload` / `validate` | Apply / check config, rules and functions. |
| `describe [triggers\|fields\|comparisons\|targets\|actions\|action <type>]` | The vocabulary of rules, with ranges and defaults. |
| `config get [section[.key]]` | Read the current value (falls back to the default if the key is absent). |
| `config keys [section]` | Every key with its default and what it does. |
| `config set <section.key> <value>` | Change one key; typed by the key's default (`true/false/on/off`, numbers, comma separated lists). The whole file is validated first. |
| `creator [name]` | Show or change the main creator. |
| `account list\|add <user> [player]\|remove <user>\|target <user> <player\|clear>` | Several creators at once, each bound to a player. |
| `connect\|disconnect\|reconnect [account\|all]` | Control connections. |
| `rules list\|show <id>\|enable <id>\|disable <id>\|remove <id>` | Inspect and toggle rules (`remove` only for rules in `custom.yml`). |
| `rules add <id> <trigger> <action> [key=value ...]` | Create a rule, validated before it is saved (see below). |
| `functions list\|preview <id>\|run <id>`, `jobs list\|cancel <id>` | Reusable function sequences. |
| `preset install` | Back up and install the interaction preset and turn live actions on. |
| `chat on\|off`, `debug on\|off` | Chat mirror, diagnostics. |

## Creating a rule in one line

```text
/tiktok rules add thanks gift.final broadcast message="Thanks {viewer} for {gift.name}!"
/tiktok rules add rose-zombie gift.final spawn-entity entity=minecraft:zombie count=2 match.gift-name=Rose cooldown=10s:user
/tiktok rules add likes-chicken like spawn-entity entity=minecraft:chicken every=50
/tiktok rules add hi chat broadcast message=Hello match.text:equals-ignore-case=!hi
```

`key=value` pairs, values with spaces in double quotes:

- `match.<field>[:<comparison>]=<value>` — `match.diamonds:greater-or-equal=100` (default comparison `equals`).
- `cooldown=<duration>[:<scope>]` — `10s`, `500ms`, `2m`; scope `user` (default), `rule`, `global`.
- `every=<n>` — fire every n likes (only with the `like` trigger).
- `enabled=false`, `priority=<n>`, `stop-processing=true` (later rules are skipped once this one matched).
- everything else is a parameter of the action: `/tiktok describe action <type>` lists them with ranges.
- `target=` chooses who is affected: `nearest-online` (default), `all-online`, `random-online`, `specific-player:<name>`.

Anything richer (several actions, `conditions` trees, per-rule `stop-processing`) is written in a
YAML file under `events/` — see `docs/EVENTS.md` and the schema.

## Safety switches (read before turning things on)

The shipped defaults are conservative so nothing surprising happens on a fresh server:

| Key | Default | Meaning |
|---|---|---|
| `security.execute-live-actions` | `false` | Master switch. While `false`, events are received, shown in chat and matched, but no action changes the world. |
| `security.allow-console-commands` / `security.allow-raw-commands` | `false` | Needed for the `command` action. |
| `actions.max-entities-per-event`, `actions.max-entities-per-minute` | 5 / 60 | Entity budgets. |
| `actions.allowed-entities` | `[]` | Entities the rules may spawn (`"*"` for all). |
| `functions.allowed-commands` | a short list | Command roots a function may run. |

Turn live actions on with `/tiktok config set security.execute-live-actions true` after testing
with `/tiktok test ...`. Viewer text can never become a command.

## Reading the chat

With `chat.announce-status: true` the in-game chat shows connection changes
(`@name is connected`, `offline: the creator is not live`, errors) and, once, a notice when rules
match while live actions are off. `chat.show-gifts` and `chat.show-social` add finished gifts and
follows/shares. Set any of them to `false` with `/tiktok config set`.

## When something does not work

| Symptom | Do |
|---|---|
| Nothing happens on gifts | `/tiktok status`: *Live Actions* disabled? `/tiktok config set security.execute-live-actions true`. |
| `offline` | The creator is not live; the mod retries every minute. |
| Reload failed | `/tiktok validate` lists each problem with file, rule and line; fix and `/tiktok reload`. |
| Rule never fires | `/tiktok rules show <id>`, then `/tiktok debug on` and `/tiktok test ...` to see the match. |
| A key is unknown | `/tiktok config keys <section>`. |
