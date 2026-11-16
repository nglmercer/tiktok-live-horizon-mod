//! The mod's configuration (`config/tiktok_live/config.yml`), validated like the plugin's `PluginConfig`.
use crate::value::Val;

pub const DEFAULT_BUNDLE_URL: &str =
    "https://sf16-website-login.neutral.ttwstatic.com/obj/tiktok_web_login_static/webmssdk/1.0.0.388/webmssdk.js";
pub const DEFAULT_BUNDLE_SHA256: &str = "dee22566273d398e074df6db40f39cfd827f6b8efd6fc382de03c44c501299ac";

pub const DEFAULT_CONFIG: &str = r##"# TikTok LIVE — configuration.
#
# Every value is validated when the file is loaded. A reload that fails keeps the
# previous configuration, which works.

tiktok:
  # The creator's TikTok username. The leading @ is optional.
  unique-id: "@elpadredomingooficial"
  # This creator's default player (name or UUID); empty uses the rules' targets.
  target-player: ""
  # Additional collaborators, each with its own LIVE connection.
  # accounts:
  #   - unique-id: "@collaborator"
  #     target-player: "MinecraftPlayer"
  accounts: []
  # Connect as soon as the server starts.
  auto-connect: true
  # Retry after a connection failure.
  reconnect: true
  # Download the gift catalog to match gifts by exact id.
  fetch-gifts: true
  # Ignore the room's history instead of acting on it.
  ignore-history: true

connection:
  # WebSocket retries after a failure (the budget is renewed when it connects).
  max-retries: 3
  # Cookie header of a TikTok session. Empty: an anonymous guest identity.
  session-cookie: ""

signing:
  # TikTok's signing bundle (webmssdk.js). It does not ship with the mod: it is downloaded once,
  # its SHA-256 is verified and it is stored in horizon-mod-data/tiktok_live/. If TikTok rotates it,
  # the hash no longer matches and the connection stops with a clear error: the mod is updated,
  # nothing is ever signed blindly.
  bundle-url: "https://sf16-website-login.neutral.ttwstatic.com/obj/tiktok_web_login_static/webmssdk/1.0.0.388/webmssdk.js"
  # The SHA-256 the downloaded bundle must have.
  bundle-sha256: "dee22566273d398e074df6db40f39cfd827f6b8efd6fc382de03c44c501299ac"
  # Absolute path of a local bundle (your own mirror, or a server without Internet); empty uses the download.
  bundle-path: ""

chat:
  # Mirror TikTok chat at all.
  enabled: true
  # Print every chat message in the server console.
  console: true
  # Mirror every chat message into Minecraft chat.
  minecraft: true
  # The prefix shown in Minecraft chat.
  prefix: "[TikTok]"
  # Announce connection state changes in Minecraft chat (connected, the creator is not live,
  # errors...) and that rules are matching without executing.
  announce-status: true
  # Show finished gifts in Minecraft chat.
  show-gifts: true
  # Show follows and shares in Minecraft chat.
  show-social: true
  # Remove what Minecraft's font cannot draw (emojis) from text that comes from TikTok.
  strip-unsupported: true
  # The maximum number of characters of a chat message.
  max-length: 200
  # Ingest limit, in messages per second. What exceeds it is suppressed and reported in
  # /tiktok status.
  messages-per-second: 5
  # Maximum number of messages waiting to be shown.
  max-queue: 128

events:
  # Run rules at all.
  enabled: true
  # When true, a rule that does not say otherwise is active.
  default-enabled: true
  # How long an event's identifier is remembered to deduplicate.
  deduplication-window-seconds: 300
  # Maximum number of events waiting to be processed.
  max-pending-events: 512
  # Historical events never trigger actions that change the world.
  ignore-history: true

functions:
  # Run functions at all.
  enabled: true
  # How many functions may run at the same time.
  max-concurrent: 2
  # How many functions may wait in the queue.
  max-queued: 64
  # Minecraft commands run per tick across all functions.
  max-commands-per-tick: 4
  # Seconds a function may run before it is cancelled.
  max-execution-seconds: 60
  # Maximum repeat count of a function step.
  max-repeat: 20
  # Maximum nesting depth of function calls.
  max-depth: 8
  # Command roots a function may run. The function files are written by the administrator; '*' allows any command.
  allowed-commands:
    - summon
    - title
    - playsound
    - particle
    - effect

actions:
  # Where an action looks for its target player:
  # all-online, random-online, nearest-online, specific-player, specific-world
  target-policy: nearest-online
  # What to do when there is no eligible player: skip or console
  no-player-policy: skip
  # Maximum number of actions one event may run.
  max-actions-per-event: 8
  # Maximum number of entities one event may spawn.
  max-entities-per-event: 5
  # Maximum number of entities spawned per minute, across all events.
  max-entities-per-minute: 60
  # Entity types an action may spawn. Empty denies all spawning.
  allowed-entities:
    - zombie
    - chicken

security:
  # Master switch: while false, events are received, shown and matched,
  # but nothing they trigger changes the world. Try it first with /tiktok test.
  execute-live-actions: false
  # Allow the `command` action to run console commands.
  allow-console-commands: false
  # Allow the `command` action at all.
  allow-raw-commands: false

# Changes made in an editor are validated and applied without /tiktok reload.
hot-reload:
  enabled: true
  debounce-ms: 750

debug:
  # Log every event.
  log-events: false
  # Log connection state changes.
  log-connection: true
  # Log every rule match.
  log-rule-matches: false
"##;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetPolicy {
    AllOnline,
    RandomOnline,
    NearestOnline,
    SpecificPlayer,
    SpecificWorld,
}

impl TargetPolicy {
    fn parse(raw: Option<&str>, fallback: TargetPolicy) -> TargetPolicy {
        match raw.map(|s| s.trim().to_ascii_lowercase().replace('-', "_")).as_deref() {
            Some("all_online") => TargetPolicy::AllOnline,
            Some("random_online") => TargetPolicy::RandomOnline,
            Some("nearest_online") => TargetPolicy::NearestOnline,
            Some("specific_player") => TargetPolicy::SpecificPlayer,
            Some("specific_world") => TargetPolicy::SpecificWorld,
            _ => fallback,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoPlayerPolicy {
    Skip,
    Console,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub unique_id: String,
    pub target_player: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub unique_id: String,
    pub auto_connect: bool,
    pub reconnect: bool,
    pub fetch_gifts: bool,
    pub tiktok_ignore_history: bool,
    pub accounts: Vec<Account>,
    pub max_retries: i64,
    pub session_cookie: String,
    pub bundle_url: String,
    pub bundle_sha256: String,
    pub bundle_path: String,
    pub chat_enabled: bool,
    pub chat_console: bool,
    pub chat_minecraft: bool,
    pub chat_prefix: String,
    pub chat_max_length: i64,
    pub chat_messages_per_second: i64,
    pub chat_max_queue: i64,
    pub chat_announce_status: bool,
    pub chat_show_gifts: bool,
    pub chat_show_social: bool,
    pub chat_strip_unsupported: bool,
    pub events_enabled: bool,
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub events_default_enabled: bool,
    pub dedup_window_seconds: i64,
    pub max_pending_events: i64,
    pub events_ignore_history: bool,
    pub functions_enabled: bool,
    pub max_concurrent_functions: i64,
    pub max_queued_functions: i64,
    pub max_commands_per_tick: i64,
    pub max_execution_seconds: i64,
    pub max_repeat: i64,
    pub max_depth: i64,
    pub allowed_commands: Vec<String>,
    pub target_policy: TargetPolicy,
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub no_player_policy: NoPlayerPolicy,
    pub max_actions_per_event: i64,
    pub max_entities_per_event: i64,
    pub max_entities_per_minute: i64,
    pub allowed_entities: Vec<String>,
    pub execute_live_actions: bool,
    pub allow_console_commands: bool,
    pub allow_raw_commands: bool,
    pub hot_reload_enabled: bool,
    pub hot_reload_debounce_ms: i64,
    pub log_events: bool,
    pub log_connection: bool,
    pub log_rule_matches: bool,
}

pub fn normalize_unique_id(raw: &str) -> String {
    let text = raw.trim();
    text.strip_prefix('@').unwrap_or(text).to_owned()
}

fn section<'a>(root: &'a Val, name: &str) -> Option<&'a Val> {
    root.get(name).filter(|v| matches!(v, Val::Map(_)))
}

fn get_bool(section: Option<&Val>, key: &str, fallback: bool) -> bool {
    section.and_then(|s| s.get(key)).and_then(Val::as_bool).unwrap_or(fallback)
}

fn get_string(section: Option<&Val>, key: &str, fallback: &str) -> String {
    match section.and_then(|s| s.get(key)) {
        Some(Val::Null) | None => fallback.to_owned(),
        Some(v) => v.text(),
    }
}

fn clamp(section: Option<&Val>, key: &str, fallback: i64, min: i64, max: i64) -> i64 {
    let value = section.and_then(|s| s.get(key)).and_then(Val::as_i64).unwrap_or(fallback);
    value.clamp(min, max)
}

fn read_strings(section: Option<&Val>, key: &str, fallback: &[&str]) -> Vec<String> {
    match section.and_then(|s| s.get(key)) {
        Some(Val::List(items)) => items
            .iter()
            .filter(|v| !v.is_null())
            .map(|v| v.text().trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => fallback.iter().map(|s| s.trim().to_ascii_lowercase()).collect(),
    }
}

fn valid_username(name: &str) -> bool {
    (1..=24).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

fn valid_player_name_or_uuid(name: &str) -> bool {
    let simple = (1..=16).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let parts: Vec<&str> = name.split('-').collect();
    let uuid = parts.len() == 5
        && [8usize, 4, 4, 4, 12].iter().zip(&parts).all(|(n, p)| p.len() == *n && p.chars().all(|c| c.is_ascii_hexdigit()));
    simple || uuid
}

impl Config {
    /// Reads and validates a `config.yml`. Returns every problem found.
    pub fn parse(yaml: &str) -> Result<Config, Vec<String>> {
        let raw: serde_norway::Value =
            serde_norway::from_str(yaml).map_err(|e| vec![format!("The file is not valid YAML: {e}")])?;
        let root = Val::from_yaml(raw);
        if !matches!(root, Val::Map(_)) {
            return Err(vec!["Configuration could not be read".to_owned()]);
        }
        let tiktok = section(&root, "tiktok");
        let chat = section(&root, "chat");
        let connection = section(&root, "connection");
        let signing = section(&root, "signing");
        let events = section(&root, "events");
        let functions = section(&root, "functions");
        let actions = section(&root, "actions");
        let security = section(&root, "security");
        let hot = section(&root, "hot-reload");
        let debug = section(&root, "debug");

        let unique_id = normalize_unique_id(&get_string(tiktok, "unique-id", ""));
        let mut accounts = vec![Account {
            unique_id: unique_id.clone(),
            target_player: get_string(tiktok, "target-player", "").trim().to_owned(),
        }];
        let mut problems = Vec::new();
        match tiktok.and_then(|t| t.get("accounts")) {
            Some(Val::List(items)) => {
                for item in items {
                    match item {
                        Val::Map(map) => accounts.push(Account {
                            unique_id: normalize_unique_id(&map.get("unique-id").map(Val::text).unwrap_or_default()),
                            target_player: map.get("target-player").map(|v| v.text().trim().to_owned()).unwrap_or_default(),
                        }),
                        _ => {
                            problems.push("Each tiktok.accounts entry must contain unique-id and optional target-player".to_owned());
                            break;
                        }
                    }
                }
            }
            Some(Val::Null) | None => {}
            Some(_) => problems.push("tiktok.accounts must be a list".to_owned()),
        }

        let config = Config {
            unique_id,
            auto_connect: tiktok.is_none() || get_bool(tiktok, "auto-connect", true),
            reconnect: tiktok.is_none() || get_bool(tiktok, "reconnect", true),
            fetch_gifts: tiktok.is_none() || get_bool(tiktok, "fetch-gifts", true),
            tiktok_ignore_history: tiktok.is_none() || get_bool(tiktok, "ignore-history", true),
            accounts,
            max_retries: clamp(connection, "max-retries", 3, 0, 20),
            session_cookie: get_string(connection, "session-cookie", "").trim().to_owned(),
            bundle_url: get_string(signing, "bundle-url", DEFAULT_BUNDLE_URL).trim().to_owned(),
            bundle_sha256: get_string(signing, "bundle-sha256", DEFAULT_BUNDLE_SHA256).trim().to_ascii_lowercase(),
            bundle_path: get_string(signing, "bundle-path", "").trim().to_owned(),
            chat_enabled: chat.is_none() || get_bool(chat, "enabled", true),
            chat_console: chat.is_none() || get_bool(chat, "console", true),
            chat_minecraft: chat.is_none() || get_bool(chat, "minecraft", true),
            chat_prefix: get_string(chat, "prefix", "[TikTok]"),
            chat_max_length: clamp(chat, "max-length", 200, 16, 1000),
            chat_messages_per_second: clamp(chat, "messages-per-second", 5, 1, 100),
            chat_max_queue: clamp(chat, "max-queue", 128, 1, 4096),
            chat_announce_status: chat.is_none() || get_bool(chat, "announce-status", true),
            chat_show_gifts: chat.is_none() || get_bool(chat, "show-gifts", true),
            chat_show_social: chat.is_none() || get_bool(chat, "show-social", true),
            chat_strip_unsupported: chat.is_none() || get_bool(chat, "strip-unsupported", true),
            events_enabled: events.is_none() || get_bool(events, "enabled", true),
            events_default_enabled: events.is_some() && get_bool(events, "default-enabled", false),
            dedup_window_seconds: clamp(events, "deduplication-window-seconds", 300, 1, 86_400),
            max_pending_events: clamp(events, "max-pending-events", 512, 1, 65_536),
            events_ignore_history: events.is_none() || get_bool(events, "ignore-history", true),
            functions_enabled: functions.is_none() || get_bool(functions, "enabled", true),
            max_concurrent_functions: clamp(functions, "max-concurrent", 2, 1, 64),
            max_queued_functions: clamp(functions, "max-queued", 64, 0, 65_536),
            max_commands_per_tick: clamp(functions, "max-commands-per-tick", 4, 1, 256),
            max_execution_seconds: clamp(functions, "max-execution-seconds", 60, 1, 3600),
            max_repeat: clamp(functions, "max-repeat", 20, 1, 10_000),
            max_depth: clamp(functions, "max-depth", 8, 1, 64),
            allowed_commands: read_strings(functions, "allowed-commands", &["summon", "title", "playsound", "particle", "effect"]),
            target_policy: TargetPolicy::parse(
                actions.and_then(|a| a.get("target-policy")).map(Val::text).as_deref(),
                TargetPolicy::NearestOnline,
            ),
            no_player_policy: match actions
                .and_then(|a| a.get("no-player-policy"))
                .map(|v| v.text().trim().to_ascii_uppercase())
                .as_deref()
            {
                Some("CONSOLE") => NoPlayerPolicy::Console,
                _ => NoPlayerPolicy::Skip,
            },
            max_actions_per_event: clamp(actions, "max-actions-per-event", 8, 0, 256),
            max_entities_per_event: clamp(actions, "max-entities-per-event", 5, 0, 1000),
            max_entities_per_minute: clamp(actions, "max-entities-per-minute", 60, 0, 100_000),
            allowed_entities: read_strings(actions, "allowed-entities", &[]),
            execute_live_actions: security.is_some() && get_bool(security, "execute-live-actions", false),
            allow_console_commands: security.is_some() && get_bool(security, "allow-console-commands", false),
            allow_raw_commands: security.is_some() && get_bool(security, "allow-raw-commands", false),
            hot_reload_enabled: hot.is_none() || get_bool(hot, "enabled", true),
            hot_reload_debounce_ms: clamp(hot, "debounce-ms", 750, 250, 5000),
            log_events: debug.is_some() && get_bool(debug, "log-events", false),
            log_connection: debug.is_none() || get_bool(debug, "log-connection", true),
            log_rule_matches: debug.is_some() && get_bool(debug, "log-rule-matches", false),
        };

        if config.unique_id.is_empty() {
            problems.push("tiktok.unique-id must not be empty".to_owned());
        } else if !valid_username(&config.unique_id) {
            problems.push(format!("tiktok.unique-id must be a TikTok username, got: {}", config.unique_id));
        }
        let mut seen = std::collections::HashSet::new();
        for account in &config.accounts {
            if !valid_username(&account.unique_id) {
                problems.push(format!("Invalid TikTok account username: {}", account.unique_id));
            }
            if !seen.insert(account.unique_id.to_ascii_lowercase()) {
                problems.push(format!("Duplicate TikTok account: {}", account.unique_id));
            }
            if !account.target_player.is_empty() && !valid_player_name_or_uuid(&account.target_player) {
                problems.push(format!("target-player must be a Minecraft player name or UUID: {}", account.target_player));
            }
        }
        if config.bundle_sha256.len() != 64 || !config.bundle_sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            problems.push("signing.bundle-sha256 must be 64 hexadecimal characters".to_owned());
        }
        if !config.bundle_url.starts_with("https://") {
            problems.push("signing.bundle-url must be an https:// URL".to_owned());
        }
        if config.chat_prefix.chars().count() > 32 {
            problems.push("chat.prefix must be at most 32 characters".to_owned());
        }
        if config.functions_enabled && config.allowed_commands.is_empty() {
            problems.push("functions.allowed-commands must list at least one command root".to_owned());
        }
        if config.execute_live_actions && !config.functions_enabled && !config.events_enabled {
            problems.push("security.execute-live-actions is enabled but neither events nor functions are".to_owned());
        }
        if problems.is_empty() { Ok(config) } else { Err(problems) }
    }

    pub fn entity_allowed(&self, name: &str) -> bool {
        let lower = name.trim().to_ascii_lowercase();
        let short = lower.strip_prefix("minecraft:").unwrap_or(&lower);
        self.allowed_entities.iter().any(|a| a == "*" || a == &lower || a == short)
    }

    pub fn command_allowed(&self, root: &str) -> bool {
        let lower = root.to_ascii_lowercase();
        self.allowed_commands.iter().any(|c| c == "*" || c == &lower)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_config_is_valid_and_conservative() {
        let config = Config::parse(DEFAULT_CONFIG).expect("default config parses");
        assert_eq!(config.unique_id, "elpadredomingooficial");
        assert!(!config.execute_live_actions);
        assert_eq!(config.target_policy, TargetPolicy::NearestOnline);
        assert!(config.entity_allowed("minecraft:zombie"));
        assert!(!config.entity_allowed("wither"));
    }

    #[test]
    fn invalid_values_are_reported() {
        let bad = "tiktok:\n  unique-id: \"\"\nchat:\n  prefix: \"0123456789012345678901234567890123456789\"\n";
        let problems = Config::parse(bad).unwrap_err();
        assert!(problems.iter().any(|p| p.contains("unique-id must not be empty")));
        assert!(problems.iter().any(|p| p.contains("chat.prefix")));
    }
}
