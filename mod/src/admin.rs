//! Command-line administration designed for agents (and for anyone who prefers not to edit files):
//! read and change the configuration with validation, validate without applying, add and remove rules,
//! and describe everything that can be written in a rule (triggers, fields, actions).
//!
//! Everything returns stable plain text, one thing per line, so an agent can read it without guessing.
use mod_api::ServerHost;
use serde_json::{json, Value as Json};

use crate::actions::Catalog;
use crate::commands::{write_text, Args};
use crate::config::{Config, DEFAULT_CONFIG};
use crate::event::TRIGGERS;
use crate::hub::{read_text, Hub, CONFIG_PATH};
use crate::rules::loader::load_rules;
use crate::yamledit;

/// The rules file the mod manages entirely (`/tiktok rules add|remove`).
pub const CUSTOM_RULES: &str = "tiktok_live/events/custom.yml";

const CUSTOM_HEADER: &str = "# Rules created with /tiktok rules add. The mod manages this file: you may edit it by hand,\n# but comments are lost the next time /tiktok rules add|remove is used.\n";

// --- configuration --------------------------------------------------------------------------

pub struct KeyInfo {
    pub section: String,
    pub key: String,
    pub default: String,
    pub doc: String,
}

/// Every `config.yml` key with its default value and the comment that explains it, read from
/// the template the mod itself installs (so the documentation cannot drift out of date).
pub fn config_keys() -> Vec<KeyInfo> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut doc: Vec<String> = Vec::new();
    for line in DEFAULT_CONFIG.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            doc.push(t.trim_start_matches('#').trim().to_owned());
            continue;
        }
        if t.is_empty() {
            doc.clear();
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            if let Some(name) = t.strip_suffix(':') {
                section = name.to_owned();
            }
            doc.clear();
            continue;
        }
        if indent == 2 && !t.starts_with('-') {
            if let Some((key, value)) = t.split_once(':') {
                let default = value.split(" #").next().unwrap_or("").trim().to_owned();
                out.push(KeyInfo { section: section.clone(), key: key.trim().to_owned(), default, doc: doc.join(" ") });
            }
        }
        doc.clear();
    }
    out
}

fn default_value(section: &str, key: &str) -> Option<serde_norway::Value> {
    let root: serde_norway::Value = serde_norway::from_str(DEFAULT_CONFIG).ok()?;
    root.get(section)?.get(key).cloned()
}

fn show_yaml(value: &serde_norway::Value) -> String {
    use serde_norway::Value as V;
    match value {
        V::Null => "null".to_owned(),
        V::Bool(b) => b.to_string(),
        V::Number(n) => n.to_string(),
        V::String(s) => s.clone(),
        V::Sequence(items) => format!("[{}]", items.iter().map(show_yaml).collect::<Vec<_>>().join(", ")),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// `/tiktok config get [section[.key]]`.
pub fn config_get(host: &mut dyn ServerHost, arg: Args<'_>) -> Vec<String> {
    let text = read_text(host, CONFIG_PATH).unwrap_or_else(|_| DEFAULT_CONFIG.to_owned());
    let Ok(root) = serde_norway::from_str::<serde_norway::Value>(&text) else {
        return vec!["config.yml is not valid YAML; fix it or restore the defaults.".to_owned()];
    };
    let Some(path) = arg("path") else {
        let mut lines = vec!["config.yml sections (use /tiktok config get <section> or <section.key>):".to_owned()];
        if let Some(map) = root.as_mapping() {
            for key in map.keys().filter_map(|k| k.as_str()) {
                lines.push(format!("  {key}"));
            }
        }
        return lines;
    };
    match path.split_once('.') {
        None => {
            let mut lines: Vec<String> = Vec::new();
            let mut present: Vec<String> = Vec::new();
            if let Some(map) = root.get(path.as_str()).and_then(|v| v.as_mapping()) {
                for (k, v) in map {
                    if let Some(k) = k.as_str() {
                        present.push(k.to_owned());
                        lines.push(format!("{path}.{k} = {}", show_yaml(v)));
                    }
                }
            }
            for info in config_keys().into_iter().filter(|i| i.section == path && !present.contains(&i.key)) {
                let value = default_value(&info.section, &info.key).map(|v| show_yaml(&v)).unwrap_or(info.default);
                lines.push(format!("{path}.{} = {value} (default; not in config.yml)", info.key));
            }
            if lines.is_empty() {
                lines.push(format!("No such section: {path}"));
            }
            lines
        }
        Some((section, key)) => match root.get(section).and_then(|s| s.get(key)) {
            Some(value) => vec![format!("{path} = {}", show_yaml(value))],
            None => match default_value(section, key) {
                Some(value) => vec![format!("{path} = {} (default; not present in config.yml)", show_yaml(&value))],
                None => vec![format!("No such key: {path}. See /tiktok config keys.")],
            },
        },
    }
}

/// `/tiktok config keys [section]`.
pub fn config_keys_command(arg: Args<'_>) -> Vec<String> {
    let only = arg("section").map(|s| s.to_ascii_lowercase());
    let mut lines = Vec::new();
    for info in config_keys() {
        if only.as_deref().is_some_and(|s| s != info.section) {
            continue;
        }
        let default = if info.default.is_empty() { "(list)".to_owned() } else { info.default.clone() };
        let doc = if info.doc.is_empty() { String::new() } else { format!(" - {}", info.doc) };
        lines.push(format!("{}.{} = {default}{doc}", info.section, info.key));
    }
    if lines.is_empty() {
        lines.push("No such section. Sections: tiktok, connection, signing, chat, events, functions, actions, security, hot-reload, debug.".to_owned());
    }
    lines
}

/// How to write a value into `config.yml`: an already formatted scalar, or a list of items.
#[derive(Debug, PartialEq)]
pub enum Edit {
    Scalar(String),
    List(Vec<String>),
}

/// Converts the user's text to the key's type (the type of the default value).
pub fn typed_edit(default: &serde_norway::Value, raw: &str) -> Result<Edit, String> {
    use serde_norway::Value as V;
    let raw = raw.trim();
    match default {
        V::Bool(_) => match raw.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" => Ok(Edit::Scalar("true".to_owned())),
            "false" | "off" | "no" => Ok(Edit::Scalar("false".to_owned())),
            _ => Err("expected true or false".to_owned()),
        },
        V::Number(n) if n.is_i64() || n.is_u64() => {
            raw.parse::<i64>().map(|v| Edit::Scalar(v.to_string())).map_err(|_| "expected a whole number".to_owned())
        }
        V::Number(_) => raw.parse::<f64>().map(|v| Edit::Scalar(v.to_string())).map_err(|_| "expected a number".to_owned()),
        V::Sequence(_) => {
            let items: Vec<String> = raw.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
            Ok(Edit::List(items))
        }
        _ => Ok(Edit::Scalar(yamledit::quote(raw))),
    }
}

/// `/tiktok config set <section.key> <value>`: validates the whole configuration before saving,
/// applies it and returns the result.
pub fn config_set(hub: &mut Hub, host: &mut dyn ServerHost, arg: Args<'_>) -> Vec<String> {
    let (Some(path), Some(raw)) = (arg("path"), arg("value")) else {
        return vec!["Usage: /tiktok config set <section.key> <value>   (lists: comma separated)".to_owned()];
    };
    let Some((section, key)) = path.split_once('.') else {
        return vec![format!("'{path}' is not a key: use <section>.<key>. See /tiktok config keys.")];
    };
    if section == "tiktok" && key == "accounts" {
        return vec!["Use /tiktok account add|remove|target to manage the accounts.".to_owned()];
    }
    let Some(default) = default_value(section, key) else {
        return vec![format!("Unknown key: {path}. See /tiktok config keys {section}.")];
    };
    let edit = match typed_edit(&default, &raw) {
        Ok(edit) => edit,
        Err(why) => return vec![format!("Invalid value for {path}: {why}.")],
    };
    let text = read_text(host, CONFIG_PATH).unwrap_or_else(|_| DEFAULT_CONFIG.to_owned());
    let updated = match &edit {
        Edit::Scalar(value) => yamledit::upsert_scalar(&text, section, key, value),
        Edit::List(items) => yamledit::upsert_list(&text, section, key, items),
    };
    let Some(updated) = updated else {
        return vec![format!("Could not edit {path} in config.yml.")];
    };
    if let Err(problems) = Config::parse(&updated) {
        let mut lines = vec![format!("Not saved: the value makes config.yml invalid ({path} = {raw}):")];
        lines.extend(problems.into_iter().map(|p| format!("  - {p}")));
        return lines;
    }
    if let Err(error) = write_text(host, CONFIG_PATH, &updated) {
        return vec![format!("Could not save config.yml: {error}")];
    }
    let mut lines = vec![format!("{path} = {raw}")];
    lines.extend(apply(hub, host));
    lines
}

/// Applies what is on disk right now and reports the result in lines.
pub fn apply(hub: &mut Hub, host: &mut dyn ServerHost) -> Vec<String> {
    match hub.apply_reload(host) {
        Ok(count) => vec![format!("Applied: {count} rule(s) loaded.")],
        Err(problems) => {
            let mut lines = vec!["The change was saved but the reload failed; the previous configuration is still active:".to_owned()];
            lines.extend(problems.into_iter().map(|p| format!("  - {p}")));
            lines
        }
    }
}

/// `/tiktok validate`: the same as a reload, without applying anything.
pub fn validate(hub: &mut Hub, host: &mut dyn ServerHost) -> Vec<String> {
    match hub.validate(host) {
        Ok((rules, functions)) => vec![format!("Valid: config.yml, {rules} rule(s) and {functions} function(s). Nothing was applied; run /tiktok reload.")],
        Err(problems) => {
            let mut lines = vec![format!("{} problem(s); nothing was applied:", problems.len())];
            lines.extend(problems.into_iter().map(|p| format!("  - {p}")));
            lines
        }
    }
}

// --- rules ---------------------------------------------------------------------------------

/// Splits `key=value key2="value with spaces"`.
pub fn tokenize(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect();
        if i >= chars.len() || chars[i] != '=' {
            return Err(format!("expected key=value, found '{key}'"));
        }
        i += 1;
        let mut value = String::new();
        if i < chars.len() && chars[i] == '"' {
            i += 1;
            loop {
                match chars.get(i) {
                    None => return Err(format!("unterminated quote in the value of '{key}'")),
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some('\\') if i + 1 < chars.len() => {
                        value.push(chars[i + 1]);
                        i += 2;
                    }
                    Some(c) => {
                        value.push(*c);
                        i += 1;
                    }
                }
            }
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                value.push(chars[i]);
                i += 1;
            }
        }
        out.push((key, value));
    }
    Ok(out)
}

fn typed_json(raw: &str) -> Json {
    match raw {
        "true" => Json::Bool(true),
        "false" => Json::Bool(false),
        _ => raw
            .parse::<i64>()
            .map(Json::from)
            .ok()
            .or_else(|| raw.parse::<f64>().ok().filter(|f| f.is_finite()).map(Json::from))
            .unwrap_or_else(|| Json::String(raw.to_owned())),
    }
}

/// Builds a rule from `id`, `trigger`, `action` and `key=value`:
/// `match.<field>[:<comparison>]=v`, `cooldown=10s[:scope]`, `every=N` (likes threshold),
/// `enabled`, `priority`, `stop-processing`; the rest are the action's parameters.
pub fn build_rule(id: &str, trigger: &str, action: &str, params: &str) -> Result<Json, String> {
    let mut rule = serde_json::Map::new();
    rule.insert("id".into(), json!(id));
    rule.insert("trigger".into(), json!(trigger));
    let mut matchers = serde_json::Map::new();
    let mut act = serde_json::Map::new();
    act.insert("type".into(), json!(action));
    for (key, value) in tokenize(params)? {
        if let Some(field) = key.strip_prefix("match.") {
            let (field, op) = field.split_once(':').unwrap_or((field, "equals"));
            let entry = matchers.entry(field.to_owned()).or_insert_with(|| json!({}));
            entry[op] = typed_json(&value);
        } else {
            match key.as_str() {
                "enabled" | "priority" | "stop-processing" => {
                    rule.insert(key, typed_json(&value));
                }
                "cooldown" => {
                    let (duration, scope) = value.split_once(':').unwrap_or((value.as_str(), "user"));
                    rule.insert("cooldown".into(), json!({ "duration": duration, "scope": scope }));
                }
                "every" => {
                    let threshold = value.parse::<i64>().map_err(|_| "every must be a whole number".to_owned())?;
                    rule.insert("aggregate".into(), json!({ "metric": "count", "scope": "global", "threshold": threshold, "carry-remainder": true }));
                }
                _ => {
                    act.insert(key, typed_json(&value));
                }
            }
        }
    }
    if !matchers.is_empty() {
        rule.insert("match".into(), Json::Object(matchers));
    }
    rule.insert("actions".into(), json!([Json::Object(act)]));
    Ok(Json::Object(rule))
}

fn read_custom(host: &mut dyn ServerHost) -> Vec<Json> {
    let Ok(text) = read_text(host, CUSTOM_RULES) else {
        return Vec::new();
    };
    serde_norway::from_str::<Json>(&text)
        .ok()
        .and_then(|v| v.get("rules").and_then(|r| r.as_array().cloned()))
        .unwrap_or_default()
}

fn custom_text(rules: &[Json]) -> Result<String, String> {
    let body = serde_norway::to_string(&json!({ "rules": rules })).map_err(|e| e.to_string())?;
    Ok(format!("{CUSTOM_HEADER}{body}"))
}

/// `/tiktok rules add <id> <trigger> <action> [key=value ...]`.
pub fn rules_add(hub: &mut Hub, host: &mut dyn ServerHost, arg: Args<'_>) -> Vec<String> {
    let (Some(id), Some(trigger), Some(action)) = (arg("id"), arg("trigger"), arg("action")) else {
        return vec!["Usage: /tiktok rules add <id> <trigger> <action> [key=value ...]. See /tiktok describe.".to_owned()];
    };
    if hub.engine.snapshot.rules.iter().any(|r| r.id == id) {
        return vec![format!("A rule named {id} already exists. Remove it first, or pick another id.")];
    }
    let rule = match build_rule(&id, &trigger, &action, &arg("params").unwrap_or_default()) {
        Ok(rule) => rule,
        Err(why) => return vec![format!("Could not read the parameters: {why}")],
    };
    let mut rules = read_custom(host);
    rules.push(rule);
    let text = match custom_text(&rules) {
        Ok(text) => text,
        Err(why) => return vec![format!("Could not write the rule: {why}")],
    };
    let known: std::collections::HashSet<String> = hub.functions.registry.keys().cloned().collect();
    let catalog = Catalog { config: &hub.config, functions: Some(&known) };
    if let Err(problem) = load_rules(&text, "custom.yml", &catalog) {
        return vec![format!("Not saved: {problem}"), "See /tiktok describe action <type> for the parameters.".to_owned()];
    }
    if let Err(error) = write_text(host, CUSTOM_RULES, &text) {
        return vec![format!("Could not save {CUSTOM_RULES}: {error}")];
    }
    let mut lines = vec![format!("Rule {id} saved in {CUSTOM_RULES}.")];
    lines.extend(apply(hub, host));
    lines
}

/// `/tiktok rules remove <id>`: only rules of `events/custom.yml`.
pub fn rules_remove(hub: &mut Hub, host: &mut dyn ServerHost, arg: Args<'_>) -> Vec<String> {
    let Some(id) = arg("id") else {
        return vec!["Usage: /tiktok rules remove <id>".to_owned()];
    };
    let mut rules = read_custom(host);
    let before = rules.len();
    rules.retain(|r| r.get("id").and_then(|v| v.as_str()) != Some(id.as_str()));
    if rules.len() == before {
        return match hub.engine.snapshot.rules.iter().find(|r| r.id == id) {
            Some(rule) => vec![format!("Rule {id} lives in {}, not in {CUSTOM_RULES}: edit that file, or /tiktok rules disable {id}.", rule.source)],
            None => vec![format!("Unknown rule: {id}")],
        };
    }
    let text = match custom_text(&rules) {
        Ok(text) => text,
        Err(why) => return vec![format!("Could not write the rules: {why}")],
    };
    if let Err(error) = write_text(host, CUSTOM_RULES, &text) {
        return vec![format!("Could not save {CUSTOM_RULES}: {error}")];
    }
    let mut lines = vec![format!("Rule {id} removed.")];
    lines.extend(apply(hub, host));
    lines
}

/// `/tiktok rules show <id>`.
pub fn rules_show(hub: &Hub, arg: Args<'_>) -> Vec<String> {
    let Some(id) = arg("id") else {
        return vec!["Usage: /tiktok rules show <id>".to_owned()];
    };
    let Some(rule) = hub.engine.snapshot.rules.iter().find(|r| r.id == id) else {
        return vec![format!("Unknown rule: {id}")];
    };
    let enabled = hub.engine.override_of(&rule.id).unwrap_or(rule.enabled);
    let mut lines = vec![
        format!("Rule {} ({})", rule.id, rule.source),
        format!("  trigger: {}", rule.trigger),
        format!("  enabled: {enabled}   priority: {}   stop-processing: {}", rule.priority, rule.stop_processing),
    ];
    for m in &rule.matchers {
        lines.push(format!("  match: {} {} {}", m.field, m.op.wire_name(), m.value.text()));
    }
    if let Some(a) = &rule.aggregate {
        lines.push(format!("  threshold: {} (scope {:?}, carry remainder {})", a.threshold, a.scope, a.carry_remainder));
    }
    if rule.cooldown.is_some() {
        lines.push("  cooldown: yes".to_owned());
    }
    for action in &rule.actions {
        let params: Vec<String> = action.params.iter().map(|(k, v)| format!("{k}={}", v.text())).collect();
        lines.push(format!("  action: {} {}", action.kind, params.join(" ")));
    }
    lines
}

// --- description ----------------------------------------------------------------------------

const TRIGGER_DOCS: [(&str, &str); 12] = [
    ("chat", "a chat message"),
    ("chat.emote", "a reaction made only of emotes"),
    ("gift", "a gift sent or repeated (every update of a streak)"),
    ("gift.final", "a finished gift streak (one event per gift; use this for rewards)"),
    ("like", "likes sent to the room"),
    ("member", "a membership subscription"),
    ("social.follow", "a confirmed follow"),
    ("social.share", "a confirmed share"),
    ("social.other", "a social action with an unconfirmed code"),
    ("social.unknown", "a social action with no usable code"),
    ("room.user", "the viewer count changed"),
    ("unknown", "a message the decoder did not recognize"),
];

const FIELD_DOCS: [(&str, &str); 28] = [
    ("event.type", "the trigger name"),
    ("event.id", "message identifier"),
    ("event.roomId", "room the event came from"),
    ("event.timestamp", "when it was received (ms)"),
    ("event.isHistory", "history replay (true/false)"),
    ("user.id", "viewer id"),
    ("user.uniqueId", "viewer @username"),
    ("user.nickname", "viewer display name"),
    ("chat.text", "message body"),
    ("chat.emotes", "emotes in the message"),
    ("chat.emoteOnly", "the message has only emotes"),
    ("gift.id", "exact gift id"),
    ("gift.name", "gift name (localized)"),
    ("gift.diamonds", "value of one gift in diamonds"),
    ("gift.streakable", "the gift can be sent as a streak"),
    ("gift.repeatCount", "times the gift was repeated"),
    ("gift.comboCount", "combo count"),
    ("gift.groupId", "streak group"),
    ("gift.repeatEnd", "the streak ended"),
    ("like.count", "likes this event carries"),
    ("like.total", "room cumulative likes"),
    ("social.kind", "follow | share | other | unknown"),
    ("social.action", "raw action code"),
    ("social.followCount", "room cumulative follows"),
    ("social.shareCount", "room cumulative shares"),
    ("member.count", "membership count"),
    ("room.viewers", "viewers in the room"),
    ("connection.targetPlayer", "player bound to the account that received the event"),
];

const TARGET_DOCS: [(&str, &str); 4] = [
    ("nearest-online", "the online player nearest to the origin (default)"),
    ("all-online", "every online player"),
    ("random-online", "one random online player"),
    ("specific-player:<name>", "the player with that name or uuid"),
];

type ParamDoc = (&'static str, &'static str);

/// `(action, summary, parameters)`. Each parameter is `(name, type/range/default)`.
const ACTION_DOCS: &[(&str, &str, &[ParamDoc])] = &[
    ("broadcast", "show a message to every player", &[("message", "text, required, max 512; {field} placeholders")]),
    ("title", "show a title and subtitle", &[("title", "text, required; {field} placeholders"), ("subtitle", "text, max 512"), ("fade-in", "ticks 0..1200, default 10"), ("stay", "ticks 0..12000, default 60"), ("fade-out", "ticks 0..1200, default 20"), ("color", "gold, red, ... or #RRGGBB"), ("subtitle-color", "like color; defaults to color"), ("bold", "true|false, title only")]),
    ("sound", "play a sound", &[("sound", "id, required, e.g. minecraft:entity.experience_orb.pickup"), ("volume", "0..16, default 1"), ("pitch", "0..2, default 1")]),
    ("particle", "spawn particles", &[("particle", "id, required, e.g. minecraft:heart"), ("count", "1..10000, default 20")]),
    ("spawn-entity", "spawn entities next to the target", &[("entity", "id, required, e.g. minecraft:zombie"), ("count", "1..30000, default 1"), ("max-effective-count", "1..128, default 128"), ("charged", "true|false (creepers)"), ("name", "text, max 64: a name tag on each entity; {nickname}, {viewer}... placeholders"), ("name-visible", "true|false, default true"), ("multiply-by-repeat", "true|false: multiply count by the gift repeat count")]),
    ("give-item", "give items to the target", &[("material", "item id, required, not air"), ("count", "1..64, default 1")]),
    ("remove-item", "take items from the target", &[("material", "item id, required"), ("count", "1..64, default 1")]),
    ("effect", "apply a potion effect", &[("effect", "id, required, e.g. minecraft:speed"), ("duration", "seconds 1..3600, default 5"), ("amplifier", "0..255, default 0")]),
    ("heal", "heal the target", &[("amount", "health points >0..1000, default 4")]),
    ("teleport", "teleport the target", &[("destination", "required: 'x,y,z' or 'player:<name>'")]),
    ("command", "run a server command (off by default)", &[("command", "required, max 256, no leading '/', no ';'. Needs security.allow-raw-commands and security.allow-console-commands")]),
    ("prime-tnt", "spawn ignited TNT", &[("count", "1..30000, default 1"), ("fuse-ticks", "20..240, default 80"), ("burst-size", "1..12, default 6")]),
    ("lightning", "strike lightning repeatedly", &[("count", "1..32, default 1"), ("duration-seconds", "0..3600, default 60"), ("interval-ticks", "10..1200, default 20"), ("damage", "true|false"), ("multiply-by-repeat", "true|false")]),
    ("lightning-effect", "harmless lightning (no damage)", &[("count", "1..32, default 1"), ("duration-seconds", "0..3600, default 0"), ("interval-ticks", "10..1200, default 20")]),
    ("block-cage", "build a temporary cage around the target", &[("material", "block id, required"), ("radius", "1..3, default 2"), ("height", "3..6, default 3"), ("restore-seconds", "0..120, default 20"), ("ignite", "true|false (tnt only)"), ("fuse-ticks", "20..240, default 80"), ("ignite-delay-ticks", "1..200, default 40"), ("occupant", "entity id inside the cage"), ("occupant-charged", "true|false (creeper)")]),
    ("tnt-cage", "block-cage with TNT as the default material", &[("radius", "1..3, default 2"), ("height", "3..6, default 3"), ("restore-seconds", "0..120, default 20"), ("fuse-ticks", "20..240, default 80"), ("ignite-delay-ticks", "1..200, default 40")]),
    ("void-shaft", "open a temporary shaft under the target", &[("radius", "0..1, default 1"), ("blocks-per-tick", "1..128, default 96"), ("max-blocks", "1..4096, default 2048"), ("restore-seconds", "0..180, default 30")]),
    ("chunk-replace", "replace blocks of the target's chunk", &[("material", "block id, required"), ("radius-chunks", "0..1, default 0"), ("blocks-per-tick", "1..4096, default 512"), ("max-blocks", ">=0, default 0 (unlimited)"), ("restore-seconds", "0..180, default 0"), ("min-y", "whole number"), ("max-y", "whole number"), ("from", "only replace this block id")]),
    ("place-cobweb", "place temporary cobwebs", &[("count", "1..32, default 6"), ("restore-seconds", "1..120, default 12")]),
    ("temporary-clear-inventory", "empty the inventory and give it back", &[("restore-seconds", "1..30, default 8")]),
    ("function", "run a reusable function", &[("name", "function id, required (see /tiktok functions list)"), ("args", "mapping of argument values")]),
];

/// `/tiktok describe [triggers|fields|comparisons|targets|actions|action <type>|config]`.
pub fn describe(arg: Args<'_>) -> Vec<String> {
    let what = arg("what").map(|w| w.to_ascii_lowercase()).unwrap_or_default();
    let name = arg("name").map(|n| n.to_ascii_lowercase());
    match (what.as_str(), name) {
        ("triggers", _) => TRIGGER_DOCS.iter().map(|(n, d)| format!("{n} - {d}")).collect(),
        ("fields", _) => {
            let mut lines: Vec<String> = FIELD_DOCS.iter().map(|(n, d)| format!("{n} - {d}")).collect();
            lines.push("Aliases (usable in match, conditions and {placeholders}):".to_owned());
            lines.extend(crate::rules::fields::ALIASES.iter().map(|(a, p)| format!("  {a} = {p}")));
            lines.push("  viewer = display name (nickname, else @username)  [placeholders only]".to_owned());
            lines
        }
        ("comparisons", _) => {
            let mut lines = vec!["Comparisons for match: <field>: {<comparison>: <value>} (a plain value means equals):".to_owned()];
            lines.extend(crate::rules::compare::NAMES.iter().map(|n| format!("  {n}")));
            lines
        }
        ("targets", _) => TARGET_DOCS.iter().map(|(n, d)| format!("target: {n} - {d}")).collect(),
        ("actions", None) => ACTION_DOCS.iter().map(|(n, d, _)| format!("{n} - {d}")).collect(),
        ("action", Some(n)) | ("actions", Some(n)) => match ACTION_DOCS.iter().find(|(k, _, _)| *k == n) {
            Some((kind, summary, params)) => {
                let mut lines = vec![format!("{kind} - {summary}"), "  target: see /tiktok describe targets".to_owned()];
                lines.extend(params.iter().map(|(p, d)| format!("  {p}: {d}")));
                lines.push(format!("  example: /tiktok rules add my-rule gift.final {kind} {}", example_params(kind)));
                lines
            }
            None => vec![format!("Unknown action: {n}. See /tiktok describe actions.")],
        },
        ("action", None) => vec!["Usage: /tiktok describe action <type>".to_owned()],
        _ => vec![
            "What can be described:".to_owned(),
            "  /tiktok describe triggers       the events a rule can listen for".to_owned(),
            "  /tiktok describe fields         event fields and their aliases".to_owned(),
            "  /tiktok describe comparisons    how match compares a field".to_owned(),
            "  /tiktok describe targets        who an action affects".to_owned(),
            "  /tiktok describe actions        every action type".to_owned(),
            "  /tiktok describe action <type>  the parameters of one action".to_owned(),
            "  /tiktok config keys [section]   every config.yml key with its default".to_owned(),
            "Workflow: /tiktok rules add ... -> /tiktok test ... -> /tiktok status. Files: config/tiktok_live/AGENTS.md".to_owned(),
        ],
    }
}

fn example_params(kind: &str) -> &'static str {
    match kind {
        "broadcast" => "message=\"Thanks {viewer}!\" match.gift-name=Rose",
        "title" => "title=\"{gift.name}!\" subtitle=\"Thanks {viewer}\"",
        "spawn-entity" => "entity=minecraft:zombie name={nickname}",
        "give-item" => "material=minecraft:apple count=3",
        "effect" => "effect=minecraft:speed duration=10",
        "function" => "name=gifts:spawn_zombies",
        _ => "target=nearest-online",
    }
}

/// Every trigger, in case another module needs them next to their description.
#[allow(dead_code)]
pub fn trigger_names() -> &'static [&'static str] {
    &TRIGGERS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_keys_carry_defaults_and_comments() {
        let keys = config_keys();
        let key = keys.iter().find(|k| k.section == "chat" && k.key == "show-gifts").expect("show-gifts is documented");
        assert_eq!(key.default, "true");
        assert!(key.doc.contains("gifts"));
        assert!(keys.iter().any(|k| k.section == "security" && k.key == "execute-live-actions"));
    }

    #[test]
    fn values_follow_the_type_of_the_default() {
        use serde_norway::Value as V;
        assert_eq!(typed_edit(&V::Bool(true), "off"), Ok(Edit::Scalar("false".into())));
        assert!(typed_edit(&V::Bool(true), "maybe").is_err());
        assert_eq!(typed_edit(&serde_norway::from_str("5").unwrap(), "12"), Ok(Edit::Scalar("12".into())));
        assert!(typed_edit(&serde_norway::from_str("5").unwrap(), "x").is_err());
        assert_eq!(typed_edit(&serde_norway::from_str("[a]").unwrap(), "x, y"), Ok(Edit::List(vec!["x".into(), "y".into()])));
        assert_eq!(typed_edit(&V::String("a".into()), "hello \"x\""), Ok(Edit::Scalar("\"hello \\\"x\\\"\"".into())));
    }

    #[test]
    fn tokens_support_quotes() {
        let tokens = tokenize("message=\"Hi {viewer}!\" match.text:equals-ignore-case=!hi count=2").unwrap();
        assert_eq!(tokens[0], ("message".into(), "Hi {viewer}!".into()));
        assert_eq!(tokens[1], ("match.text:equals-ignore-case".into(), "!hi".into()));
        assert_eq!(tokens[2], ("count".into(), "2".into()));
        assert!(tokenize("novalue").is_err());
        assert!(tokenize("a=\"open").is_err());
    }

    #[test]
    fn a_rule_built_from_parameters_loads() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        let catalog = Catalog { config: &config, functions: None };
        let rule = build_rule("hello", "chat", "broadcast", "message=\"Hi {viewer}\" match.text:equals-ignore-case=!hi cooldown=10s:user").unwrap();
        let text = custom_text(&[rule]).unwrap();
        let rules = load_rules(&text, "custom.yml", &catalog).expect("the generated rule loads");
        assert_eq!(rules[0].id, "hello");
        assert_eq!(rules[0].actions[0].kind, "broadcast");
        assert_eq!(rules[0].matchers.len(), 1);
        let bad = build_rule("bad", "not-a-trigger", "broadcast", "message=x").unwrap();
        assert!(load_rules(&custom_text(&[bad]).unwrap(), "custom.yml", &catalog).is_err());
        let threshold = build_rule("likes", "like", "broadcast", "message=x every=50").unwrap();
        assert!(load_rules(&custom_text(&[threshold]).unwrap(), "custom.yml", &catalog).is_ok());
    }

    #[test]
    fn every_action_documented_is_a_known_action_and_vice_versa() {
        for (kind, _, _) in ACTION_DOCS {
            assert!(crate::actions::TYPES.contains(kind), "{kind} is documented but unknown");
        }
        for kind in crate::actions::TYPES {
            assert!(ACTION_DOCS.iter().any(|(k, _, _)| k == &kind), "{kind} has no description");
        }
        let docs: Vec<&str> = TRIGGER_DOCS.iter().map(|(n, _)| *n).collect();
        assert_eq!(docs, TRIGGERS.to_vec());
    }
}
