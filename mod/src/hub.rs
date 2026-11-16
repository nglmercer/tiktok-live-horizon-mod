//! The hub: the mod's shared state, the event pipeline and configuration reloading.
//! Nothing here uses the `host` outside a hook; the network threads only write to `inbox`.
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use mod_api::{ConfigEntryKind, ConfigPath, ItemStack, ServerHost, Text};

use crate::actions::{self, Catalog, Cx, Emit, RateLimiter, Target};
use crate::blocks::BlockManager;
use crate::chat::ChatBridge;
use crate::hostworld::HostWorld;
use crate::config::{Config, DEFAULT_CONFIG};
use crate::event::Event;
use crate::functions::engine::Engine as FunctionEngine;
use crate::functions::model::Definition;
use crate::functions::{compile as fcompile, parse as fparse, Limits as FnLimits};
use crate::live::{BundleSource, Inbound, LiveHandle, Settings, sha256_hex};
use crate::rules::engine::{RuleEngine, Snapshot};
use crate::rules::loader;
use crate::scheduler::Scheduler;

pub const MOD_ID: &str = "tiktok_live";
pub const CONFIG_PATH: &str = "tiktok_live/config.yml";
pub const EVENTS_DIR: &str = "tiktok_live/events";
pub const FUNCTIONS_DIR: &str = "tiktok_live/functions";
pub const SCRIPTS_DIR: &str = "tiktok_live/scripts";

/// A TikTok event older than this is dropped when drained (e.g. after a pause).
const MAX_EVENT_AGE_MS: i64 = 60_000;
const MAX_EVENTS_PER_TICK: usize = 32;
/// How many ticks pass between checks of the rule files (1 s at 20 TPS).
const POLL_TICKS: u64 = 20;

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub const BUNDLE_FILE: &str = "webmssdk.js";

/// How many messages of one connection are drained per tick.
const MAX_INBOUND_PER_TICK: usize = 256;

pub struct Connection {
    pub handle: LiveHandle,
    /// What makes a configuration change require a reconnect.
    pub signature: String,
}

#[derive(Default)]
pub struct Stats {
    pub received: u64,
    pub dropped: u64,
    pub duplicates: u64,
    pub processed: u64,
    pub matched: u64,
    pub last_event: String,
    pub command_failures: u64,
}

pub struct Hub {
    pub config: Config,
    pub engine: RuleEngine,
    pub limiter: RateLimiter,
    pub sched: Scheduler,
    pub chat: ChatBridge,
    pub functions: FunctionEngine,
    pub blocks: BlockManager,
    /// Inventories in custody: player (lower-case) -> (ticks until returned, items per slot).
    escrow: HashMap<String, (u64, Vec<(u32, ItemStack)>)>,
    pub stats: Stats,
    pub tick: u64,
    pub started_ms: i64,
    pub inbox: VecDeque<Event>,
    seen: HashMap<String, i64>,
    pub reload_pending: bool,
    pub last_reload: String,
    pub debug: bool,
    signature: String,
    signature_changed_at: Option<i64>,
    synthetic: u64,
    pub connections: Vec<Connection>,
    /// Accounts the administrator disconnected by hand: they do not reconnect on their own.
    pub manual_off: HashSet<String>,
    started_connections: bool,
    defaults_done: bool,
    last_state: HashMap<String, String>,
    /// Already announced (once per start) that rules are matching without executing.
    announced_live_off: bool,
}

impl Hub {
    pub fn new() -> Hub {
        let config = Config::parse(DEFAULT_CONFIG).expect("the shipped default configuration is valid");
        let limiter = RateLimiter::new(config.max_entities_per_minute);
        Hub {
            config,
            engine: RuleEngine::default(),
            limiter,
            sched: Scheduler::default(),
            chat: ChatBridge::default(),
            functions: FunctionEngine::default(),
            blocks: BlockManager::default(),
            escrow: HashMap::new(),
            stats: Stats::default(),
            tick: 0,
            started_ms: now_ms(),
            inbox: VecDeque::new(),
            seen: HashMap::new(),
            reload_pending: true,
            last_reload: "not loaded yet".to_owned(),
            debug: false,
            signature: String::new(),
            signature_changed_at: None,
            synthetic: 0,
            connections: Vec::new(),
            manual_off: HashSet::new(),
            started_connections: false,
            defaults_done: false,
            last_state: HashMap::new(),
            announced_live_off: false,
        }
    }

    // --- event intake --------------------------------------------------------------------

    /// Puts an event in the queue (filters history, duplicates and overflow). Safe to call
    /// from anywhere; the work with the game happens in `on_tick`.
    pub fn offer(&mut self, event: Event) {
        self.stats.received += 1;
        if (self.config.events_ignore_history || self.config.tiktok_ignore_history) && event.history {
            self.stats.dropped += 1;
            return;
        }
        if !self.admit(&event) {
            self.stats.duplicates += 1;
            return;
        }
        if self.inbox.len() as i64 >= self.config.max_pending_events {
            self.stats.dropped += 1;
            return;
        }
        self.inbox.push_back(event);
    }

    pub fn next_synthetic_id(&mut self) -> String {
        self.synthetic += 1;
        format!("synthetic-{}-{}", self.started_ms, self.synthetic)
    }

    fn dedup_key(event: &Event) -> String {
        let mut key = format!("{}|{}|{}", event.room_id, event.kind, event.id);
        if event.kind == "gift" || event.kind == "gift.final" {
            let group = event.payload.get("gift.groupId").map(|v| v.text()).unwrap_or_default();
            let gift = event.payload.get("gift.id").map(|v| v.text()).unwrap_or_default();
            key.push_str(&format!("|{group}|{gift}"));
            if event.kind == "gift.final" {
                key.push_str("|final");
            }
        }
        key
    }

    fn admit(&mut self, event: &Event) -> bool {
        if event.id.is_empty() {
            return true;
        }
        let now = now_ms();
        let window = self.config.dedup_window_seconds * 1000;
        let key = Self::dedup_key(event);
        if let Some(previous) = self.seen.get(&key) {
            if now - previous < window {
                return false;
            }
        }
        self.seen.insert(key, now);
        if self.seen.len() > (self.config.dedup_window_seconds * 64).max(256) as usize {
            self.seen.retain(|_, at| now - *at < window);
        }
        true
    }

    // --- the tick -----------------------------------------------------------------------

    pub fn on_tick(&mut self, host: &mut dyn ServerHost) {
        self.tick += 1;
        if self.reload_pending {
            self.reload_pending = false;
            let _ = self.apply_reload(host);
        } else if self.tick % POLL_TICKS == 0 {
            self.poll_files(host);
        }
        self.drain_connections(host);
        self.drain_events(host);
        let lines = self.chat.drain();
        for line in lines {
            let _ = host.messages().broadcast(Text::literal(line));
        }
        self.tick_functions(host);
        self.tick_blocks(host);
        self.tick_escrow(host);
        let due = self.sched.due(self.tick);
        for command in due {
            self.run_command(host, &command);
        }
        if self.tick % POLL_TICKS == 0 {
            let suppressed = self.chat.take_suppressed();
            if suppressed > 0 {
                log::warn!("Suppressed {suppressed} chat message(s) by the ingest rate limit");
            }
        }
    }

    fn drain_events(&mut self, host: &mut dyn ServerHost) {
        let mut handled = 0;
        while handled < MAX_EVENTS_PER_TICK {
            let Some(event) = self.inbox.pop_front() else {
                break;
            };
            handled += 1;
            self.stats.processed += 1;
            self.handle_event(host, event);
        }
    }

    fn handle_event(&mut self, host: &mut dyn ServerHost, event: Event) {
        if let Some(source) = event.payload.get("connection.uniqueId") {
            let source = source.text();
            if !self.config.accounts.iter().any(|a| a.unique_id.eq_ignore_ascii_case(&source)) {
                return;
            }
        }
        let now = now_ms();
        self.stats.last_event = format!("{} at {}", event.kind, now);
        if let Some(line) = self.chat.accept(&self.config, &event, now) {
            if let Some(console) = line.console {
                log::info!("{console}");
            }
        }
        if self.config.log_events {
            log::info!("event {} id={} user={}", event.kind, event.id, event.user_key());
        }
        if !self.config.events_enabled {
            return;
        }
        let matched = self.engine.matching(&event, now);
        if matched.is_empty() {
            return;
        }
        self.stats.matched += matched.len() as u64;
        if self.config.log_rule_matches {
            for rule in &matched {
                log::info!("Matched {} ({})", rule.id, event.kind);
            }
        }
        if !self.config.execute_live_actions {
            // Rules match, but nothing an event triggers may change the world.
            if self.debug {
                log::info!("{} rule(s) matched {} but security.execute-live-actions is false", matched.len(), event.kind);
            }
            if !self.announced_live_off && self.config.chat_announce_status && self.config.chat_enabled {
                self.announced_live_off = true;
                self.chat.push_system(
                    &self.config,
                    "Rules are matching events but live actions are OFF, so nothing changes in the world. Enable security.execute-live-actions or run /tiktok preset install.",
                );
            }
            return;
        }
        let players = snapshot_players(host);
        let mut limiter = std::mem::replace(&mut self.limiter, RateLimiter::new(0));
        let mut emits = Vec::new();
        let mut world = HostWorld { host: &mut *host };
        {
            let mut cx = Cx::new(&self.config, &event, &players, now, &mut limiter).with_world(&mut world, &mut self.blocks, self.tick);
            cx.locked = self.escrow.keys().cloned().collect();
            for rule in &matched {
                for spec in &rule.actions {
                    if !cx.consume_action_slot() {
                        self.engine.rejected += 1;
                        break;
                    }
                    let outcome = actions::execute(spec, &mut cx);
                    if outcome.is_ok() {
                        self.engine.executed += 1;
                    } else {
                        self.engine.rejected += 1;
                    }
                    if self.debug {
                        log::info!("{} -> {}: {outcome}", rule.id, spec.kind);
                    }
                }
                if rule.stop_processing {
                    break;
                }
            }
            emits.append(&mut cx.out);
        }
        self.limiter = limiter;
        self.run_emits(host, emits);
    }

    pub fn run_emits(&mut self, host: &mut dyn ServerHost, emits: Vec<Emit>) {
        for emit in emits {
            match emit {
                Emit::Command(command) => self.run_command(host, &command),
                Emit::Broadcast(message) => {
                    let _ = host.messages().broadcast(Text::literal(message));
                }
                Emit::Escrow { player, ticks } => self.take_inventory(host, &player, ticks),
                Emit::Function { name, args, target } => {
                    if let Err(error) = self.submit_function(&name, &args, target) {
                        self.engine.rejected += 1;
                        log::warn!("function {name}: {error}");
                    }
                }
                Emit::Later { ticks, command } => {
                    if !self.sched.later(self.tick, ticks, command) {
                        self.engine.rejected += 1;
                    }
                }
                Emit::Every { key, interval, duration, commands } => {
                    if !self.sched.every(self.tick, key, interval, duration, commands) {
                        self.engine.rejected += 1;
                    }
                }
            }
        }
    }

    fn run_command(&mut self, host: &mut dyn ServerHost, command: &str) {
        if !host.commands().run_command(command) {
            self.stats.command_failures += 1;
            if self.debug {
                log::info!("command failed: {command}");
            }
        }
    }

    // --- configuration and rules ----------------------------------------------------------

    /// Rereads configuration and rules. It is transactional: if anything fails, everything before is kept.
    pub fn reload(&mut self, host: &mut dyn ServerHost) -> Result<usize, Vec<String>> {
        if !self.defaults_done {
            self.defaults_done = true;
            install_defaults(host);
        }
        let text = match read_text(host, CONFIG_PATH) {
            Ok(text) => text,
            Err(_) => DEFAULT_CONFIG.to_owned(),
        };
        let config = Config::parse(&text).map_err(|problems| problems.into_iter().map(|p| format!("config.yml: {p}")).collect::<Vec<_>>())?;
        let (registry, mut problems) = read_functions(host, &config);
        let known: HashSet<String> = registry.keys().cloned().collect();
        let (rules, rule_problems) = read_rules(host, &config, &known);
        problems.extend(rule_problems);
        if !problems.is_empty() {
            return Err(problems);
        }
        let count = rules.len();
        self.functions.replace_registry(registry);
        self.limiter.set_limit(config.max_entities_per_minute);
        self.config = config;
        self.engine.apply(Snapshot::new(rules));
        self.signature = file_signature(host);
        Ok(count)
    }

    /// Reloads and applies; returns how many rules ended up loaded or the problems (the previous
    /// configuration stays active if there are any).
    pub fn apply_reload(&mut self, host: &mut dyn ServerHost) -> Result<usize, Vec<String>> {
        match self.reload(host) {
            Ok(count) => {
                self.last_reload = format!("ok: {count} rule(s) loaded");
                log::info!("Configuration and {count} rule(s) loaded");
                self.reconcile_connections(host);
                Ok(count)
            }
            Err(problems) => {
                self.last_reload = format!("failed: {}", problems.join("; "));
                log::warn!("Reload failed; the previous configuration is still active:");
                for problem in &problems {
                    log::warn!("  - {problem}");
                }
                Err(problems)
            }
        }
    }

    /// The same as a reload without applying anything: `(rules, functions)` or the problems.
    pub fn validate(&mut self, host: &mut dyn ServerHost) -> Result<(usize, usize), Vec<String>> {
        let text = read_text(host, CONFIG_PATH).unwrap_or_else(|_| DEFAULT_CONFIG.to_owned());
        let config = Config::parse(&text).map_err(|problems| problems.into_iter().map(|p| format!("config.yml: {p}")).collect::<Vec<_>>())?;
        let (registry, mut problems) = read_functions(host, &config);
        let known: HashSet<String> = registry.keys().cloned().collect();
        let (rules, rule_problems) = read_rules(host, &config, &known);
        problems.extend(rule_problems);
        if problems.is_empty() {
            Ok((rules.len(), registry.len()))
        } else {
            Err(problems)
        }
    }

    /// Detects changes in `events/` (the game already watches `config.yml`) and reloads after the debounce.
    fn poll_files(&mut self, host: &mut dyn ServerHost) {
        if !self.config.hot_reload_enabled {
            return;
        }
        let now = now_ms();
        let signature = file_signature(host);
        if signature != self.signature {
            match self.signature_changed_at {
                None => self.signature_changed_at = Some(now),
                Some(since) if now - since >= self.config.hot_reload_debounce_ms => {
                    self.signature_changed_at = None;
                    let _ = self.apply_reload(host);
                }
                Some(_) => {}
            }
        } else {
            self.signature_changed_at = None;
        }
    }

    // --- connections to TikTok -------------------------------------------------------------

    fn account_signature(&self, unique_id: &str) -> String {
        let c = &self.config;
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            unique_id.to_ascii_lowercase(),
            c.reconnect,
            c.fetch_gifts,
            c.max_retries,
            c.session_cookie,
            c.bundle_url,
            c.bundle_sha256,
            c.bundle_path
        )
    }

    /// The signing bundle: from a local file, from the mod's data store, or to be downloaded.
    fn bundle_source(&self, host: &mut dyn ServerHost) -> Result<BundleSource, String> {
        let expected = self.config.bundle_sha256.clone();
        let verified = |text: String, origin: &str| -> Result<BundleSource, String> {
            let actual = sha256_hex(text.as_bytes());
            if actual == expected {
                Ok(BundleSource::Ready(text))
            } else {
                Err(format!("{origin}: sha256 {actual} does not match signing.bundle-sha256 {expected}"))
            }
        };
        if !self.config.bundle_path.is_empty() {
            let text = std::fs::read_to_string(&self.config.bundle_path)
                .map_err(|e| format!("signing.bundle-path {}: {e}", self.config.bundle_path))?;
            return verified(text, "signing.bundle-path");
        }
        if let Ok(path) = ConfigPath::new(BUNDLE_FILE) {
            if let Ok(bytes) = host.data_files().read(&path) {
                if let Ok(text) = String::from_utf8(bytes) {
                    if let Ok(ready) = verified(text, "stored bundle") {
                        return Ok(ready);
                    }
                }
            }
        }
        Ok(BundleSource::Download { url: self.config.bundle_url.clone(), sha256: expected })
    }

    pub fn start_connection(&mut self, host: &mut dyn ServerHost, account: &crate::config::Account) {
        let bundle = match self.bundle_source(host) {
            Ok(bundle) => bundle,
            Err(error) => {
                log::warn!("@{}: {error}", account.unique_id);
                self.last_reload = format!("failed: {error}");
                return;
            }
        };
        let settings = Settings {
            unique_id: account.unique_id.clone(),
            target_player: account.target_player.clone(),
            max_retries: self.config.max_retries as u32,
            reconnect: self.config.reconnect,
            fetch_gifts: self.config.fetch_gifts,
            session_cookie: self.config.session_cookie.clone(),
            bundle,
        };
        match LiveHandle::spawn(settings) {
            Ok(handle) => {
                if self.config.log_connection {
                    log::info!("Connecting to @{}", account.unique_id);
                }
                let signature = self.account_signature(&account.unique_id);
                self.connections.push(Connection { handle, signature });
            }
            Err(error) => log::warn!("@{}: could not start the connection: {error}", account.unique_id),
        }
    }

    fn stop_connection(&mut self, index: usize) {
        let mut connection = self.connections.remove(index);
        connection.handle.request_stop();
        // The thread's stop is awaited with a limit; if it does not end, it is left running without blocking the tick.
        connection.handle.join_timeout(std::time::Duration::from_millis(500));
    }

    /// Aligns connections with the configuration: starts the new ones, stops the ones that no longer exist
    /// and restarts the ones whose relevant settings changed. A rules change never touches them.
    pub fn reconcile_connections(&mut self, host: &mut dyn ServerHost) {
        let desired: Vec<crate::config::Account> = self.config.accounts.clone();
        let mut index = 0;
        while index < self.connections.len() {
            let name = self.connections[index].handle.unique_id.clone();
            match desired.iter().find(|a| a.unique_id.eq_ignore_ascii_case(&name)) {
                None => self.stop_connection(index),
                Some(account) => {
                    if self.connections[index].signature != self.account_signature(&account.unique_id) {
                        self.stop_connection(index);
                        self.start_connection(host, account);
                        // The new connection ends up last; we do not advance so as not to skip the next one.
                    } else {
                        index += 1;
                    }
                }
            }
        }
        if self.config.auto_connect || self.started_connections {
            for account in &desired {
                let running = self.connections.iter().any(|c| c.handle.unique_id.eq_ignore_ascii_case(&account.unique_id));
                let off = self.manual_off.contains(&account.unique_id.to_ascii_lowercase());
                if !running && !off && (self.config.auto_connect || self.started_connections) {
                    self.start_connection(host, account);
                }
            }
        }
        self.started_connections = true;
    }

    fn drain_connections(&mut self, host: &mut dyn ServerHost) {
        let mut events = Vec::new();
        let mut bundles = Vec::new();
        for connection in &self.connections {
            for _ in 0..MAX_INBOUND_PER_TICK {
                match connection.handle.rx.try_recv() {
                    Ok(Inbound::Event(event)) => events.push((connection.handle.unique_id.clone(), *event)),
                    Ok(Inbound::Bundle(text)) => bundles.push(text),
                    Err(_) => break,
                }
            }
        }
        for text in bundles {
            if let Ok(path) = ConfigPath::new(BUNDLE_FILE) {
                match host.data_files().write(&path, text.as_bytes()) {
                    Ok(()) => log::info!("Signing bundle stored for the next start ({} bytes)", text.len()),
                    Err(error) => log::warn!("Could not store the signing bundle: {error:?}"),
                }
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        for (account, mut event) in events {
            // If the server was paused, the accumulated events are no longer "live":
            // reacting to them all at once would be a flood of late actions.
            if event.timestamp > 0 && now - event.timestamp > MAX_EVENT_AGE_MS {
                self.stats.dropped += 1;
                continue;
            }
            // The target player is the one in the current configuration (changing it does not reconnect).
            let target = self
                .config
                .accounts
                .iter()
                .find(|a| a.unique_id.eq_ignore_ascii_case(&account))
                .map(|a| a.target_player.clone())
                .unwrap_or_default();
            event.payload.insert("connection.targetPlayer".to_owned(), crate::value::Val::Str(target));
            self.offer(event);
        }
        let mut changes = Vec::new();
        for connection in &self.connections {
            let status = connection.handle.status();
            let line = format!("{}:{}:{}", connection.handle.unique_id, status.state, status.detail);
            let previous = self.last_state.insert(connection.handle.unique_id.clone(), line.clone());
            if previous.as_deref() != Some(line.as_str()) {
                changes.push((connection.handle.unique_id.clone(), status.state, status.detail));
            }
        }
        for (account, state, detail) in changes {
            if self.config.log_connection {
                log::info!("@{account} is {state} ({detail})");
            }
            if self.config.chat_announce_status && self.config.chat_enabled {
                let text = if detail.is_empty() { format!("@{account} is {state}") } else { format!("@{account} is {state}: {detail}") };
                self.chat.push_system(&self.config, &text);
            }
        }
    }

    fn running_index(&self, unique_id: &str) -> Option<usize> {
        self.connections.iter().position(|c| c.handle.unique_id.eq_ignore_ascii_case(unique_id))
    }

    /// `/tiktok connect`: starts the account if it is not running and removes it from the disconnected list.
    pub fn connect_account(&mut self, host: &mut dyn ServerHost, account: &crate::config::Account) -> bool {
        self.manual_off.remove(&account.unique_id.to_ascii_lowercase());
        if self.running_index(&account.unique_id).is_some() {
            return false;
        }
        self.start_connection(host, account);
        true
    }

    /// `/tiktok disconnect`: stops the account and keeps it from reconnecting on its own.
    pub fn disconnect_account(&mut self, account: &crate::config::Account) -> bool {
        self.manual_off.insert(account.unique_id.to_ascii_lowercase());
        match self.running_index(&account.unique_id) {
            Some(index) => {
                self.stop_connection(index);
                true
            }
            None => false,
        }
    }

    /// `/tiktok reconnect`: restarts the account immediately.
    pub fn reconnect_account(&mut self, host: &mut dyn ServerHost, account: &crate::config::Account) {
        self.manual_off.remove(&account.unique_id.to_ascii_lowercase());
        if let Some(index) = self.running_index(&account.unique_id) {
            self.stop_connection(index);
        }
        self.start_connection(host, account);
    }

    /// Stops all connections (called when the server stops).
    pub fn shutdown(&mut self) {
        for connection in &self.connections {
            connection.handle.request_stop();
        }
        for mut connection in self.connections.drain(..) {
            if !connection.handle.join_timeout(std::time::Duration::from_secs(3)) {
                log::warn!("The connection to @{} did not stop in time; leaving it to finish", connection.handle.unique_id);
            }
        }
    }

    // --- functions -----------------------------------------------------------------------

    pub fn fn_limits(&self) -> FnLimits {
        FnLimits::from_config(&self.config)
    }

    /// Compiles and queues a function. `Err` with the reason if it is rejected.
    pub fn submit_function(&mut self, name: &str, args: &std::collections::BTreeMap<String, crate::value::Val>, target: Option<String>) -> Result<u64, String> {
        if !self.config.functions_enabled {
            return Err("functions are disabled (functions.enabled)".to_owned());
        }
        if !self.config.allow_console_commands {
            return Err("Console commands are disabled (security.allow-console-commands)".to_owned());
        }
        let limits = self.fn_limits();
        let allowed = self.config.allowed_commands.clone();
        let (concurrent, queued) = (self.config.max_concurrent_functions as usize, self.config.max_queued_functions as usize);
        self.functions.submit(name, args, target, &limits, &allowed, concurrent, queued, now_ms())
    }

    fn tick_blocks(&mut self, host: &mut dyn ServerHost) {
        if self.blocks.tasks() == 0 {
            return;
        }
        let mut out = Vec::new();
        let lines = {
            let mut world = HostWorld { host: &mut *host };
            self.blocks.tick(&mut world, self.tick, &mut out)
        };
        for line in lines {
            log::info!("{line}");
        }
        self.run_emits(host, out);
    }

    /// Stores the player's inventory and empties it; it returns in `ticks` ticks.
    fn take_inventory(&mut self, host: &mut dyn ServerHost, name: &str, ticks: u64) {
        let key = name.to_ascii_lowercase();
        if self.escrow.contains_key(&key) {
            return;
        }
        let Some(player) = host.players().player_by_name(name) else { return };
        let Some(slots) = player.inventory_slots(&*host) else { return };
        for (slot, _) in &slots {
            let _ = player.set_inventory_slot(&mut *host, *slot, None);
        }
        self.escrow.insert(key, (self.tick + ticks, slots));
    }

    /// Returns `name`'s inventory if they are online; what does not fit in its slot is handed over.
    fn return_inventory(&mut self, host: &mut dyn ServerHost, name: &str) -> bool {
        let key = name.to_ascii_lowercase();
        let Some(player) = host.players().player_by_name(name) else { return false };
        let Some((_, slots)) = self.escrow.remove(&key) else { return true };
        let occupied: std::collections::HashSet<u32> =
            player.inventory_slots(&*host).unwrap_or_default().into_iter().map(|(slot, _)| slot).collect();
        for (slot, stack) in slots {
            if occupied.contains(&slot) {
                let _ = player.give_stack(&mut *host, stack);
            } else if player.set_inventory_slot(&mut *host, slot, Some(stack.clone())).is_err() {
                let _ = player.give_stack(&mut *host, stack);
            }
        }
        true
    }

    fn tick_escrow(&mut self, host: &mut dyn ServerHost) {
        let due: Vec<String> = self.escrow.iter().filter(|(_, (at, _))| self.tick >= *at).map(|(name, _)| name.clone()).collect();
        for name in due {
            // If the player is not there, the inventory waits for them to come back (`player_logged_in`).
            self.return_inventory(host, &name);
        }
    }

    /// A player joins: if their inventory was in custody, it is given back.
    pub fn on_player_joined(&mut self, host: &mut dyn ServerHost, name: &str) {
        if self.escrow.contains_key(&name.to_ascii_lowercase()) {
            self.return_inventory(host, name);
        }
    }

    /// Gives back to the world everything the actions changed (called when the server stops).
    pub fn restore_world(&mut self, host: &mut dyn ServerHost) {
        let names: Vec<String> = self.escrow.keys().cloned().collect();
        for name in names {
            self.return_inventory(host, &name);
        }
        let mut world = HostWorld { host: &mut *host };
        self.blocks.restore_all(&mut world);
    }

    fn tick_functions(&mut self, host: &mut dyn ServerHost) {
        if self.functions.active() == 0 {
            return;
        }
        let online: HashSet<String> = host
            .players()
            .server_players()
            .into_iter()
            .filter_map(|p| p.name(&*host))
            .map(|n| n.to_ascii_lowercase())
            .collect();
        let commands = self.functions.tick(
            self.tick,
            now_ms(),
            self.config.max_commands_per_tick as usize,
            self.config.max_concurrent_functions as usize,
            &online,
        );
        for command in commands {
            self.run_command(host, &command);
        }
    }

    /// QA: runs an action on a dummy player, with the real world. It does not go through rules or
    /// `execute-live-actions`; `/tiktok test blocks` uses it to test with nobody connected.
    pub fn run_dummy_action(&mut self, host: &mut dyn ServerHost, spec: &crate::rules::model::ActionSpec, dummy: Target) -> crate::actions::Outcome {
        let event = Event::new("test");
        let players = vec![dummy];
        let mut limiter = std::mem::replace(&mut self.limiter, RateLimiter::new(0));
        let (outcome, emits) = {
            let mut world = HostWorld { host: &mut *host };
            let mut cx = Cx::new(&self.config, &event, &players, now_ms(), &mut limiter).with_world(&mut world, &mut self.blocks, self.tick);
            let outcome = actions::execute(spec, &mut cx);
            (outcome, std::mem::take(&mut cx.out))
        };
        self.limiter = limiter;
        self.run_emits(host, emits);
        outcome
    }

    pub fn request_reload(&mut self) {
        self.reload_pending = true;
    }
}

impl Default for Hub {
    fn default() -> Self {
        Hub::new()
    }
}

/// The players connected right now, with their position and dimension.
pub fn snapshot_players(host: &mut dyn ServerHost) -> Vec<Target> {
    let mut out = Vec::new();
    for player in host.players().server_players() {
        let Some(name) = player.name(&*host) else { continue };
        let pos = player.entity().position(&*host).unwrap_or([0.0, 0.0, 0.0]);
        let dim = player.dimension(&*host).map(|d| d.to_string()).unwrap_or_else(|| "minecraft:overworld".to_owned());
        out.push(Target { name, uuid: uuid_string(player.uuid()), pos, dim });
    }
    out
}

/// Writes the missing default files (rules, functions, example) into `config/tiktok_live/`.
fn install_defaults(host: &mut dyn ServerHost) {
    for (relative, content) in crate::defaults::FILES {
        let Ok(path) = ConfigPath::new(&format!("tiktok_live/{relative}")) else { continue };
        // The guide and the schemas belong to the mod, not to the operator: they are refreshed when they change.
        let managed = *relative == "AGENTS.md" || relative.starts_with("schemas/");
        if managed {
            if host.config_files().read(&path).ok().as_deref() != Some(content.as_bytes()) {
                if let Err(error) = host.config_files().write(&path, content.as_bytes()) {
                    log::warn!("Could not refresh {relative}: {error:?}");
                }
            }
        } else if matches!(host.config_files().stat(&path), Ok(None)) {
            match host.config_files().write(&path, content.as_bytes()) {
                Ok(()) => log::info!("Installed default {relative}"),
                Err(error) => log::warn!("Could not install default {relative}: {error:?}"),
            }
        }
    }
}

pub fn uuid_string(id: u128) -> String {
    let hex = format!("{id:032x}");
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

pub fn read_text(host: &mut dyn ServerHost, path: &str) -> Result<String, String> {
    let path = ConfigPath::new(path).map_err(|e| format!("{e:?}"))?;
    let bytes = host.config_files().read(&path).map_err(|e| format!("{e:?}"))?;
    String::from_utf8(bytes).map_err(|_| "the file is not UTF-8".to_owned())
}

/// Names and data of the files in a folder with the given extension, in order.
fn list_files(host: &mut dyn ServerHost, folder: &str, extension: &str) -> Vec<(String, u64, u64)> {
    let Ok(folder_path) = ConfigPath::new(folder) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = match host.config_files().list(&folder_path, after.as_deref(), 256) {
            Ok(page) => page,
            Err(_) => break,
        };
        if page.is_empty() {
            break;
        }
        after = page.last().map(|e| e.name.clone());
        for entry in page {
            if matches!(entry.stat.kind, ConfigEntryKind::File) && entry.name.ends_with(extension) {
                out.push((entry.name, entry.stat.len, entry.stat.modified_millis));
            }
        }
    }
    out.sort();
    out
}

fn file_signature(host: &mut dyn ServerHost) -> String {
    let mut parts: Vec<String> = list_files(host, EVENTS_DIR, ".yml")
        .into_iter()
        .map(|(name, len, modified)| format!("e/{name}:{len}:{modified}"))
        .collect();
    for ext in [".json", ".mcfunction"] {
        parts.extend(list_tree(host, FUNCTIONS_DIR, ext, 0).into_iter().map(|(name, len, modified)| format!("f/{name}:{len}:{modified}")));
    }
    parts.extend(list_tree(host, SCRIPTS_DIR, ".ttl", 0).into_iter().map(|(name, len, modified)| format!("s/{name}:{len}:{modified}")));
    parts.join("|")
}

/// Files with the given extension under `folder`, recursively (maximum depth 6), as
/// relative paths with `/`.
fn list_tree(host: &mut dyn ServerHost, folder: &str, extension: &str, depth: usize) -> Vec<(String, u64, u64)> {
    fn walk(host: &mut dyn ServerHost, root: &str, relative: &str, extension: &str, depth: usize, out: &mut Vec<(String, u64, u64)>) {
        if depth > 6 {
            return;
        }
        let path = if relative.is_empty() { root.to_owned() } else { format!("{root}/{relative}") };
        let Ok(folder_path) = ConfigPath::new(&path) else { return };
        let mut after: Option<String> = None;
        loop {
            let Ok(page) = host.config_files().list(&folder_path, after.as_deref(), 256) else { return };
            if page.is_empty() {
                return;
            }
            after = page.last().map(|e| e.name.clone());
            for entry in page {
                let child = if relative.is_empty() { entry.name.clone() } else { format!("{relative}/{}", entry.name) };
                match entry.stat.kind {
                    ConfigEntryKind::Folder => walk(host, root, &child, extension, depth + 1, out),
                    _ if entry.name.to_ascii_lowercase().ends_with(extension) => out.push((child, entry.stat.len, entry.stat.modified_millis)),
                    _ => {}
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(host, folder, "", extension, depth, &mut out);
    out.sort();
    out
}

fn safe_function_file(name: &str) -> bool {
    let file = name.rsplit('/').next().unwrap_or(name);
    if file.is_empty() || file.len() > 128 || file.starts_with('.') || file.contains("..") {
        return false;
    }
    let lower = file.to_ascii_lowercase();
    (lower.ends_with(".json") || lower.ends_with(".mcfunction"))
        && file.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// `gifts/spawn_zombies.json` -> `gifts:spawn_zombies`; a file at the root is `minecraft:<name>`.
fn function_id_for(relative: &str) -> String {
    let without_ext = relative.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(relative).to_ascii_lowercase();
    match without_ext.split_once('/') {
        None => format!("minecraft:{without_ext}"),
        Some((namespace, path)) => format!("{namespace}:{path}"),
    }
}

fn read_functions(host: &mut dyn ServerHost, config: &Config) -> (std::collections::BTreeMap<String, Definition>, Vec<String>) {
    let limits = FnLimits::from_config(config);
    let mut registry = std::collections::BTreeMap::new();
    let mut problems = Vec::new();
    let mut files = list_tree(host, FUNCTIONS_DIR, ".json", 0);
    files.extend(list_tree(host, FUNCTIONS_DIR, ".mcfunction", 0));
    files.sort();
    for (relative, len, _) in files {
        if !safe_function_file(&relative) {
            continue;
        }
        let id = function_id_for(&relative);
        let result = (|| -> Result<Definition, String> {
            if len as usize > limits.max_file_bytes {
                return Err(format!("File too large: {len} bytes (FILE_TOO_LARGE)"));
            }
            let text = read_text(host, &format!("{FUNCTIONS_DIR}/{relative}"))?;
            let mut definition = if relative.to_ascii_lowercase().ends_with(".mcfunction") {
                fparse::parse_mcfunction(&id, &text, &relative)
            } else {
                fparse::parse_json(&id, &text, &relative)
            }
            .map_err(|e| e.to_string())?;
            // The path rules: if the file declares another id, it is relocated (like the plugin).
            definition.id = id.clone();
            fcompile::validate(&definition, &limits).map_err(|e| e.to_string())?;
            Ok(definition)
        })();
        match result {
            Ok(definition) => {
                if registry.insert(id.clone(), definition).is_some() {
                    problems.push(format!("function {id}: Duplicate function id: {id} (DUPLICATE_FUNCTION)"));
                }
            }
            Err(error) => problems.push(format!("function {id}: {error}")),
        }
    }
    (registry, problems)
}

fn read_rules(host: &mut dyn ServerHost, config: &Config, functions: &HashSet<String>) -> (Vec<crate::rules::model::EventRule>, Vec<String>) {
    let catalog = Catalog { config, functions: Some(functions) };
    let mut rules = Vec::new();
    let mut problems = Vec::new();
    let mut ids: HashMap<String, String> = HashMap::new();
    for (name, _, _) in list_files(host, EVENTS_DIR, ".yml") {
        match read_text(host, &format!("{EVENTS_DIR}/{name}")) {
            Ok(text) => match loader::load_rules(&text, &name, &catalog) {
                Ok(loaded) => {
                    for rule in loaded {
                        if let Some(first) = ids.insert(rule.id.clone(), name.clone()) {
                            problems.push(format!("rule file {name}: duplicate rule id '{}' (also in {first})", rule.id));
                        } else {
                            rules.push(rule);
                        }
                    }
                }
                Err(error) => problems.push(format!("rule file {name}: {error}")),
            },
            Err(error) => problems.push(format!("rule file {name}: The file could not be read: {error}")),
        }
    }
    for (name, _, _) in list_files(host, SCRIPTS_DIR, ".ttl") {
        match read_text(host, &format!("{SCRIPTS_DIR}/{name}")) {
            Ok(text) => match crate::dsl::compile(&name, &text, &catalog) {
                Ok(compiled) => {
                    for rule in compiled {
                        if let Some(first) = ids.insert(rule.id.clone(), name.clone()) {
                            problems.push(format!("script {name}: duplicate rule id '{}' (also in {first})", rule.id));
                        } else {
                            rules.push(rule);
                        }
                    }
                }
                Err(error) => problems.push(format!("script {name}: {error}")),
            },
            Err(error) => problems.push(format!("script {name}: The file could not be read: {error}")),
        }
    }
    (rules, problems)
}
