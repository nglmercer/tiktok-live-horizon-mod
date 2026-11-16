//! A rule's actions. Each action is a pure function: it reads a snapshot of the world
//! (the connected players) and emits commands and messages; the hub runs them with the host.
//! All text that comes from TikTok is sanitized before it reaches a command.
use std::collections::{BTreeMap, HashSet};

use crate::blocks::{BlockManager, CageParams, Started, World};
use crate::config::{Config, NoPlayerPolicy, TargetPolicy};
use crate::event::{safe, truncate, Event};
use crate::rules::fields;
use crate::rules::loader::ActionCatalog;
use crate::rules::model::ActionSpec;
use crate::value::Val;

/// A connected player, as they were when the event was received.
#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub uuid: String,
    pub pos: [f64; 3],
    pub dim: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Ok(String),
    Skipped(String),
    Failed(String),
}

impl Outcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, Outcome::Ok(_))
    }
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn message(&self) -> &str {
        match self {
            Outcome::Ok(m) | Outcome::Skipped(m) | Outcome::Failed(m) => m,
        }
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Ok(m) => write!(f, "OK: {m}"),
            Outcome::Skipped(m) => write!(f, "SKIPPED: {m}"),
            Outcome::Failed(m) => write!(f, "FAILED: {m}"),
        }
    }
}

/// What an action asks of the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum Emit {
    /// A console command.
    Command(String),
    /// A message to all players.
    Broadcast(String),
    /// Starts a function (`target`: the player it runs on, if any).
    Function { name: String, args: BTreeMap<String, Val>, target: Option<String> },
    /// Stores the player's inventory, empties it and gives it back after `ticks` ticks.
    Escrow { player: String, ticks: u64 },
    /// A command in `ticks` ticks.
    Later { ticks: u64, command: String },
    /// Commands every `interval` ticks for `duration` ticks. With `key`, another emission
    /// with the same key replaces the previous one instead of stacking.
    Every { key: Option<String>, interval: u64, duration: u64, commands: Vec<String> },
}

/// Entity limit per minute, shared by all events.
pub struct RateLimiter {
    limit: u32,
    used: u32,
    window_start: i64,
}

impl RateLimiter {
    pub fn new(limit: i64) -> RateLimiter {
        RateLimiter { limit: limit.max(0) as u32, used: 0, window_start: 0 }
    }

    pub fn set_limit(&mut self, limit: i64) {
        self.limit = limit.max(0) as u32;
    }

    pub fn try_acquire(&mut self, now_ms: i64) -> bool {
        if self.limit == 0 {
            return false;
        }
        if now_ms - self.window_start >= 60_000 {
            self.window_start = now_ms;
            self.used = 0;
        }
        if self.used >= self.limit {
            return false;
        }
        self.used += 1;
        true
    }

    pub fn used(&self) -> u32 {
        self.used
    }

    pub fn limit(&self) -> u32 {
        self.limit
    }
}

pub struct Cx<'a> {
    pub config: &'a Config,
    pub event: &'a Event,
    pub players: &'a [Target],
    pub now_ms: i64,
    pub limiter: &'a mut RateLimiter,
    pub out: Vec<Emit>,
    /// The world and the restoration manager, for the actions that change blocks.
    pub world: Option<&'a mut dyn World>,
    pub blocks: Option<&'a mut BlockManager>,
    pub tick: u64,
    /// Players (lower-case) whose inventory is already held in custody.
    pub locked: Vec<String>,
    actions_run: i64,
    entities_this_event: i64,
}

impl<'a> Cx<'a> {
    pub fn new(config: &'a Config, event: &'a Event, players: &'a [Target], now_ms: i64, limiter: &'a mut RateLimiter) -> Cx<'a> {
        Cx { config, event, players, now_ms, limiter, out: Vec::new(), world: None, blocks: None, tick: 0, locked: Vec::new(), actions_run: 0, entities_this_event: 0 }
    }

    pub fn with_world(mut self, world: &'a mut dyn World, blocks: &'a mut BlockManager, tick: u64) -> Cx<'a> {
        self.world = Some(world);
        self.blocks = Some(blocks);
        self.tick = tick;
        self
    }

    pub fn consume_action_slot(&mut self) -> bool {
        if self.actions_run >= self.config.max_actions_per_event {
            return false;
        }
        self.actions_run += 1;
        true
    }

    pub fn consume_entity_slot(&mut self) -> bool {
        if self.entities_this_event >= self.config.max_entities_per_event {
            return false;
        }
        if !self.limiter.try_acquire(self.now_ms) {
            return false;
        }
        self.entities_this_event += 1;
        true
    }

    fn connection_target(&self) -> Option<String> {
        match self.event.payload.get("connection.targetPlayer") {
            Some(v) if !v.text().trim().is_empty() => Some(format!("specific-player:{}", v.text().trim())),
            _ => None,
        }
    }

    fn target_spec(&self, spec: Option<&str>) -> Option<String> {
        let connection = self.connection_target();
        match (&connection, spec) {
            (Some(c), None) => Some(c.clone()),
            (Some(c), Some(s)) if s.trim().is_empty() || s.trim().eq_ignore_ascii_case("nearest-online") => Some(c.clone()),
            _ => spec.map(str::to_owned),
        }
    }

    fn origin(&self) -> Option<[f64; 3]> {
        let spec = self.connection_target();
        resolve_targets(spec.as_deref(), self.config.target_policy, None, self.players, self.now_ms)
            .first()
            .map(|t| t.pos)
    }

    pub fn targets(&self, spec: Option<&str>) -> Vec<Target> {
        let spec = self.target_spec(spec);
        resolve_targets(spec.as_deref(), self.config.target_policy, self.origin(), self.players, self.now_ms)
    }

    fn repeat_count(&self) -> i64 {
        self.event.number("gift.repeatCount").max(1)
    }
}

fn pick_random(players: &[Target], seed: i64) -> Vec<Target> {
    if players.is_empty() {
        return Vec::new();
    }
    let mixed = (seed as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    vec![players[(mixed >> 33) as usize % players.len()].clone()]
}

fn nearest(origin: Option<[f64; 3]>, players: &[Target]) -> Vec<Target> {
    let Some(first) = players.first() else {
        return Vec::new();
    };
    let Some(origin) = origin else {
        return vec![first.clone()];
    };
    let distance = |t: &Target| {
        let d = [t.pos[0] - origin[0], t.pos[1] - origin[1], t.pos[2] - origin[2]];
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
    };
    let mut best = first;
    for player in players {
        if distance(player) < distance(best) {
            best = player;
        }
    }
    vec![best.clone()]
}

fn resolve_default(policy: TargetPolicy, origin: Option<[f64; 3]>, players: &[Target], seed: i64) -> Vec<Target> {
    match policy {
        TargetPolicy::AllOnline => players.to_vec(),
        TargetPolicy::RandomOnline => pick_random(players, seed),
        TargetPolicy::NearestOnline => nearest(origin, players),
        TargetPolicy::SpecificPlayer | TargetPolicy::SpecificWorld => Vec::new(),
    }
}

fn dimension_matches(dim: &str, wanted: &str) -> bool {
    let wanted = wanted.trim().to_ascii_lowercase();
    let dim = dim.to_ascii_lowercase();
    if dim == wanted || dim.strip_prefix("minecraft:") == Some(wanted.as_str()) {
        return true;
    }
    matches!(
        (wanted.as_str(), dim.as_str()),
        ("world", "minecraft:overworld") | ("world_nether", "minecraft:the_nether") | ("world_the_end", "minecraft:the_end")
    )
}

pub fn resolve_targets(
    spec: Option<&str>,
    policy: TargetPolicy,
    origin: Option<[f64; 3]>,
    players: &[Target],
    seed: i64,
) -> Vec<Target> {
    let normalized = spec.map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();
    if normalized.is_empty() {
        return resolve_default(policy, origin, players, seed);
    }
    let (kind, argument) = match normalized.split_once(':') {
        Some((k, a)) => (k, a),
        None => (normalized.as_str(), ""),
    };
    match kind {
        "all-online" => players.to_vec(),
        "random-online" => pick_random(players, seed),
        "nearest-online" => nearest(origin, players),
        "specific-player" => players
            .iter()
            .filter(|p| p.name.to_ascii_lowercase() == argument || p.uuid.to_ascii_lowercase() == argument)
            .take(1)
            .cloned()
            .collect(),
        "specific-world" => players.iter().filter(|p| dimension_matches(&p.dim, argument)).cloned().collect(),
        _ => resolve_default(policy, origin, players, seed),
    }
}

fn valid_player_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `namespace:path` or `path`, Minecraft identifier characters only.
fn valid_id(id: &str) -> bool {
    let (ns, path) = match id.split_once(':') {
        Some((n, p)) => (Some(n), p),
        None => (None, id),
    };
    let ok_ns = ns.is_none_or(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.')));
    let ok_path = !path.is_empty()
        && path.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '/'));
    ok_ns && ok_path && id.len() <= 128
}

fn qualify(id: &str) -> String {
    let lower = id.trim().to_ascii_lowercase();
    if lower.contains(':') { lower } else { format!("minecraft:{lower}") }
}

/// Legacy Bukkit names that differ from Minecraft ids.
fn effect_id(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    let short = lower.strip_prefix("minecraft:").unwrap_or(&lower);
    let mapped = match short {
        "increase_damage" => "strength",
        "slow" => "slowness",
        "fast_digging" => "haste",
        "slow_digging" => "mining_fatigue",
        "jump" => "jump_boost",
        "confusion" => "nausea",
        "damage_resistance" => "resistance",
        "heal" => "instant_health",
        "harm" => "instant_damage",
        other => other,
    };
    if lower.contains(':') && !lower.starts_with("minecraft:") { lower } else { format!("minecraft:{mapped}") }
}

const COLORS: [&str; 16] = [
    "black", "dark_blue", "dark_green", "dark_aqua", "dark_red", "dark_purple", "gold", "gray", "dark_gray", "blue", "green", "aqua", "red",
    "light_purple", "yellow", "white",
];

/// A text color: a Minecraft name (`gold`) or `#RRGGBB`.
fn valid_color(color: &str) -> bool {
    let lower = color.trim().to_ascii_lowercase();
    COLORS.contains(&lower.as_str())
        || lower.strip_prefix('#').is_some_and(|hex| hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A text component with optional color and bold.
fn styled_text(text: &str, color: &str, bold: bool) -> String {
    let mut component = serde_json::json!({ "text": text });
    if !color.trim().is_empty() {
        component["color"] = serde_json::json!(color.trim().to_ascii_lowercase());
    }
    if bold {
        component["bold"] = serde_json::json!(true);
    }
    component.to_string()
}

fn skipped_no_target() -> Outcome {
    Outcome::Skipped("No eligible target".to_owned())
}

// --- validation and execution, one pair per type ------------------------------------------

pub const TYPES: [&str; 21] = [
    "broadcast",
    "title",
    "sound",
    "particle",
    "spawn-entity",
    "give-item",
    "effect",
    "heal",
    "teleport",
    "command",
    "prime-tnt",
    "remove-item",
    "lightning",
    "lightning-effect",
    "function",
    "block-cage",
    "tnt-cage",
    "void-shaft",
    "chunk-replace",
    "place-cobweb",
    "temporary-clear-inventory",
];

fn in_range(name: &str, value: i64, min: i64, max: i64) -> Result<(), String> {
    if value < min || value > max {
        Err(format!("'{name}' must be between {min} and {max}"))
    } else {
        Ok(())
    }
}

pub fn validate(spec: &ActionSpec, config: &Config, functions: Option<&HashSet<String>>) -> Result<(), String> {
    match spec.kind.as_str() {
        "broadcast" => {
            let message = spec.require("message")?;
            if message.chars().count() > 512 {
                return Err("'message' must be at most 512 characters".into());
            }
        }
        "title" => {
            spec.require("title")?;
            if spec.optional("subtitle").unwrap_or_default().chars().count() > 512 {
                return Err("'subtitle' must be at most 512 characters".into());
            }
            for key in ["color", "subtitle-color"] {
                if spec.optional(key).is_some_and(|c| !valid_color(&c)) {
                    return Err(format!("'{key}' must be a Minecraft color name (gold, red, ...) or #RRGGBB"));
                }
            }
            if spec.param("bold").is_some_and(|v| !matches!(v, Val::Bool(_))) {
                return Err("bold must be a boolean".into());
            }
            let (fade_in, stay, fade_out) = (spec.int("fade-in", 10)?, spec.int("stay", 60)?, spec.int("fade-out", 20)?);
            if !(0..=1200).contains(&fade_in) || !(0..=12000).contains(&stay) || !(0..=1200).contains(&fade_out) {
                return Err("title timings must be between 0 and 1200 (stay up to 12000) ticks".into());
            }
        }
        "sound" => {
            let sound = spec.require("sound")?;
            if !valid_sound(&sound) {
                return Err("'sound' is not a valid sound name".into());
            }
            let (volume, pitch) = (spec.float("volume", 1.0)?, spec.float("pitch", 1.0)?);
            if !(0.0..=16.0).contains(&volume) {
                return Err("'volume' must be between 0 and 16".into());
            }
            if !(0.0..=2.0).contains(&pitch) {
                return Err("'pitch' must be between 0 and 2".into());
            }
        }
        "particle" => {
            let particle = spec.require("particle")?;
            if !valid_id(&particle.to_ascii_lowercase()) {
                return Err("'particle' is not a valid particle name".into());
            }
            in_range("count", spec.int("count", 20)?, 1, 10_000)?;
        }
        "spawn-entity" => {
            let entity = spec.require("entity")?;
            if !valid_id(&entity.to_ascii_lowercase()) {
                return Err("'entity' is not a valid entity type".into());
            }
            let count = spec.int("count", 1)?;
            if count < 1 {
                return Err("'count' must be at least 1".into());
            }
            if count > 30_000 {
                return Err("'count' must not exceed 30000".into());
            }
            let limit = spec.int("max-effective-count", 128)?;
            if !(1..=128).contains(&limit) {
                return Err("max-effective-count must be 1..128".into());
            }
            if spec.param("charged").is_some_and(|v| !matches!(v, Val::Bool(_))) {
                return Err("charged must be a boolean".into());
            }
            if spec.param("name-visible").is_some_and(|v| !matches!(v, Val::Bool(_))) {
                return Err("name-visible must be a boolean".into());
            }
            if spec.optional("name").unwrap_or_default().chars().count() > 256 {
                return Err("'name' must be at most 256 characters".into());
            }
        }
        "give-item" | "remove-item" => {
            let material = spec.require("material")?;
            if !valid_id(&material.to_ascii_lowercase()) || qualify(&material).ends_with(":air") {
                return Err(format!("Unknown material: {material}"));
            }
            in_range("count", spec.int("count", 1)?, 1, 64)?;
        }
        "effect" => {
            let effect = spec.require("effect")?;
            if !valid_sound(&effect) {
                return Err("'effect' is not a valid potion effect name".into());
            }
            in_range("duration", spec.int("duration", 5)?, 1, 3600)?;
            in_range("amplifier", spec.int("amplifier", 0)?, 0, 255)?;
        }
        "heal" => {
            let amount = spec.float("amount", 4.0)?;
            if amount <= 0.0 || amount > 1000.0 {
                return Err("'amount' must be greater than 0 and at most 1000".into());
            }
        }
        "teleport" => {
            let destination = spec.require("destination")?;
            if parse_destination(&destination).is_none() {
                return Err("'destination' must be 'x,y,z' or 'player:<name>'".into());
            }
        }
        "command" => {
            let command = spec.require("command")?;
            if command.chars().count() > 256 {
                return Err("'command' must be at most 256 characters".into());
            }
            if command.contains('\n') || command.contains('\r') || command.contains(';') {
                return Err("'command' must not contain line breaks or ';'".into());
            }
            if command.starts_with('/') {
                return Err("'command' must not start with '/'".into());
            }
            if !config.allow_raw_commands {
                return Err("Raw commands are disabled (security.allow-raw-commands)".into());
            }
            if !config.allow_console_commands {
                return Err("Console commands are disabled (security.allow-console-commands)".into());
            }
        }
        "prime-tnt" => {
            let (count, fuse, burst) = (spec.int("count", 1)?, spec.int("fuse-ticks", 80)?, spec.int("burst-size", 6)?);
            if !(1..=30_000).contains(&count) {
                return Err("count must be 1..30000".into());
            }
            if !(20..=240).contains(&fuse) {
                return Err("fuse-ticks must be 20..240".into());
            }
            if !(1..=12).contains(&burst) {
                return Err("burst-size must be 1..12".into());
            }
        }
        "lightning" | "lightning-effect" => {
            let default_seconds = if spec.kind == "lightning" { 60 } else { 0 };
            let (count, seconds, interval) =
                (spec.int("count", 1)?, spec.int("duration-seconds", default_seconds)?, spec.int("interval-ticks", 20)?);
            if !(1..=32).contains(&count) {
                return Err("count must be 1..32".into());
            }
            if !(0..=3600).contains(&seconds) {
                return Err("duration-seconds must be 0..3600".into());
            }
            if !(10..=1200).contains(&interval) {
                return Err("interval-ticks must be 10..1200".into());
            }
            for key in ["damage", "multiply-by-repeat"] {
                if spec.param(key).is_some_and(|v| !matches!(v, Val::Bool(_))) {
                    return Err(format!("{key} must be a boolean"));
                }
            }
        }
        "block-cage" | "tnt-cage" => validate_cage(spec)?,
        "void-shaft" => {
            in_range("radius", spec.int("radius", 1)?, 0, 1)?;
            in_range("blocks-per-tick", spec.int("blocks-per-tick", 96)?, 1, 128)?;
            in_range("max-blocks", spec.int("max-blocks", 2048)?, 1, 4096)?;
            in_range("restore-seconds", spec.int("restore-seconds", 30)?, 0, 180)?;
        }
        "chunk-replace" => {
            let material = spec.require("material")?;
            block_material(&material, "material", true, true)?;
            in_range("radius-chunks", spec.int("radius-chunks", 0)?, 0, 1)?;
            in_range("blocks-per-tick", spec.int("blocks-per-tick", 512)?, 1, 4096)?;
            if spec.int("max-blocks", 0)? < 0 {
                return Err("max-blocks must be 0 or more (0 is unlimited)".to_owned());
            }
            in_range("restore-seconds", spec.int("restore-seconds", 0)?, 0, 180)?;
            let (min_y, max_y) = (optional_y(spec, "min-y")?, optional_y(spec, "max-y")?);
            if let (Some(min), Some(max)) = (min_y, max_y) {
                if min > max {
                    return Err("min-y must not be above max-y".to_owned());
                }
            }
            if let Some(from) = spec.optional("from") {
                block_material(&from, "from", true, true)?;
            }
        }
        "temporary-clear-inventory" => in_range("restore-seconds", spec.int("restore-seconds", 8)?, 1, 30)?,
        "place-cobweb" => {
            in_range("count", spec.int("count", 6)?, 1, 32)?;
            in_range("restore-seconds", spec.int("restore-seconds", 12)?, 1, 120)?;
        }
        "function" => {
            let name = spec.require("name")?;
            if !crate::functions::valid_function_id(&name) {
                return Err(format!("'{name}' is not a valid function id"));
            }
            if let Some(known) = functions {
                if !known.contains(&name) {
                    return Err(format!("Unknown function: {name}"));
                }
            }
            if spec.param("args").is_some_and(|v| !matches!(v, Val::Map(_))) {
                return Err("'args' must be a mapping of argument names to values".to_owned());
            }
        }
        other => return Err(format!("unknown action type '{other}'")),
    }
    Ok(())
}

fn optional_y(spec: &ActionSpec, name: &str) -> Result<Option<i64>, String> {
    match spec.param(name) {
        None => Ok(None),
        Some(_) => spec.int(name, 0).map(Some).map_err(|_| format!("'{name}' must be a whole number")),
    }
}

/// A placeable vanilla block (`BlockMaterials.resolve`): `minecraft:` only, and no air or fluids
/// unless allowed.
fn block_material(name: &str, param: &str, allow_air: bool, allow_fluids: bool) -> Result<String, String> {
    let lower = name.trim().to_ascii_lowercase();
    if !valid_id(&lower) {
        return Err(format!("Invalid vanilla block material for '{param}': {name}"));
    }
    let id = qualify(&lower);
    if !id.starts_with("minecraft:") {
        return Err(format!("Invalid vanilla block material for '{param}': {name}"));
    }
    if !allow_air && matches!(id.as_str(), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air") {
        return Err(format!("Material must not be air for '{param}': {name}"));
    }
    if !allow_fluids && matches!(id.as_str(), "minecraft:water" | "minecraft:lava") {
        return Err(format!("Material is not a placeable solid block for '{param}': {name}"));
    }
    Ok(id)
}

fn validate_cage(spec: &ActionSpec) -> Result<(), String> {
    let name = spec.optional("material").or_else(|| (spec.kind == "tnt-cage").then(|| "minecraft:tnt".to_owned()));
    let Some(name) = name else {
        return Err("material is required for block-cage".to_owned());
    };
    let material = block_material(&name, "material", false, false)?;
    in_range("radius", spec.int("radius", 2)?, 1, 3)?;
    in_range("height", spec.int("height", 3)?, 3, 6)?;
    in_range("restore-seconds", spec.int("restore-seconds", 20)?, 0, 120)?;
    in_range("fuse-ticks", spec.int("fuse-ticks", 80)?, 20, 240)?;
    in_range("ignite-delay-ticks", spec.int("ignite-delay-ticks", 40)?, 1, 200)?;
    for key in ["ignite", "occupant-charged"] {
        if spec.param(key).is_some_and(|v| !matches!(v, Val::Bool(_))) {
            return Err(format!("{key} must be a boolean"));
        }
    }
    if matches!(spec.param("ignite"), Some(Val::Bool(true))) && material != "minecraft:tnt" {
        return Err("ignite: true is only supported with minecraft:tnt".to_owned());
    }
    let charged = matches!(spec.param("occupant-charged"), Some(Val::Bool(true)));
    match spec.optional("occupant") {
        None if charged => return Err("occupant-charged requires occupant: minecraft:creeper".to_owned()),
        None => {}
        Some(occupant) => {
            if !valid_id(&occupant.to_ascii_lowercase()) {
                return Err(format!("Invalid occupant entity key: {occupant}"));
            }
            if charged && qualify(&occupant) != "minecraft:creeper" {
                return Err("Only a creeper can be charged".to_owned());
            }
        }
    }
    Ok(())
}

fn valid_sound(name: &str) -> bool {
    (1..=128).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-'))
}

enum Destination {
    Coordinates([f64; 3]),
    Player(String),
}

fn parse_destination(text: &str) -> Option<Destination> {
    let text = text.trim();
    if let Some(name) = text.to_ascii_lowercase().strip_prefix("player:").map(str::to_owned) {
        let original = &text["player:".len()..];
        return if name.is_empty() || !valid_player_name(original) { None } else { Some(Destination::Player(original.to_owned())) };
    }
    let parts: Vec<&str> = text.split(',').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut numbers = [0.0; 3];
    for (slot, part) in numbers.iter_mut().zip(&parts) {
        let value = part.trim().parse::<f64>().ok()?;
        if !value.is_finite() || value.abs() > 30_000_000.0 {
            return None;
        }
        *slot = value;
    }
    Some(Destination::Coordinates(numbers))
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Runs an action: validates again, resolves targets and emits.
pub fn execute(spec: &ActionSpec, cx: &mut Cx<'_>) -> Outcome {
    let target_param = spec.optional("target");
    match spec.kind.as_str() {
        "broadcast" => {
            let message = safe(&spec.string("message", ""));
            cx.out.push(Emit::Broadcast(message));
            Outcome::Ok(format!("Broadcast to {} player(s)", cx.players.len()))
        }
        "title" => {
            let players = named(cx.targets(target_param.as_deref()));
            if players.is_empty() {
                return skipped_no_target();
            }
            let title = truncate(&safe(&fields::render(&spec.string("title", ""), cx.event)), 256);
            let subtitle = truncate(&safe(&fields::render(&spec.string("subtitle", ""), cx.event)), 512);
            let (fade_in, stay, fade_out) =
                (spec.int("fade-in", 10).unwrap_or(10), spec.int("stay", 60).unwrap_or(60), spec.int("fade-out", 20).unwrap_or(20));
            let color = spec.string("color", "");
            let subtitle_color = spec.string("subtitle-color", &color);
            let bold = matches!(spec.param("bold"), Some(Val::Bool(true)));
            for player in &players {
                cx.out.push(Emit::Command(format!("title {} times {fade_in} {stay} {fade_out}", player.name)));
                cx.out.push(Emit::Command(format!("title {} subtitle {}", player.name, styled_text(&subtitle, &subtitle_color, false))));
                cx.out.push(Emit::Command(format!("title {} title {}", player.name, styled_text(&title, &color, bold))));
            }
            Outcome::Ok(format!("Title shown to {} player(s)", players.len()))
        }
        "sound" => {
            let players = named(cx.targets(target_param.as_deref()));
            if players.is_empty() {
                return skipped_no_target();
            }
            let sound = spec.string("sound", "");
            let (volume, pitch) = (spec.float("volume", 1.0).unwrap_or(1.0), spec.float("pitch", 1.0).unwrap_or(1.0));
            for player in &players {
                cx.out.push(Emit::Command(format!("playsound {sound} master {} ~ ~ ~ {volume} {pitch}", player.name)));
            }
            Outcome::Ok(format!("Sound played to {} player(s)", players.len()))
        }
        "particle" => {
            let players = named(cx.targets(target_param.as_deref()));
            if players.is_empty() {
                return skipped_no_target();
            }
            let particle = qualify(&spec.string("particle", ""));
            let count = spec.int("count", 20).unwrap_or(20);
            for player in &players {
                cx.out.push(Emit::Command(format!("execute at {} run particle {particle} ~ ~ ~ 0 0 0 0 {count}", player.name)));
            }
            Outcome::Ok(format!("Particles spawned for {} player(s)", players.len()))
        }
        "spawn-entity" => spawn_entity(spec, cx, target_param.as_deref()),
        "give-item" => {
            let players = named(cx.targets(target_param.as_deref()));
            if players.is_empty() {
                return skipped_no_target();
            }
            let material = qualify(&spec.string("material", ""));
            let count = spec.int("count", 1).unwrap_or(1);
            for player in &players {
                cx.out.push(Emit::Command(format!("give {} {material} {count}", player.name)));
            }
            Outcome::Ok(format!("Gave {} item(s)", count * players.len() as i64))
        }
        "effect" => {
            let players = named(cx.targets(target_param.as_deref()));
            if players.is_empty() {
                return skipped_no_target();
            }
            let effect = effect_id(&spec.string("effect", ""));
            let (duration, amplifier) = (spec.int("duration", 5).unwrap_or(5), spec.int("amplifier", 0).unwrap_or(0));
            for player in &players {
                cx.out.push(Emit::Command(format!("effect give {} {effect} {duration} {amplifier} true", player.name)));
            }
            Outcome::Ok(format!("Effect applied to {} player(s)", players.len()))
        }
        "heal" => {
            let players = named(cx.targets(target_param.as_deref()));
            if players.is_empty() {
                return skipped_no_target();
            }
            // Instant health heals 4 * 2^amplifier: the level closest to `amount` is chosen.
            let amount = spec.float("amount", 4.0).unwrap_or(4.0);
            let amplifier = (amount / 4.0).max(1.0).log2().round().clamp(0.0, 8.0) as i64;
            for player in &players {
                cx.out.push(Emit::Command(format!("effect give {} minecraft:instant_health 1 {amplifier} true", player.name)));
            }
            Outcome::Ok(format!("Healed {} player(s)", players.len()))
        }
        "teleport" => teleport(spec, cx, target_param.as_deref()),
        "command" => {
            if !cx.config.allow_raw_commands || !cx.config.allow_console_commands {
                return Outcome::Skipped("Raw commands are disabled".to_owned());
            }
            let command = spec.string("command", "");
            let root = command.split(' ').next().unwrap_or("");
            if !cx.config.command_allowed(root) {
                return Outcome::Skipped("The command root is not in the allowlist".to_owned());
            }
            cx.out.push(Emit::Command(command.clone()));
            Outcome::Ok(format!("Dispatched /{command}"))
        }
        "prime-tnt" => prime_tnt(spec, cx, target_param.as_deref()),
        "remove-item" => {
            let players = named(cx.targets(target_param.as_deref()));
            let Some(player) = players.first() else {
                return Outcome::Skipped("No online target".to_owned());
            };
            let material = qualify(&spec.string("material", ""));
            let mut count = spec.int("count", 1).unwrap_or(1);
            if spec.flag("multiply-by-repeat") {
                count = (count * cx.repeat_count()).min(64);
            }
            cx.out.push(Emit::Command(format!("clear {} {material} {count}", player.name)));
            Outcome::Ok(format!("Removed up to {count} {material}"))
        }
        "lightning" | "lightning-effect" => lightning(spec, cx, target_param.as_deref()),
        "block-cage" | "tnt-cage" => cage(spec, cx, target_param.as_deref()),
        "void-shaft" => void_shaft(spec, cx, target_param.as_deref()),
        "chunk-replace" => chunk_replace(spec, cx, target_param.as_deref()),
        "place-cobweb" => place_cobweb(spec, cx, target_param.as_deref()),
        "temporary-clear-inventory" => {
            let Some(player) = first_target(cx, target_param.as_deref()) else {
                return Outcome::Skipped("No online target".to_owned());
            };
            if cx.locked.iter().any(|n| n.eq_ignore_ascii_case(&player.name)) {
                return Outcome::Skipped("Target is already in temporary inventory lock".to_owned());
            }
            let seconds = spec.int("restore-seconds", 8).unwrap_or(8);
            cx.locked.push(player.name.to_ascii_lowercase());
            cx.out.push(Emit::Escrow { player: player.name.clone(), ticks: seconds as u64 * 20 });
            Outcome::Ok(format!("Temporarily hid the inventory for {seconds} seconds"))
        }
        "function" => {
            let name = spec.string("name", "");
            let args = match spec.param("args") {
                Some(Val::Map(map)) => map.clone(),
                _ => BTreeMap::new(),
            };
            let target = named(cx.targets(None)).first().map(|t| t.name.clone());
            cx.out.push(Emit::Function { name: name.clone(), args, target });
            Outcome::Ok(format!("Started function {name}"))
        }
        other => Outcome::Failed(format!("unknown action type '{other}'")),
    }
}

/// Discards players whose name is not a Minecraft name: it never reaches a command.
fn named(players: Vec<Target>) -> Vec<Target> {
    players.into_iter().filter(|p| valid_player_name(&p.name)).collect()
}

fn spawn_entity(spec: &ActionSpec, cx: &mut Cx<'_>, target: Option<&str>) -> Outcome {
    let name = spec.string("entity", "");
    if !cx.config.entity_allowed(&name) {
        return Outcome::Skipped("The entity type is not in actions.allowed-entities".to_owned());
    }
    let players = named(cx.targets(target));
    if players.is_empty() {
        return skipped_no_target();
    }
    let mut requested = spec.int("count", 1).unwrap_or(1);
    if spec.flag("multiply-by-repeat") {
        requested = (requested * cx.repeat_count()).min(30_000);
    }
    let limit = spec.int("max-effective-count", 128).unwrap_or(128);
    let effective = requested.min(limit).min(cx.config.max_entities_per_event);
    let entity = qualify(&name);
    let charged = matches!(spec.param("charged"), Some(Val::Bool(true))) && entity == "minecraft:creeper";
    // `name` puts a name tag with the text (with {field}, e.g. {nickname}); `name-visible: false` hides it.
    let label = {
        let rendered = safe(&fields::render(&spec.string("name", ""), cx.event));
        let rendered = if cx.config.chat_strip_unsupported { crate::event::displayable(&rendered) } else { rendered };
        truncate(&rendered, 64)
    };
    let mut tags = Vec::new();
    if charged {
        tags.push("powered:1b".to_owned());
    }
    if !label.is_empty() {
        let visible = !matches!(spec.param("name-visible"), Some(Val::Bool(false)));
        tags.push(format!("CustomName:{{text:{}}}", serde_json::to_string(&label).unwrap_or_else(|_| "\"\"".to_owned())));
        tags.push(format!("CustomNameVisible:{}", if visible { "1b" } else { "0b" }));
    }
    let nbt = if tags.is_empty() { String::new() } else { format!(" {{{}}}", tags.join(",")) };
    let mut spawned = 0;
    for player in &players {
        for _ in 0..effective {
            if !cx.consume_entity_slot() {
                return Outcome::Ok(format!("Spawned {spawned} entity/entities before the limit"));
            }
            cx.out.push(Emit::Command(format!("execute at {} run summon {entity} ~ ~1 ~{nbt}", player.name)));
            spawned += 1;
        }
    }
    Outcome::Ok(format!("Spawned {spawned} entity/entities"))
}

fn teleport(spec: &ActionSpec, cx: &mut Cx<'_>, target: Option<&str>) -> Outcome {
    let players = named(cx.targets(target));
    if players.is_empty() {
        return skipped_no_target();
    }
    let Some(destination) = parse_destination(&spec.string("destination", "")) else {
        return Outcome::Failed("The destination is not a coordinate triple".to_owned());
    };
    let mut moved = 0;
    for player in &players {
        match &destination {
            Destination::Coordinates(c) => {
                if distance(player.pos, *c) > 1000.0 {
                    return Outcome::Failed("The destination is too far away".to_owned());
                }
                cx.out.push(Emit::Command(format!("tp {} {} {} {}", player.name, c[0], c[1], c[2])));
            }
            Destination::Player(other) => {
                let Some(found) = cx.players.iter().find(|p| p.name.eq_ignore_ascii_case(other)) else {
                    return Outcome::Skipped("The destination player is not online".to_owned());
                };
                if found.dim == player.dim && distance(player.pos, found.pos) > 1000.0 {
                    return Outcome::Failed("The destination is too far away".to_owned());
                }
                cx.out.push(Emit::Command(format!("tp {} {}", player.name, found.name)));
            }
        }
        moved += 1;
    }
    Outcome::Ok(format!("Teleported {moved} player(s)"))
}

fn prime_tnt(spec: &ActionSpec, cx: &mut Cx<'_>, target: Option<&str>) -> Outcome {
    let mut requested = spec.int("count", 1).unwrap_or(1);
    if spec.flag("multiply-by-repeat") {
        requested = (requested * cx.repeat_count()).min(30_000);
    }
    let allowed = requested.min(128).min(cx.config.max_entities_per_event);
    let players = named(cx.targets(target));
    let Some(player) = players.first() else {
        return Outcome::Skipped("No online target for TNT".to_owned());
    };
    if allowed == 0 {
        return Outcome::Skipped("Entity budget is zero".to_owned());
    }
    let fuse = spec.int("fuse-ticks", 80).unwrap_or(80);
    let burst = spec.int("burst-size", 6).unwrap_or(6).max(1) as u64;
    let mut scheduled = 0i64;
    for index in 0..allowed {
        if !cx.consume_entity_slot() {
            break;
        }
        let theta = (index + 1) as f64 * 2.399963229728653;
        let radius = 2.5 + (index % 3) as f64;
        let (dx, dz) = (theta.cos() * radius, theta.sin() * radius);
        cx.out.push(Emit::Later {
            ticks: 1 + index as u64 / burst,
            command: format!("execute at {} run summon minecraft:tnt ~{dx:.3} ~1 ~{dz:.3} {{fuse:{fuse}}}", player.name),
        });
        scheduled += 1;
    }
    Outcome::Ok(format!("Scheduled up to {scheduled} primed TNT (requested {requested}; bounded bursts and shared per-minute budget)"))
}

fn started(result: Started) -> Outcome {
    match result {
        Started::Ok(m) => Outcome::Ok(m),
        Started::Skipped(m) => Outcome::Skipped(m),
        Started::Failed(m) => Outcome::Failed(m),
    }
}

fn first_target(cx: &Cx<'_>, target: Option<&str>) -> Option<Target> {
    named(cx.targets(target)).into_iter().next()
}

fn cage(spec: &ActionSpec, cx: &mut Cx<'_>, target: Option<&str>) -> Outcome {
    let name = spec.optional("material").unwrap_or_else(|| "minecraft:tnt".to_owned());
    let Ok(material) = block_material(&name, "material", false, false) else {
        return Outcome::Failed("Invalid cage material".to_owned());
    };
    let Some(player) = first_target(cx, target) else {
        return Outcome::Skipped("No eligible player for block cage".to_owned());
    };
    let occupant = match spec.optional("occupant") {
        None => None,
        Some(occupant) => {
            if !cx.config.entity_allowed(&occupant) {
                return Outcome::Skipped("Cage occupant is not in actions.allowed-entities".to_owned());
            }
            Some((qualify(&occupant), matches!(spec.param("occupant-charged"), Some(Val::Bool(true)))))
        }
    };
    let ignite = matches!(spec.param("ignite"), Some(Val::Bool(true)));
    let params = CageParams {
        dim: player.dim.clone(),
        center: player.pos,
        material: material.clone(),
        radius: spec.int("radius", 2).unwrap_or(2) as i32,
        height: spec.int("height", 3).unwrap_or(3) as i32,
        restore_seconds: spec.int("restore-seconds", 20).unwrap_or(20) as u64,
        ignite: ignite.then(|| (spec.int("ignite-delay-ticks", 40).unwrap_or(40) as u64, spec.int("fuse-ticks", 80).unwrap_or(80))),
        occupant,
    };
    // Entity budgets are consumed before touching the world.
    if params.occupant.is_some() && !cx.consume_entity_slot() {
        return Outcome::Skipped("Entity budget unavailable for occupant".to_owned());
    }
    if ignite && !cx.consume_entity_slot() {
        return Outcome::Skipped("Entity budget unavailable for cage ignition".to_owned());
    }
    let tick = cx.tick;
    let (Some(world), Some(blocks)) = (cx.world.as_deref_mut(), cx.blocks.as_deref_mut()) else {
        return Outcome::Failed("The world is not available".to_owned());
    };
    if !world.valid_block(&material) {
        return Outcome::Failed(format!("Unknown block material: {material}"));
    }
    let mut out = Vec::new();
    let result = blocks.cage(world, tick, &params, &mut out);
    cx.out.extend(out);
    match started(result) {
        Outcome::Ok(m) => Outcome::Ok(format!("{m} around {}{}", player.name, params.occupant.as_ref().map(|(e, _)| format!(" with {e} inside")).unwrap_or_default())),
        other => other,
    }
}

fn void_shaft(spec: &ActionSpec, cx: &mut Cx<'_>, target: Option<&str>) -> Outcome {
    let Some(player) = first_target(cx, target) else {
        return Outcome::Skipped("No eligible target for void shaft".to_owned());
    };
    let radius = spec.int("radius", 1).unwrap_or(1) as i32;
    let per_tick = spec.int("blocks-per-tick", 96).unwrap_or(96) as usize;
    let max = spec.int("max-blocks", 2048).unwrap_or(2048);
    let restore_seconds = spec.int("restore-seconds", 30).unwrap_or(30) as u64;
    let Some(world) = cx.world.as_deref() else {
        return Outcome::Failed("The world is not available".to_owned());
    };
    let Some((min_y, _)) = world.height(&player.dim) else {
        return Outcome::Skipped("Target world unavailable".to_owned());
    };
    let (px, py, pz) = (player.pos[0].floor() as i32, player.pos[1].floor() as i32, player.pos[2].floor() as i32);
    let start_y = py - 1;
    if start_y < min_y {
        return Outcome::Skipped("Player already at minimum world height".to_owned());
    }
    let side = i64::from(radius) * 2 + 1;
    let potential = side * side * i64::from(start_y - min_y + 1);
    if potential > max {
        return Outcome::Skipped(format!("Void shaft needs {potential} blocks, max-blocks is {max}"));
    }
    let mut columns = Vec::new();
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let (x, z) = (px + dx, pz + dz);
            if !world.is_loaded(&player.dim, [x, start_y, z]) {
                return Outcome::Skipped("Shaft crosses an unloaded chunk".to_owned());
            }
            columns.push((x, z));
        }
    }
    let Some(blocks) = cx.blocks.as_deref_mut() else {
        return Outcome::Failed("The world is not available".to_owned());
    };
    blocks.start_scan(
        &player.dim,
        columns,
        min_y,
        start_y,
        None,
        "minecraft:air".to_owned(),
        0,
        per_tick,
        restore_seconds * 20,
        128,
        "void-shaft",
        Some((player.name.clone(), player.pos)),
    );
    Outcome::Ok(format!("Preparing up to {potential}-block shaft under {} in batches of {per_tick}", player.name))
}

fn chunk_replace(spec: &ActionSpec, cx: &mut Cx<'_>, target: Option<&str>) -> Outcome {
    let Some(player) = first_target(cx, target) else {
        return Outcome::Skipped("No eligible target for chunk replace".to_owned());
    };
    let Ok(material) = block_material(&spec.string("material", ""), "material", true, true) else {
        return Outcome::Failed("Invalid material".to_owned());
    };
    let only = spec.optional("from").and_then(|f| block_material(&f, "from", true, true).ok());
    let radius = spec.int("radius-chunks", 0).unwrap_or(0) as i32;
    let per_tick = spec.int("blocks-per-tick", 512).unwrap_or(512) as usize;
    let max_blocks = spec.int("max-blocks", 0).unwrap_or(0).max(0) as usize;
    let restore_seconds = spec.int("restore-seconds", 0).unwrap_or(0) as u64;
    let Some(world) = cx.world.as_deref() else {
        return Outcome::Failed("The world is not available".to_owned());
    };
    if !world.valid_block(&material) {
        return Outcome::Failed(format!("Unknown block material: {material}"));
    }
    let Some((world_min, world_max)) = world.height(&player.dim) else {
        return Outcome::Skipped("Target world unavailable".to_owned());
    };
    let bottom = (optional_y(spec, "min-y").ok().flatten().unwrap_or(i64::from(world_min)) as i32).max(world_min);
    let top = (optional_y(spec, "max-y").ok().flatten().unwrap_or(i64::from(world_max) - 1) as i32).min(world_max - 1);
    if bottom > top {
        return Outcome::Skipped("Chunk replace Y range is outside the world".to_owned());
    }
    let (center_x, center_z) = ((player.pos[0].floor() as i32) >> 4, (player.pos[2].floor() as i32) >> 4);
    let mut columns = Vec::new();
    let mut skipped_chunks = 0;
    for cx_chunk in (center_x - radius)..=(center_x + radius) {
        for cz_chunk in (center_z - radius)..=(center_z + radius) {
            if !world.is_loaded(&player.dim, [cx_chunk << 4, bottom, cz_chunk << 4]) {
                skipped_chunks += 1;
                continue;
            }
            for x in (cx_chunk << 4)..((cx_chunk << 4) + 16) {
                for z in (cz_chunk << 4)..((cz_chunk << 4) + 16) {
                    columns.push((x, z));
                }
            }
        }
    }
    if columns.is_empty() {
        return Outcome::Skipped("No blocks to replace in target chunk(s)".to_owned());
    }
    let Some(blocks) = cx.blocks.as_deref_mut() else {
        return Outcome::Failed("The world is not available".to_owned());
    };
    blocks.start_scan(&player.dim, columns, bottom, top, only, material.clone(), max_blocks, per_tick, restore_seconds * 20, 1024, "chunk-replace", None);
    let area = if radius == 0 { format!("chunk {center_x},{center_z}") } else { format!("{0}x{0} chunks around {center_x},{center_z}", radius * 2 + 1) };
    Outcome::Ok(format!(
        "Replacing blocks with {material} in {area} in batches of {per_tick}{}",
        if skipped_chunks > 0 { format!(" ({skipped_chunks} unloaded chunks skipped)") } else { String::new() }
    ))
}

fn place_cobweb(spec: &ActionSpec, cx: &mut Cx<'_>, target: Option<&str>) -> Outcome {
    let Some(player) = first_target(cx, target) else {
        return Outcome::Skipped("No online target".to_owned());
    };
    let mut count = spec.int("count", 6).unwrap_or(6);
    if spec.flag("multiply-by-repeat") {
        count = (count * cx.repeat_count()).min(32);
    }
    let restore = spec.int("restore-seconds", 12).unwrap_or(12) as u64;
    let tick = cx.tick;
    let (Some(world), Some(blocks)) = (cx.world.as_deref_mut(), cx.blocks.as_deref_mut()) else {
        return Outcome::Failed("The world is not available".to_owned());
    };
    started(blocks.cobwebs(world, tick, &player.dim, player.pos, count, restore))
}

fn lightning(spec: &ActionSpec, cx: &mut Cx<'_>, target: Option<&str>) -> Outcome {
    let default_damage = spec.kind == "lightning";
    let players = named(cx.targets(target));
    let Some(player) = players.first() else {
        return Outcome::Skipped("No online target".to_owned());
    };
    let mut count = spec.int("count", 1).unwrap_or(1);
    if matches!(spec.param("multiply-by-repeat"), Some(Val::Bool(true))) {
        count = (count * cx.repeat_count()).min(32);
    }
    count = count.min(cx.config.max_entities_per_event);
    let seconds = spec.int("duration-seconds", if default_damage { 60 } else { 0 }).unwrap_or(0);
    let interval = spec.int("interval-ticks", 20).unwrap_or(20).max(1) as u64;
    let damage = match spec.param("damage") {
        None => default_damage,
        Some(v) => matches!(v, Val::Bool(true)),
    };
    let mut commands = Vec::new();
    for i in 0..count {
        if !cx.consume_entity_slot() {
            break;
        }
        let command = if damage {
            format!("execute at {} run summon minecraft:lightning_bolt ~ ~ ~", player.name)
        } else {
            let theta = i as f64 * 2.399963229728653;
            format!("execute at {} run summon minecraft:lightning_bolt ~{:.3} ~ ~{:.3}", player.name, theta.cos() * 2.0, theta.sin() * 2.0)
        };
        commands.push(command);
    }
    if commands.is_empty() {
        return Outcome::Skipped("Entity budget is exhausted".to_owned());
    }
    let strikes = commands.len();
    if seconds == 0 {
        for command in commands {
            cx.out.push(Emit::Command(command));
        }
        return Outcome::Ok("Lightning burst triggered".to_owned());
    }
    let first = commands.clone();
    for command in first {
        cx.out.push(Emit::Command(command));
    }
    cx.out.push(Emit::Every {
        key: Some(format!("storm:{}", player.name.to_ascii_lowercase())),
        interval,
        duration: seconds as u64 * 20,
        commands,
    });
    Outcome::Ok(format!("Lightning storm scheduled for {seconds} seconds ({strikes} strike(s) per burst)"))
}

/// The action catalog the rule loader uses to validate.
pub struct Catalog<'a> {
    pub config: &'a Config,
    /// The functions that exist, to validate `function` at load time. `None` does not check.
    pub functions: Option<&'a HashSet<String>>,
}

impl ActionCatalog for Catalog<'_> {
    fn contains(&self, kind: &str) -> bool {
        TYPES.contains(&kind)
    }

    fn type_list(&self) -> String {
        TYPES.join(", ")
    }

    fn validate(&self, spec: &ActionSpec) -> Result<(), String> {
        validate(spec, self.config, self.functions)
    }
}

#[allow(dead_code)] // reserved: part of the configuration's API/semantics
pub fn no_player_console(config: &Config) -> bool {
    config.no_player_policy == NoPlayerPolicy::Console
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config::parse(crate::config::DEFAULT_CONFIG).unwrap()
    }

    fn players() -> Vec<Target> {
        vec![
            Target { name: "Ana".into(), uuid: "u1".into(), pos: [0.0, 64.0, 0.0], dim: "minecraft:overworld".into() },
            Target { name: "Beto".into(), uuid: "u2".into(), pos: [100.0, 64.0, 0.0], dim: "minecraft:the_nether".into() },
        ]
    }

    fn run(spec: &ActionSpec, event: &Event) -> (Outcome, Vec<Emit>) {
        let config = config();
        let players = players();
        let mut limiter = RateLimiter::new(60);
        let mut cx = Cx::new(&config, event, &players, 1_000, &mut limiter);
        let outcome = execute(spec, &mut cx);
        (outcome, cx.out)
    }

    #[test]
    fn title_escapes_viewer_text_into_json() {
        let mut event = Event::new("chat");
        event.nickname = "Mal\"@e[type=!player]\n".into();
        let mut spec = ActionSpec::new("title");
        spec.params.insert("title".into(), Val::Str("Hola {viewer}".into()));
        let (outcome, emits) = run(&spec, &event);
        assert!(outcome.is_ok());
        let titles: Vec<&String> = emits
            .iter()
            .filter_map(|e| if let Emit::Command(c) = e { Some(c) } else { None })
            .filter(|c| c.contains(" title {"))
            .collect();
        assert_eq!(titles.len(), 1);
        assert!(titles[0].contains("\\\"@e[type=!player]"), "{}", titles[0]);
        assert!(!titles[0].contains('\n'));
    }

    #[test]
    fn spawn_entity_respects_allowed_entities_and_per_event_limit() {
        let event = Event::new("gift");
        let mut spec = ActionSpec::new("spawn-entity");
        spec.params.insert("entity".into(), Val::Str("wither".into()));
        let (outcome, emits) = run(&spec, &event);
        assert!(matches!(outcome, Outcome::Skipped(_)));
        assert!(emits.is_empty());
        spec.params.insert("entity".into(), Val::Str("zombie".into()));
        spec.params.insert("count".into(), Val::Int(50));
        let (outcome, emits) = run(&spec, &event);
        assert!(outcome.is_ok());
        assert_eq!(emits.len(), 5, "max-entities-per-event is 5 in the default config");
    }

    #[test]
    fn a_spawned_entity_can_carry_the_viewer_name() {
        let mut event = Event::new("chat");
        event.nickname = "Ana \"@e\"😀".into();
        let mut spec = ActionSpec::new("spawn-entity");
        spec.params.insert("entity".into(), Val::Str("zombie".into()));
        spec.params.insert("name".into(), Val::Str("{nickname}".into()));
        let (outcome, emits) = run(&spec, &event);
        assert!(outcome.is_ok());
        let Some(Emit::Command(command)) = emits.first() else { panic!("no command") };
        assert!(command.contains("summon minecraft:zombie ~ ~1 ~ {CustomName:{text:\"Ana \\\"@e\\\"\"},CustomNameVisible:1b}"), "{command}");
        assert!(!command.contains('😀'));
        spec.params.insert("name-visible".into(), Val::Bool(false));
        let (_, emits) = run(&spec, &event);
        assert!(matches!(&emits[0], Emit::Command(c) if c.ends_with("CustomNameVisible:0b}")));
    }

    #[test]
    fn titles_can_be_golden() {
        let mut event = Event::new("gift.final");
        event.nickname = "Ana".into();
        let event = event.with("gift.name", Val::Str("Rose".into())).with("gift.repeatCount", Val::Int(3));
        let mut spec = ActionSpec::new("title");
        spec.params.insert("title".into(), Val::Str("{nickname}".into()));
        spec.params.insert("subtitle".into(), Val::Str("sent {gift.name} x{gift.repeatCount}".into()));
        spec.params.insert("color".into(), Val::Str("gold".into()));
        spec.params.insert("bold".into(), Val::Bool(true));
        let (outcome, emits) = run(&spec, &event);
        assert!(outcome.is_ok());
        let commands: Vec<String> = emits.iter().filter_map(|e| if let Emit::Command(c) = e { Some(c.clone()) } else { None }).collect();
        assert!(commands.iter().any(|c| c.ends_with(r#"title {"bold":true,"color":"gold","text":"Ana"}"#)), "{commands:?}");
        assert!(commands.iter().any(|c| c.contains(r#"subtitle {"color":"gold","text":"sent Rose x3"}"#)), "{commands:?}");
        assert!(valid_color("#FFAA00") && valid_color("Gold") && !valid_color("purplish") && !valid_color("#12345"));

    }

    #[test]
    fn nearest_online_prefers_the_connection_target() {
        let event = Event::new("chat").with("connection.targetPlayer", Val::Str("beto".into()));
        let spec = {
            let mut s = ActionSpec::new("give-item");
            s.params.insert("material".into(), Val::Str("diamond".into()));
            s
        };
        let (_, emits) = run(&spec, &event);
        assert_eq!(emits, vec![Emit::Command("give Beto minecraft:diamond 1".into())]);
    }

    #[test]
    fn teleport_rejects_far_destinations() {
        let event = Event::new("chat");
        let mut spec = ActionSpec::new("teleport");
        spec.params.insert("destination".into(), Val::Str("5000,64,0".into()));
        let (outcome, _) = run(&spec, &event);
        assert!(matches!(outcome, Outcome::Failed(_)));
    }

    #[test]
    fn validation_catches_bad_parameters() {
        let config = config();
        let mut spec = ActionSpec::new("sound");
        spec.params.insert("sound".into(), Val::Str("entity.player.levelup; op hacker".into()));
        assert!(validate(&spec, &config, None).is_err());
        let spec = ActionSpec::new("command");
        assert!(validate(&spec, &config, None).is_err());
    }
}
