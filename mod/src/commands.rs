//! The `/tiktok` subcommands. Each one returns the reply lines.
use mod_api::{ConfigPath, ServerHost};

use crate::config::{normalize_unique_id, Config};
use crate::event::{self, Event};
use crate::hub::{now_ms, read_text, Hub, CONFIG_PATH};
use crate::value::Val;

/// Every command usage the mod registers (all require level 4).
pub const USAGES: &[&str] = &[
    "tiktok help",
    "tiktok status",
    "tiktok reload",
    "tiktok rules list",
    "tiktok rules enable <id:word>",
    "tiktok rules disable <id:word>",
    "tiktok rules show <id:word>",
    "tiktok rules remove <id:word>",
    "tiktok rules add <id:word> <trigger:word> <action:word>",
    "tiktok rules add <id:word> <trigger:word> <action:word> <params:text>",
    "tiktok validate",
    "tiktok describe",
    "tiktok describe <what:word>",
    "tiktok describe <what:word> <name:word>",
    "tiktok config get",
    "tiktok config get <path:word>",
    "tiktok config keys",
    "tiktok config keys <section:word>",
    "tiktok config set <path:word> <value:text>",
    "tiktok debug <mode:word>",
    "tiktok chat <mode:word>",
    "tiktok creator",
    "tiktok creator <name:word>",
    "tiktok test chat <message:text>",
    "tiktok test gift",
    "tiktok test gift <name:word>",
    "tiktok test gift <name:word> <second:word>",
    "tiktok test gift <name:word> <second:word> <third:word>",
    "tiktok test like",
    "tiktok test like <count:word>",
    "tiktok test blocks <action:word> <x:integer> <y:integer> <z:integer>",
    "tiktok test follow",
    "tiktok test share",
    "tiktok account list",
    "tiktok account add <username:word>",
    "tiktok account add <username:word> <player:word>",
    "tiktok account remove <username:word>",
    "tiktok account target <username:word> <player:word>",
    "tiktok preset install",
    "tiktok connect",
    "tiktok connect <account:word>",
    "tiktok disconnect",
    "tiktok disconnect <account:word>",
    "tiktok reconnect",
    "tiktok reconnect <account:word>",
    "tiktok functions list",
    "tiktok functions preview <id:text>",
    "tiktok functions run <id:text>",
    "tiktok jobs list",
    "tiktok jobs cancel <id:word>",
];

pub type Args<'a> = &'a dyn Fn(&str) -> Option<String>;

fn uptime(hub: &Hub) -> String {
    let seconds = ((now_ms() - hub.started_ms) / 1000).max(0);
    format!("{}h {}m {}s", seconds / 3600, (seconds / 60) % 60, seconds % 60)
}

pub fn run(hub: &mut Hub, host: &mut dyn ServerHost, words: &[String], arg: Args<'_>) -> Vec<String> {
    let sub = words.get(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    match sub.as_str() {
        "help" | "" => help(),
        "status" => status(hub),
        "reload" => reload(hub, host),
        "rules" if words.get(2).is_some_and(|w| w == "add") => crate::admin::rules_add(hub, host, arg),
        "rules" if words.get(2).is_some_and(|w| w == "remove") => crate::admin::rules_remove(hub, host, arg),
        "rules" if words.get(2).is_some_and(|w| w == "show") => crate::admin::rules_show(hub, arg),
        "rules" => rules(hub, words),
        "validate" => crate::admin::validate(hub, host),
        "describe" => crate::admin::describe(arg),
        "config" => match words.get(2).map(String::as_str) {
            Some("get") => crate::admin::config_get(host, arg),
            Some("keys") => crate::admin::config_keys_command(arg),
            Some("set") => crate::admin::config_set(hub, host, arg),
            _ => vec!["Usage: /tiktok config get [section[.key]] | keys [section] | set <section.key> <value>".to_owned()],
        },
        "debug" => debug(hub, arg),
        "chat" => chat(hub, arg),
        "creator" => creator(hub, host, arg),
        "test" if words.get(2).is_some_and(|w| w == "blocks") => test_blocks(hub, host, arg),
        "test" => test(hub, words, arg),
        "connect" | "disconnect" | "reconnect" => connection(hub, host, &sub, arg("account")),
        "functions" => functions(hub, words, arg),
        "jobs" => jobs(hub, words, arg),
        "preset" => preset(hub, host, words),
        "account" => account(hub, host, words, arg),
        other => vec![format!("Unknown subcommand: {other}")],
    }
}

fn connection(hub: &mut Hub, host: &mut dyn ServerHost, action: &str, target: Option<String>) -> Vec<String> {
    let accounts = hub.config.accounts.clone();
    let selected: Vec<_> = match target.as_deref() {
        None => accounts.first().cloned().into_iter().collect(),
        Some(t) if t.eq_ignore_ascii_case("all") => accounts,
        Some(t) => {
            let name = normalize_unique_id(t);
            match accounts.iter().find(|a| a.unique_id.eq_ignore_ascii_case(&name)) {
                Some(account) => vec![account.clone()],
                None => return vec![format!("Unknown account: @{name}")],
            }
        }
    };
    let mut lines = Vec::new();
    for account in selected {
        match action {
            "connect" => {
                let started = hub.connect_account(host, &account);
                lines.push(format!("@{}: {}", account.unique_id, if started { "connecting" } else { "already running (see /tiktok status for its state)" }));
            }
            "disconnect" => {
                let stopped = hub.disconnect_account(&account);
                lines.push(format!("@{}: {}", account.unique_id, if stopped { "disconnected; it will stay offline" } else { "was not connected; it will stay offline" }));
            }
            _ => {
                hub.reconnect_account(host, &account);
                lines.push(format!("@{}: reconnecting", account.unique_id));
            }
        }
    }
    lines
}

/// QA of the block actions on a dummy player at (x, y, z) of the Overworld.
fn test_blocks(hub: &mut Hub, host: &mut dyn ServerHost, arg: Args<'_>) -> Vec<String> {
    use crate::rules::model::ActionSpec;
    let kind = arg("action").unwrap_or_default().to_ascii_lowercase();
    let coord = |name: &str| arg(name).and_then(|v| v.parse::<f64>().ok());
    let (Some(x), Some(y), Some(z)) = (coord("x"), coord("y"), coord("z")) else {
        return vec!["Usage: /tiktok test blocks cage|void|chunk|cobweb <x> <y> <z>".to_owned()];
    };
    let mut spec = match kind.as_str() {
        "cage" => ActionSpec::new("block-cage"),
        "void" => ActionSpec::new("void-shaft"),
        "chunk" => ActionSpec::new("chunk-replace"),
        "cobweb" => ActionSpec::new("place-cobweb"),
        _ => return vec!["Usage: /tiktok test blocks cage|void|chunk|cobweb <x> <y> <z>".to_owned()],
    };
    let set = |spec: &mut ActionSpec, key: &str, value: Val| {
        spec.params.insert(key.to_owned(), value);
    };
    match kind.as_str() {
        "cage" => {
            set(&mut spec, "material", Val::Str("minecraft:obsidian".into()));
            set(&mut spec, "restore-seconds", Val::Int(10));
        }
        "void" => {
            set(&mut spec, "restore-seconds", Val::Int(10));
            set(&mut spec, "max-blocks", Val::Int(4096));
        }
        "chunk" => {
            set(&mut spec, "material", Val::Str("minecraft:glass".into()));
            set(&mut spec, "min-y", Val::Int(y as i64 - 2));
            set(&mut spec, "max-y", Val::Int(y as i64 + 2));
            set(&mut spec, "restore-seconds", Val::Int(10));
        }
        _ => {
            set(&mut spec, "restore-seconds", Val::Int(10));
        }
    }
    if let Err(error) = crate::actions::validate(&spec, &hub.config, None) {
        return vec![format!("Invalid test action: {error}")];
    }
    let dummy = crate::actions::Target { name: "TestDummy".into(), uuid: "test".into(), pos: [x, y, z], dim: "minecraft:overworld".into() };
    let outcome = hub.run_dummy_action(host, &spec, dummy);
    vec![format!("{kind}: {outcome}"), "Blocks are restored after 10 seconds.".to_owned()]
}

fn read_config_text(host: &mut dyn ServerHost) -> String {
    read_text(host, CONFIG_PATH).unwrap_or_else(|_| crate::config::DEFAULT_CONFIG.to_owned())
}

pub fn write_text(host: &mut dyn ServerHost, path: &str, text: &str) -> Result<(), String> {
    let path = ConfigPath::new(path).map_err(|e| format!("{e:?}"))?;
    host.config_files().write(&path, text.as_bytes()).map_err(|e| format!("{e:?}"))
}

/// Validates and saves an edited `config.yml`; the reload applies it.
fn save_config(hub: &mut Hub, host: &mut dyn ServerHost, updated: &str) -> Option<String> {
    if let Err(problems) = Config::parse(updated) {
        return Some(problems.join("; "));
    }
    if let Err(error) = write_text(host, CONFIG_PATH, updated) {
        return Some(format!("The configuration could not be saved: {error}"));
    }
    hub.request_reload();
    None
}

fn account(hub: &mut Hub, host: &mut dyn ServerHost, words: &[String], arg: Args<'_>) -> Vec<String> {
    let action = words.get(2).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    if action == "list" {
        let mut lines = vec!["Accounts:".to_owned()];
        for account in &hub.config.accounts {
            let target = if account.target_player.is_empty() { "(no assigned player)".to_owned() } else { account.target_player.clone() };
            let state = hub
                .connections
                .iter()
                .find(|c| c.handle.unique_id.eq_ignore_ascii_case(&account.unique_id))
                .map(|c| c.handle.status().state)
                .unwrap_or_else(|| "not connected".to_owned());
            lines.push(format!("  @{} -> {target} [{state}]", account.unique_id));
        }
        return lines;
    }
    const USAGE: &str = "Usage: /tiktok account list|add <username> [player]|remove <username>|target <username> <player|clear>";
    if !["add", "remove", "target"].contains(&action.as_str()) {
        return vec![USAGE.to_owned()];
    }
    let Some(username) = arg("username") else {
        return vec![USAGE.to_owned()];
    };
    let name = normalize_unique_id(&username);
    if name.is_empty() || name.len() > 24 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') {
        return vec!["Invalid TikTok username".to_owned()];
    }
    let primary = name.eq_ignore_ascii_case(&hub.config.unique_id);
    let exists = hub.config.accounts.iter().any(|a| a.unique_id.eq_ignore_ascii_case(&name));
    let player = arg("player").filter(|p| !p.eq_ignore_ascii_case("clear")).unwrap_or_default();
    let mut extra: Vec<(String, String)> =
        hub.config.accounts.iter().skip(1).map(|a| (a.unique_id.clone(), a.target_player.clone())).collect();
    let mut text = read_config_text(host);
    match action.as_str() {
        "add" => {
            if exists {
                return vec!["Account already exists".to_owned()];
            }
            extra.push((name.clone(), player));
        }
        "remove" => {
            if !exists {
                return vec!["Unknown account".to_owned()];
            }
            if primary {
                return vec!["The primary creator cannot be removed; use /tiktok creator to change it".to_owned()];
            }
            extra.retain(|(id, _)| !id.eq_ignore_ascii_case(&name));
        }
        _ => {
            if !exists {
                return vec!["Unknown account".to_owned()];
            }
            if primary {
                match crate::yamledit::set_scalar(&text, "tiktok", "target-player", &crate::yamledit::quote(&player)) {
                    Some(updated) => text = updated,
                    None => return vec!["config.yml has no tiktok.target-player line".to_owned()],
                }
            } else if let Some(entry) = extra.iter_mut().find(|(id, _)| id.eq_ignore_ascii_case(&name)) {
                entry.1 = player;
            }
        }
    }
    let Some(updated) = crate::yamledit::set_accounts(&text, &extra) else {
        return vec!["config.yml has no tiktok.accounts line".to_owned()];
    };
    match save_config(hub, host, &updated) {
        None => vec!["Account updated and saved.".to_owned()],
        Some(problem) => vec![problem],
    }
}

/// `/tiktok preset install`: backs up, installs `interactions.yml` and opens the limits that
/// the preset asks for; if validation fails, everything goes back to how it was.
fn preset(hub: &mut Hub, host: &mut dyn ServerHost, words: &[String]) -> Vec<String> {
    if words.get(2).map(|s| s.to_ascii_lowercase()).as_deref() != Some("install") {
        return vec!["Usage: /tiktok preset install".to_owned()];
    }
    const PRESET: &str = "tiktok_live/events/interactions.yml";
    let old_config = read_config_text(host);
    let old_preset = read_text(host, PRESET).ok();
    let backup = old_preset.as_ref().map(|text| {
        let name = format!("tiktok_live/events/interactions.yml.bak-{}", now_ms());
        (name, text.clone())
    });
    if let Some((name, text)) = &backup {
        if let Err(error) = write_text(host, name, text) {
            return vec![format!("Could not install preset: {error}")];
        }
    }
    let edit = |text: String| -> Option<String> {
        let text = crate::yamledit::set_scalar(&text, "security", "execute-live-actions", "true")?;
        let text = crate::yamledit::set_scalar(&text, "events", "enabled", "true")?;
        let text = crate::yamledit::set_list(&text, "actions", "allowed-entities", &["*".to_owned()])?;
        let text = crate::yamledit::set_scalar(&text, "actions", "max-entities-per-event", &hub.config.max_entities_per_event.max(64).to_string())?;
        crate::yamledit::set_scalar(&text, "actions", "max-entities-per-minute", &hub.config.max_entities_per_minute.max(240).to_string())
    };
    let Some(new_config) = edit(old_config.clone()) else {
        return vec!["Could not install preset: config.yml is missing a section the preset needs".to_owned()];
    };
    if let Err(error) = Config::parse(&new_config) {
        return vec![format!("Could not install preset: {}", error.join("; "))];
    }
    let rollback = |hub: &mut Hub, host: &mut dyn ServerHost| {
        let _ = write_text(host, CONFIG_PATH, &old_config);
        match &old_preset {
            Some(text) => {
                let _ = write_text(host, PRESET, text);
            }
            None => {
                let _ = write_text(host, PRESET, "rules: []\n");
            }
        }
        let _ = hub.reload(host);
    };
    if let Err(error) = write_text(host, PRESET, crate::defaults::INTERACTIONS).and_then(|()| write_text(host, CONFIG_PATH, &new_config)) {
        rollback(hub, host);
        return vec![format!("Could not install preset: {error}")];
    }
    match hub.reload(host) {
        Ok(count) => {
            hub.last_reload = format!("ok: {count} rule(s) loaded");
            let ids: Vec<String> = hub.engine.snapshot.rules.iter().filter(|r| r.id.starts_with("meme-")).map(|r| r.id.clone()).collect();
            for id in ids {
                hub.engine.set_enabled(&id, true);
            }
            vec![format!(
                "Enabled the latest meme preset ({count} total rules). Previous interactions.yml backup: {}",
                backup.map(|(name, _)| name.rsplit('/').next().unwrap_or("").to_owned()).unwrap_or_else(|| "(no previous file)".to_owned())
            )]
        }
        Err(problems) => {
            rollback(hub, host);
            vec![format!("Preset validation failed; previous rules restored: {}", problems.join("; "))]
        }
    }
}

fn functions(hub: &mut Hub, words: &[String], arg: Args<'_>) -> Vec<String> {
    let action = words.get(2).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    match action.as_str() {
        "list" => {
            let ids = hub.functions.ids();
            if ids.is_empty() {
                return vec!["No functions are loaded.".to_owned()];
            }
            let mut lines = vec!["Loaded functions:".to_owned()];
            lines.extend(ids.into_iter().map(|id| format!("  {id}")));
            lines
        }
        "preview" | "run" => {
            let Some(id) = arg("id") else {
                return vec![format!("Usage: /tiktok functions {action} <id>")];
            };
            let id = id.trim().to_owned();
            if action == "preview" {
                let limits = hub.fn_limits();
                let allowed = hub.config.allowed_commands.clone();
                match hub.functions.preview(&id, &std::collections::BTreeMap::new(), &limits, &allowed) {
                    Ok(steps) if steps.is_empty() => vec![format!("Preview of {id}:"), "  (the function has no steps)".to_owned()],
                    Ok(steps) => {
                        let mut lines = vec![format!("Preview of {id}:")];
                        lines.extend(steps.into_iter().map(|s| format!("  {s}")));
                        lines
                    }
                    Err(error) => vec![format!("The function could not be previewed: {error}")],
                }
            } else {
                match hub.submit_function(&id, &std::collections::BTreeMap::new(), None) {
                    Ok(_) => vec![format!("Started {id}.")],
                    Err(error) => vec![format!("The function could not be started: {error}")],
                }
            }
        }
        _ => vec!["Usage: /tiktok functions list|preview <id>|run <id>".to_owned()],
    }
}

fn jobs(hub: &mut Hub, words: &[String], arg: Args<'_>) -> Vec<String> {
    let action = words.get(2).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    match action.as_str() {
        "list" => {
            if hub.functions.jobs().is_empty() {
                return vec!["No function is running.".to_owned()];
            }
            let mut lines = vec!["Running functions:".to_owned()];
            for job in hub.functions.jobs() {
                let target = job.target.clone().unwrap_or_else(|| "the server".to_owned());
                lines.push(format!("  {} {} - {:?} on {target}", job.id, job.function_id, job.state));
            }
            lines
        }
        "cancel" => match arg("id").and_then(|id| id.trim().parse::<u64>().ok()) {
            Some(id) if hub.functions.cancel(id) => vec![format!("Cancelled job {id}.")],
            Some(id) => vec![format!("Unknown job: {id}")],
            None => vec!["Usage: /tiktok jobs list|cancel <id>".to_owned()],
        },
        _ => vec!["Usage: /tiktok jobs list|cancel <id>".to_owned()],
    }
}

fn help() -> Vec<String> {
    [
        "TikTok LIVE commands:",
        "  /tiktok status - show the pipeline state",
        "  /tiktok reload - reload the configuration and rules",
        "  /tiktok chat on|off - toggle the Minecraft chat mirror",
        "  /tiktok connect|disconnect|reconnect [account|all] - control the TikTok connections",
        "  /tiktok creator [name] - show or change the creator",
        "  /tiktok account list|add <username> [player]|remove <username>|target <username> <player|clear>",
        "  /tiktok preset install - back up and enable the shipped meme interactions",
        "  /tiktok rules list|show <id>|enable <id>|disable <id>|remove <id> - manage the rules",
        "  /tiktok rules add <id> <trigger> <action> [key=value ...] - create a rule (see describe)",
        "  /tiktok config get [section[.key]]|keys [section]|set <section.key> <value> - read and change config.yml safely",
        "  /tiktok describe [triggers|fields|comparisons|targets|actions|action <type>] - what a rule can contain",
        "  /tiktok validate - check config, rules and functions without applying them",
        "  /tiktok test chat <message> | gift <name|id> [count] [final|update] | like <count> | follow | share",
        "  /tiktok functions list|preview <id>|run <id> - manage the functions",
        "  /tiktok jobs list|cancel <id> - manage the running functions",
        "  /tiktok debug on|off - toggle diagnostic logging",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

fn status(hub: &Hub) -> Vec<String> {
    let yn = |b: bool| if b { "enabled" } else { "disabled" };
    vec![
        "TikTok LIVE".to_owned(),
        format!("  Creator: @{}", hub.config.unique_id),
        format!("  Hot Reload: {}", yn(hub.config.hot_reload_enabled)),
        format!("  Connections: {}", if hub.connections.is_empty() { "none".to_owned() } else { hub.connections.iter().map(|c| { let s = c.handle.status(); format!("@{} {}", c.handle.unique_id, s.state) }).collect::<Vec<_>>().join(", ") }),
        format!("  Last File Reload: {}", hub.last_reload),
        format!("  Uptime: {}", uptime(hub)),
        format!("  Last Event: {}", if hub.stats.last_event.is_empty() { "none" } else { &hub.stats.last_event }),
        format!("  Received Events: {}", hub.stats.received),
        format!("  Dropped Events: {}", hub.stats.dropped),
        format!("  Duplicate Events: {}", hub.stats.duplicates),
        format!("  Queued Events: {}", hub.inbox.len()),
        format!("  Matched Rules: {}", hub.stats.matched),
        format!("  Rules Loaded: {}", hub.engine.snapshot.rules.len()),
        format!("  Actions Executed: {}", hub.engine.executed),
        format!("  Actions Rejected: {}", hub.engine.rejected),
        format!("  Scheduled Tasks: {}", hub.sched.pending()),
        format!("  Block Changes Pending Restore: {} ({} task(s))", hub.blocks.pending(), hub.blocks.tasks()),
        format!("  Functions Loaded: {}", hub.functions.registry.len()),
        format!("  Active Jobs: {}", hub.functions.active()),
        format!("  Functions Completed/Failed/Rejected: {}/{}/{}", hub.functions.completed, hub.functions.failed, hub.functions.rejected),
        format!("  Entity Budget: {}/{} this minute", hub.limiter.used(), hub.limiter.limit()),
        format!("  Chat: received {}, suppressed {}, dropped {}, queued {}", hub.chat.received, hub.chat.suppressed, hub.chat.dropped, hub.chat.queued()),
        format!("  Live Actions: {}", if hub.config.execute_live_actions { "enabled" } else { "disabled (security.execute-live-actions)" }),
        format!("  Ticks: {}", hub.tick),
    ]
}

fn reload(hub: &mut Hub, host: &mut dyn ServerHost) -> Vec<String> {
    crate::admin::apply(hub, host)
}

fn rules(hub: &mut Hub, words: &[String]) -> Vec<String> {
    let action = words.get(2).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    match action.as_str() {
        "list" => {
            if hub.engine.snapshot.rules.is_empty() {
                return vec!["No rules are loaded.".to_owned()];
            }
            let mut lines = vec!["Loaded rules:".to_owned()];
            for rule in &hub.engine.snapshot.rules {
                let enabled = hub.engine.override_of(&rule.id).unwrap_or(rule.enabled);
                let note = if hub.engine.override_of(&rule.id).is_some() { " (override)" } else { "" };
                lines.push(format!("  {} [{}] - {}{note} ({})", rule.id, rule.trigger, if enabled { "enabled" } else { "disabled" }, rule.source));
            }
            lines
        }
        "enable" | "disable" => {
            let Some(id) = words.get(3) else {
                return vec![format!("Usage: /tiktok rules {action} <id>")];
            };
            let enabled = action == "enable";
            if hub.engine.set_enabled(id, enabled) {
                vec![format!("Rule {id} {}.", if enabled { "enabled" } else { "disabled" })]
            } else {
                vec![format!("Unknown rule: {id}")]
            }
        }
        _ => vec!["Usage: /tiktok rules list|enable <id>|disable <id>".to_owned()],
    }
}

fn debug(hub: &mut Hub, arg: Args<'_>) -> Vec<String> {
    match arg("mode").map(|m| m.to_ascii_lowercase()).as_deref() {
        Some("on") => {
            hub.debug = true;
            hub.config.log_events = true;
            hub.config.log_rule_matches = true;
            vec!["Diagnostic logging enabled.".to_owned()]
        }
        Some("off") => {
            hub.debug = false;
            hub.config.log_events = false;
            hub.config.log_rule_matches = false;
            vec!["Diagnostic logging disabled.".to_owned()]
        }
        _ => vec!["Usage: /tiktok debug on|off".to_owned()],
    }
}

fn chat(hub: &mut Hub, arg: Args<'_>) -> Vec<String> {
    match arg("mode").map(|m| m.to_ascii_lowercase()).as_deref() {
        Some("on") => {
            hub.config.chat_minecraft = true;
            vec!["Minecraft chat mirror enabled.".to_owned()]
        }
        Some("off") => {
            hub.config.chat_minecraft = false;
            vec!["Minecraft chat mirror disabled.".to_owned()]
        }
        _ => vec!["Usage: /tiktok chat on|off".to_owned()],
    }
}

/// Changes only the `unique-id:` line of the `tiktok:` block, keeping comments.
fn replace_unique_id(text: &str, name: &str) -> Option<String> {
    let mut in_tiktok = false;
    let mut out = Vec::new();
    let mut replaced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !line.starts_with(' ') && !line.starts_with('#') && !line.is_empty() {
            in_tiktok = line.trim_end() == "tiktok:";
        }
        if in_tiktok && !replaced && trimmed.starts_with("unique-id:") {
            let indent = &line[..line.len() - trimmed.len()];
            out.push(format!("{indent}unique-id: \"@{name}\""));
            replaced = true;
        } else {
            out.push(line.to_owned());
        }
    }
    replaced.then(|| out.join("\n") + "\n")
}

fn creator(hub: &mut Hub, host: &mut dyn ServerHost, arg: Args<'_>) -> Vec<String> {
    let Some(raw) = arg("name") else {
        return vec![format!("The current creator is @{}.", hub.config.unique_id), "Usage: /tiktok creator <username>".to_owned()];
    };
    let name = normalize_unique_id(&raw);
    if name == hub.config.unique_id {
        return vec![format!("The creator is already @{name}.")];
    }
    let text = read_text(host, CONFIG_PATH).unwrap_or_else(|_| crate::config::DEFAULT_CONFIG.to_owned());
    let Some(updated) = replace_unique_id(&text, &name) else {
        return vec!["The creator was not changed: config.yml has no tiktok.unique-id line.".to_owned()];
    };
    if let Err(problems) = Config::parse(&updated) {
        return vec![format!("The creator was not changed: {}", problems.join("; "))];
    }
    let path = match ConfigPath::new(CONFIG_PATH) {
        Ok(path) => path,
        Err(e) => return vec![format!("The creator was not changed: {e:?}")],
    };
    if let Err(e) = host.config_files().write(&path, updated.as_bytes()) {
        return vec![format!("The creator was not changed: {e:?}")];
    }
    hub.request_reload();
    vec![format!("The creator is now @{name}; connecting (watch the chat for its state).")]
}

fn gift_id_for_test(name: &str) -> String {
    let normalized: String = name.chars().filter(|c| !matches!(c, '_' | '-' | ' ')).collect::<String>().to_lowercase();
    match normalized.as_str() {
        "rose" => "5655".to_owned(),
        "confetti" => "5585".to_owned(),
        "rosa" => "8913".to_owned(),
        "fingerheart" => "5487".to_owned(),
        "cap" => "6104".to_owned(),
        "corgi" => "6267".to_owned(),
        "capybara" => "14488".to_owned(),
        "doughnut" => "5879".to_owned(),
        "fireworks" => "6090".to_owned(),
        "moneygun" => "7168".to_owned(),
        _ if name.chars().all(|c| c.is_ascii_digit()) && !name.is_empty() => name.to_owned(),
        _ => "0".to_owned(),
    }
}

fn test_diamond_price(gift_id: &str) -> i64 {
    match gift_id {
        "5655" => 1,
        "5585" => 100,
        "8913" => 10,
        "5487" => 5,
        "6104" => 99,
        "6267" => 299,
        "14488" => 30,
        "5879" => 30,
        "6090" => 1088,
        "7168" => 500,
        _ => 1,
    }
}

const TEST_USAGE: &str = "Usage: /tiktok test chat <message> | gift <name|id> [count] [final|update] | like <count> | follow | share";

fn synthetic(hub: &mut Hub, kind: &str) -> Event {
    let mut event = Event::new(kind);
    event.id = hub.next_synthetic_id();
    event.user_id = "0".to_owned();
    event.unique_id = "tester".to_owned();
    event.nickname = "Tester".to_owned();
    event.timestamp = now_ms();
    event
}

fn test(hub: &mut Hub, words: &[String], arg: Args<'_>) -> Vec<String> {
    let kind = words.get(2).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    let event = match kind.as_str() {
        "chat" => {
            let message = arg("message").unwrap_or_default();
            if message.is_empty() {
                return vec!["Usage: /tiktok test chat <message>".to_owned()];
            }
            synthetic(hub, event::CHAT)
                .with("chat.text", Val::Str(message))
                .with("chat.emotes", Val::List(Vec::new()))
                .with("chat.emoteOnly", Val::Bool(false))
        }
        "gift" => {
            let name = arg("name").unwrap_or_else(|| "Rose".to_owned());
            let (mut mode, mut repeats) = ("final".to_owned(), 1i64);
            if let Some(second) = arg("second") {
                if !second.is_empty() && second.chars().all(|c| c.is_ascii_digit()) {
                    repeats = second.parse().unwrap_or(-1);
                    if let Some(third) = arg("third") {
                        mode = third;
                    }
                } else {
                    mode = second;
                }
            }
            if !(1..=1000).contains(&repeats) {
                return vec!["Gift repeat must be 1..1000".to_owned()];
            }
            if !mode.eq_ignore_ascii_case("final") && !mode.eq_ignore_ascii_case("update") {
                return vec!["Gift mode must be final or update".to_owned()];
            }
            let trigger = if mode.eq_ignore_ascii_case("update") { event::GIFT } else { event::GIFT_FINAL };
            let gift_id = gift_id_for_test(&name);
            synthetic(hub, trigger)
                .with("gift.diamonds", Val::Int(test_diamond_price(&gift_id)))
                .with("gift.id", Val::Str(gift_id))
                .with("gift.name", Val::Str(name))
                .with("gift.repeatCount", Val::Int(repeats))
                .with("gift.comboCount", Val::Int(repeats))
                .with("gift.groupId", Val::Str("0".to_owned()))
                .with("gift.repeatEnd", Val::Bool(true))
        }
        "like" => {
            let count = match arg("count") {
                None => 1,
                Some(text) => match text.trim().parse::<i64>() {
                    Ok(n) => n,
                    Err(_) => return vec!["The like count must be a whole number.".to_owned()],
                },
            };
            synthetic(hub, event::LIKE).with("like.count", Val::Int(count)).with("like.total", Val::Int(count))
        }
        "follow" => social(hub, event::SOCIAL_FOLLOW, "follow", 1),
        "share" => social(hub, event::SOCIAL_SHARE, "share", 3),
        _ => return vec![TEST_USAGE.to_owned()],
    };
    let kind = event.kind.clone();
    hub.offer(event);
    vec![format!("Injected a synthetic {kind} event through the normal pipeline.")]
}

fn social(hub: &mut Hub, trigger: &str, kind: &str, action: i64) -> Event {
    synthetic(hub, trigger)
        .with("social.kind", Val::Str(kind.to_owned()))
        .with("social.action", Val::Int(action))
        .with("social.followCount", Val::Int(0))
        .with("social.shareCount", Val::Int(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_line_is_replaced_without_losing_comments() {
        let updated = replace_unique_id(crate::config::DEFAULT_CONFIG, "otro_creador").unwrap();
        assert!(updated.contains("unique-id: \"@otro_creador\""));
        assert!(updated.contains("# The creator's TikTok username."));
        assert_eq!(Config::parse(&updated).unwrap().unique_id, "otro_creador");
    }

    #[test]
    fn gift_ids_and_prices_match_the_plugin() {
        assert_eq!(gift_id_for_test("Finger Heart"), "5487");
        assert_eq!(test_diamond_price("5487"), 5);
        assert_eq!(gift_id_for_test("12345"), "12345");
        assert_eq!(gift_id_for_test("unknown"), "0");
    }
}
