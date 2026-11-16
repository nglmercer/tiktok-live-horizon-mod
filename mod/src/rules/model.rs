//! A rule's model: matchers, conditions, thresholds, cooldowns and actions.
use std::collections::BTreeMap;

use super::compare::{self, Operator};
use super::fields;
use crate::event::Event;
use crate::value::Val;

#[derive(Clone, Debug)]
pub struct FieldMatcher {
    pub field: String,
    pub op: Operator,
    pub value: Val,
}

impl FieldMatcher {
    pub fn new(field: &str, op: Operator, value: Val) -> FieldMatcher {
        FieldMatcher { field: fields::resolve(field), op, value }
    }

    pub fn matches(&self, event: &Event) -> bool {
        match fields::read(event, &self.field) {
            None => false,
            Some(actual) => compare::test(self.op, &actual, &self.value),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    Leaf(FieldMatcher),
}

impl Condition {
    pub fn test(&self, event: &Event) -> bool {
        match self {
            Condition::All(children) => children.iter().all(|c| c.test(event)),
            Condition::Any(children) => children.iter().any(|c| c.test(event)),
            Condition::Not(child) => !child.test(event),
            Condition::Leaf(matcher) => matcher.matches(event),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Count,
    Total,
}

impl Metric {
    pub fn parse(raw: &str) -> Option<Metric> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "count" => Some(Metric::Count),
            "total" => Some(Metric::Total),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggScope {
    Event,
    User,
    Global,
}

impl AggScope {
    pub fn parse(raw: &str) -> Option<AggScope> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "event" => Some(AggScope::Event),
            "user" => Some(AggScope::User),
            "global" => Some(AggScope::Global),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Aggregate {
    pub metric: Metric,
    pub scope: AggScope,
    pub threshold: i64,
    pub carry_remainder: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CooldownScope {
    Rule,
    User,
    UserAndRule,
    Global,
}

impl CooldownScope {
    pub fn parse(raw: &str) -> Option<CooldownScope> {
        match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "rule" => Some(CooldownScope::Rule),
            "user" => Some(CooldownScope::User),
            "user-and-rule" => Some(CooldownScope::UserAndRule),
            "global" => Some(CooldownScope::Global),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CooldownSpec {
    pub duration_ms: i64,
    pub scope: CooldownScope,
}

impl CooldownSpec {
    pub fn key(&self, rule_id: &str, user_key: &str) -> String {
        match self.scope {
            CooldownScope::User | CooldownScope::UserAndRule => format!("user:{user_key}|rule:{rule_id}"),
            CooldownScope::Rule => format!("rule:{rule_id}"),
            CooldownScope::Global => "global".to_owned(),
        }
    }
}

/// An action: its type and its parameters as the rule wrote them.
#[derive(Clone, Debug)]
pub struct ActionSpec {
    pub kind: String,
    pub params: BTreeMap<String, Val>,
}

impl ActionSpec {
    pub fn new(kind: &str) -> ActionSpec {
        ActionSpec { kind: kind.to_owned(), params: BTreeMap::new() }
    }

    pub fn param(&self, name: &str) -> Option<&Val> {
        self.params.get(name).filter(|v| !v.is_null())
    }

    pub fn string(&self, name: &str, fallback: &str) -> String {
        self.param(name).map(Val::text).unwrap_or_else(|| fallback.to_owned())
    }

    /// The trimmed text, or `None` if it is missing or empty.
    pub fn optional(&self, name: &str) -> Option<String> {
        let text = self.param(name)?.text().trim().to_owned();
        if text.is_empty() { None } else { Some(text) }
    }

    pub fn require(&self, name: &str) -> Result<String, String> {
        self.optional(name).ok_or_else(|| format!("'{name}' is required"))
    }

    pub fn int(&self, name: &str, fallback: i64) -> Result<i64, String> {
        match self.param(name) {
            None => Ok(fallback),
            Some(Val::Int(i)) => Ok(*i),
            Some(Val::Float(f)) => Ok(*f as i64),
            Some(other) => other
                .text()
                .trim()
                .parse::<i64>()
                .map_err(|_| format!("'{name}' must be a whole number")),
        }
    }

    pub fn float(&self, name: &str, fallback: f64) -> Result<f64, String> {
        match self.param(name) {
            None => Ok(fallback),
            Some(Val::Int(i)) => Ok(*i as f64),
            Some(Val::Float(f)) => Ok(*f),
            Some(other) => other
                .text()
                .trim()
                .parse::<f64>()
                .map_err(|_| format!("'{name}' must be a number")),
        }
    }

    /// The plugin's `Boolean.parseBoolean(optional(...))`: only "true" (case-insensitive).
    pub fn flag(&self, name: &str) -> bool {
        self.param(name).is_some_and(|v| match v {
            Val::Bool(b) => *b,
            other => other.text().trim().eq_ignore_ascii_case("true"),
        })
    }
}

#[derive(Clone, Debug)]
pub struct EventRule {
    pub id: String,
    pub enabled: bool,
    pub trigger: String,
    pub matchers: Vec<FieldMatcher>,
    pub aggregate: Option<Aggregate>,
    pub cooldown: Option<CooldownSpec>,
    pub condition: Option<Condition>,
    pub actions: Vec<ActionSpec>,
    pub priority: i32,
    pub stop_processing: bool,
    pub source: String,
}

impl EventRule {
    pub fn matches(&self, event: &Event) -> bool {
        self.matchers.iter().all(|m| m.matches(event))
            && self.condition.as_ref().is_none_or(|c| c.test(event))
    }
}
