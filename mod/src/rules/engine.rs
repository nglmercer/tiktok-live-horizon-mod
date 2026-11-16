//! Matches events with rules: filters, threshold, cooldown and priority.
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use super::model::{AggScope, Aggregate, EventRule, Metric};
use crate::event::Event;

/// An immutable set of rules, ordered by priority (higher first; stable).
#[derive(Clone, Default)]
pub struct Snapshot {
    pub rules: Vec<Arc<EventRule>>,
}

impl Snapshot {
    pub fn new(rules: Vec<EventRule>) -> Snapshot {
        let mut sorted: Vec<Arc<EventRule>> = rules.into_iter().map(Arc::new).collect();
        sorted.sort_by(|a, b| b.priority.cmp(&a.priority));
        Snapshot { rules: sorted }
    }

    pub fn for_trigger<'a>(&'a self, trigger: &'a str) -> impl Iterator<Item = &'a Arc<EventRule>> + 'a {
        self.rules.iter().filter(move |r| r.trigger == trigger)
    }

    pub fn by_id(&self, id: &str) -> Option<&Arc<EventRule>> {
        self.rules.iter().find(|r| r.id == id)
    }
}

/// Bounded cooldowns: entries expire on their own and the store never grows without limit.
pub struct CooldownStore {
    expiries: HashMap<String, i64>,
    order: VecDeque<String>,
    max_keys: usize,
}

impl CooldownStore {
    pub fn new(max_keys: usize) -> CooldownStore {
        CooldownStore { expiries: HashMap::new(), order: VecDeque::new(), max_keys: max_keys.max(16) }
    }

    /// `true` when the caller may act; `false` if the key is still on cooldown.
    pub fn try_acquire(&mut self, key: &str, duration_ms: i64, now: i64) -> bool {
        if duration_ms <= 0 {
            return true;
        }
        if let Some(expiry) = self.expiries.get(key) {
            if *expiry > now {
                return false;
            }
        }
        if self.expiries.insert(key.to_owned(), now + duration_ms).is_none() {
            self.order.push_back(key.to_owned());
        }
        self.evict(now);
        true
    }

    fn evict(&mut self, now: i64) {
        self.order.retain(|k| match self.expiries.get(k) {
            Some(expiry) if *expiry > now => true,
            _ => {
                self.expiries.remove(k);
                false
            }
        });
        while self.expiries.len() > self.max_keys {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.expiries.remove(&oldest);
                }
                None => break,
            }
        }
    }

    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn clear(&mut self) {
        self.expiries.clear();
        self.order.clear();
    }

    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn len(&self) -> usize {
        self.expiries.len()
    }
}

/// Counts deltas and signals when a threshold is crossed. Only the event's delta is counted:
/// the room's accumulated total is never the counter, so the same likes are not counted twice.
pub struct ThresholdCounter {
    counters: HashMap<String, i64>,
    order: VecDeque<String>,
    max_keys: usize,
}

impl ThresholdCounter {
    pub fn new(max_keys: usize) -> ThresholdCounter {
        ThresholdCounter { counters: HashMap::new(), order: VecDeque::new(), max_keys: max_keys.max(16) }
    }

    /// Returns how many times the threshold was crossed (zero or more).
    pub fn add(&mut self, key: &str, delta: i64, threshold: i64, carry_remainder: bool) -> i64 {
        if threshold <= 0 || delta <= 0 {
            return 0;
        }
        let value = self.counters.get(key).copied().unwrap_or(0);
        let total = value.saturating_add(delta);
        let crossed = total / threshold;
        let remainder = if crossed > 0 {
            if carry_remainder { total % threshold } else { 0 }
        } else {
            total
        };
        if self.counters.insert(key.to_owned(), remainder).is_none() {
            self.order.push_back(key.to_owned());
        }
        while self.counters.len() > self.max_keys {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.counters.remove(&oldest);
                }
                None => break,
            }
        }
        crossed
    }

    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn clear(&mut self) {
        self.counters.clear();
        self.order.clear();
    }

    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn len(&self) -> usize {
        self.counters.len()
    }
}

pub struct RuleEngine {
    pub snapshot: Snapshot,
    cooldowns: CooldownStore,
    counters: ThresholdCounter,
    overrides: HashMap<String, bool>,
    pub executed: u64,
    pub rejected: u64,
}

impl Default for RuleEngine {
    fn default() -> Self {
        RuleEngine {
            snapshot: Snapshot::default(),
            cooldowns: CooldownStore::new(8192),
            counters: ThresholdCounter::new(4096),
            overrides: HashMap::new(),
            executed: 0,
            rejected: 0,
        }
    }
}

impl RuleEngine {
    pub fn apply(&mut self, snapshot: Snapshot) {
        self.snapshot = snapshot;
    }

    /// Forces a rule on or off without touching its file. `false` if the rule does not exist.
    pub fn set_enabled(&mut self, rule_id: &str, enabled: bool) -> bool {
        if self.snapshot.by_id(rule_id).is_none() {
            return false;
        }
        self.overrides.insert(rule_id.to_owned(), enabled);
        true
    }

    pub fn override_of(&self, rule_id: &str) -> Option<bool> {
        self.overrides.get(rule_id).copied()
    }

    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn reset(&mut self) {
        self.cooldowns.clear();
        self.counters.clear();
    }

    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn cooldown_keys(&self) -> usize {
        self.cooldowns.len()
    }

    #[allow(dead_code)] // reserved
    pub fn counter_keys(&self) -> usize {
        self.counters.len()
    }

    /// The rules that fire for an event (a rule may repeat if a single
    /// likes event crosses several thresholds, up to 8 times).
    pub fn matching(&mut self, event: &Event, now: i64) -> Vec<Arc<EventRule>> {
        let candidates: Vec<Arc<EventRule>> = self.snapshot.for_trigger(&event.kind).cloned().collect();
        let mut matched = Vec::new();
        for rule in candidates {
            let times = self.match_count(&rule, event, now);
            for _ in 0..times {
                matched.push(rule.clone());
            }
        }
        matched
    }

    fn match_count(&mut self, rule: &EventRule, event: &Event, now: i64) -> i64 {
        let enabled = self.overrides.get(&rule.id).copied().unwrap_or(rule.enabled);
        if !enabled || !rule.matches(event) {
            return 0;
        }
        let mut activations = 1;
        if let Some(aggregate) = &rule.aggregate {
            let delta = metric_value(aggregate, event);
            let key = aggregate_key(aggregate, rule, event);
            let crossed = self.counters.add(&key, delta, aggregate.threshold, aggregate.carry_remainder);
            if crossed < 1 {
                return 0;
            }
            activations = crossed.min(8);
        }
        if let Some(cooldown) = &rule.cooldown {
            let key = cooldown.key(&rule.id, event.user_key());
            if !self.cooldowns.try_acquire(&key, cooldown.duration_ms, now) {
                return 0;
            }
        }
        activations
    }
}

fn metric_value(aggregate: &Aggregate, event: &Event) -> i64 {
    match aggregate.metric {
        Metric::Total => event.like_total(),
        Metric::Count => event.like_count(),
    }
}

fn aggregate_key(aggregate: &Aggregate, rule: &EventRule, event: &Event) -> String {
    match aggregate.scope {
        AggScope::User => format!("{}|user:{}", rule.id, event.user_key()),
        AggScope::Event => format!("{}|event:{}", rule.id, event.id),
        AggScope::Global => format!("{}|global", rule.id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::model::{CooldownScope, CooldownSpec};

    #[test]
    fn threshold_counts_deltas_and_carries_remainder() {
        let mut counter = ThresholdCounter::new(16);
        assert_eq!(counter.add("k", 30, 50, true), 0);
        assert_eq!(counter.add("k", 30, 50, true), 1);
        assert_eq!(counter.add("k", 45, 50, true), 1);
        assert_eq!(counter.add("j", 120, 50, false), 2);
        assert_eq!(counter.add("j", 49, 50, false), 0);
    }

    #[test]
    fn cooldown_blocks_until_expiry() {
        let mut store = CooldownStore::new(16);
        let spec = CooldownSpec { duration_ms: 1000, scope: CooldownScope::User };
        let key = spec.key("r", "ana");
        assert!(store.try_acquire(&key, 1000, 0));
        assert!(!store.try_acquire(&key, 1000, 500));
        assert!(store.try_acquire(&key, 1000, 1500));
        assert!(store.try_acquire(&spec.key("r", "otro"), 1000, 500));
    }
}
