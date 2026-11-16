//! Loads rules from YAML with the plugin's same strict validation: an invalid trigger, action,
//! comparison, field or duration is an error that names the file, the rule and the path.
use std::collections::BTreeMap;

use super::compare::Operator;
use super::fields;
use super::model::{
    ActionSpec, AggScope, Aggregate, Condition, CooldownScope, CooldownSpec, EventRule, FieldMatcher, Metric,
};
use crate::event::is_known_trigger;
use crate::value::Val;

/// What the loader needs to know about actions, without depending on the game.
pub trait ActionCatalog {
    fn contains(&self, kind: &str) -> bool;
    fn type_list(&self) -> String;
    fn validate(&self, spec: &ActionSpec) -> Result<(), String>;
}

/// Durations: `500ms`, `10s`, `2m`, `1h`, `1d` or a number of milliseconds. `-1` if invalid.
pub fn parse_duration_ms(raw: &str) -> i64 {
    let text = raw.trim().to_ascii_lowercase();
    if text.is_empty() {
        return -1;
    }
    let digits_end = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    if digits_end == 0 {
        return -1;
    }
    let Ok(value) = text[..digits_end].parse::<i64>() else {
        return -1;
    };
    let unit = &text[digits_end..];
    let factor = match unit {
        "" | "ms" => 1,
        "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return -1,
    };
    value.checked_mul(factor).unwrap_or(-1)
}

fn text(value: Option<&Val>) -> String {
    value.map(|v| v.text().trim().to_owned()).unwrap_or_default()
}

fn flag(value: Option<&Val>, fallback: bool) -> bool {
    value.and_then(Val::as_bool).unwrap_or(fallback)
}

/// Reads a rules file. A file without a `rules` section contributes nothing.
pub fn load_rules(yaml: &str, file: &str, catalog: &dyn ActionCatalog) -> Result<Vec<EventRule>, String> {
    let root: serde_norway::Value =
        serde_norway::from_str(yaml).map_err(|e| format!("The file is not valid YAML: {e}"))?;
    let root = Val::from_yaml(root);
    let Some(entries) = root.get("rules") else {
        return Ok(Vec::new());
    };
    let Val::List(entries) = entries else {
        return Err("'rules' must be a list".to_owned());
    };
    let mut rules = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let path = format!("rules[{index}]");
        let Val::Map(map) = entry else {
            return Err(format!("{path} must be a mapping"));
        };
        rules.push(read_rule(map, &path, file, catalog)?);
    }
    Ok(rules)
}

fn read_rule(
    map: &BTreeMap<String, Val>,
    path: &str,
    file: &str,
    catalog: &dyn ActionCatalog,
) -> Result<EventRule, String> {
    let id = text(map.get("id"));
    if id.is_empty() {
        return Err(format!("{path}.id is required"));
    }
    let valid_id = id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
    if !valid_id {
        return Err(format!("{path}.id must be at most 64 letters, digits, '.' or '_'"));
    }
    let where_ = format!("{file} {path} (rule '{id}')");

    let enabled = flag(map.get("enabled"), true);
    let trigger = text(map.get("trigger")).to_ascii_lowercase();
    if !is_known_trigger(&trigger) {
        return Err(format!("{where_}: unknown trigger '{trigger}'"));
    }

    let mut matchers = Vec::new();
    match map.get("match") {
        Some(Val::Map(m)) => {
            for (field, value) in m {
                matchers.extend(read_matcher(field, value, &format!("{where_}.match.{field}"))?);
            }
        }
        Some(Val::Null) | None => {}
        Some(_) => return Err(format!("{where_}.match must be a mapping")),
    }

    let condition = match map.get("conditions") {
        Some(Val::List(items)) => Some(read_conditions(items, &format!("{where_}.conditions"))?),
        Some(Val::Null) | None => None,
        Some(_) => return Err(format!("{where_}.conditions must be a list")),
    };

    let aggregate = match map.get("aggregate") {
        Some(Val::Map(m)) => Some(read_aggregate(m, &format!("{where_}.aggregate"), &trigger)?),
        Some(Val::Null) | None => None,
        Some(_) => return Err(format!("{where_}.aggregate must be a mapping")),
    };

    let cooldown = match map.get("cooldown") {
        Some(Val::Map(m)) => Some(read_cooldown(m, &format!("{where_}.cooldown"))?),
        Some(Val::Null) | None => None,
        Some(_) => return Err(format!("{where_}.cooldown must be a mapping")),
    };

    let actions = read_actions(map.get("actions"), &format!("{where_}.actions"), catalog)?;

    let mut priority = 0;
    if let Some(value) = map.get("priority").filter(|v| !v.is_null()) {
        priority = value
            .as_i64()
            .ok_or_else(|| format!("{where_}.priority must be a whole number"))? as i32;
        if !(-1000..=1000).contains(&priority) {
            return Err(format!("{where_}.priority must be between -1000 and 1000"));
        }
    }
    let stop_processing = flag(map.get("stop-processing"), false);
    Ok(EventRule {
        id,
        enabled,
        trigger,
        matchers,
        aggregate,
        cooldown,
        condition,
        actions,
        priority,
        stop_processing,
        source: file.to_owned(),
    })
}

fn read_matcher(field: &str, value: &Val, where_: &str) -> Result<Vec<FieldMatcher>, String> {
    if !fields::is_known(field) {
        return Err(format!("{where_}: unknown field '{field}'"));
    }
    if let Val::Map(m) = value {
        let mut matchers = Vec::new();
        for (name, expected) in m {
            let op = Operator::parse(name)
                .ok_or_else(|| format!("{where_}: unsupported comparison '{name}'"))?;
            matchers.push(FieldMatcher::new(field, op, expected.clone()));
        }
        if matchers.is_empty() {
            return Err(format!("{where_} must name a comparison"));
        }
        return Ok(matchers);
    }
    Ok(vec![FieldMatcher::new(field, Operator::Equals, value.clone())])
}

fn read_conditions(entries: &[Val], where_: &str) -> Result<Condition, String> {
    if entries.is_empty() {
        return Err(format!("{where_} must not be empty"));
    }
    let mut children = Vec::new();
    for entry in entries {
        children.push(read_condition(entry, where_)?);
    }
    Ok(Condition::All(children))
}

fn read_condition(entry: &Val, where_: &str) -> Result<Condition, String> {
    let Val::Map(map) = entry else {
        return Err(format!("{where_} must be a mapping"));
    };
    if let Some(all) = map.get("all") {
        return Ok(Condition::All(read_condition_list(all, &format!("{where_}.all"))?));
    }
    if let Some(any) = map.get("any") {
        return Ok(Condition::Any(read_condition_list(any, &format!("{where_}.any"))?));
    }
    if let Some(inner) = map.get("not") {
        if let Val::Map(m) = inner {
            if let Some(all) = m.get("all") {
                let list = read_condition_list(all, &format!("{where_}.not.all"))?;
                return Ok(Condition::Not(Box::new(Condition::All(list))));
            }
            if let Some(any) = m.get("any") {
                let list = read_condition_list(any, &format!("{where_}.not.any"))?;
                return Ok(Condition::Not(Box::new(Condition::Any(list))));
            }
        }
        return Ok(Condition::Not(Box::new(read_condition(inner, &format!("{where_}.not"))?)));
    }
    let field = text(map.get("field"));
    if field.is_empty() {
        return Err(format!("{where_}.field is required"));
    }
    if !fields::is_known(&field) {
        return Err(format!("{where_}: unknown field '{field}'"));
    }
    let op_name = text(map.get("operator"));
    let op = Operator::parse(if op_name.is_empty() { "equals" } else { &op_name })
        .ok_or_else(|| format!("{where_}: unsupported comparison"))?;
    let value = map
        .get("value")
        .ok_or_else(|| format!("{where_}.value is required"))?
        .clone();
    Ok(Condition::Leaf(FieldMatcher::new(&field, op, value)))
}

fn read_condition_list(value: &Val, where_: &str) -> Result<Vec<Condition>, String> {
    let Val::List(items) = value else {
        return Err(format!("{where_} must be a list"));
    };
    if items.is_empty() {
        return Err(format!("{where_} must not be empty"));
    }
    items
        .iter()
        .enumerate()
        .map(|(i, entry)| read_condition(entry, &format!("{where_}[{i}]")))
        .collect()
}

fn read_aggregate(map: &BTreeMap<String, Val>, where_: &str, trigger: &str) -> Result<Aggregate, String> {
    let metric =
        Metric::parse(&text(map.get("metric"))).ok_or_else(|| format!("{where_}.metric must be 'count' or 'total'"))?;
    let scope = AggScope::parse(&text(map.get("scope")))
        .ok_or_else(|| format!("{where_}.scope must be 'event', 'user' or 'global'"))?;
    let threshold_value = map
        .get("threshold")
        .filter(|v| !v.is_null())
        .ok_or_else(|| format!("{where_}.threshold is required"))?;
    let threshold = threshold_value
        .as_i64()
        .ok_or_else(|| format!("{where_}.threshold must be a whole number"))?;
    if threshold <= 0 || threshold > 1_000_000 {
        return Err(format!("{where_}.threshold must be between 1 and 1000000"));
    }
    if trigger != "like" {
        return Err(format!("{where_} can only be used with the 'like' trigger, not '{trigger}'"));
    }
    Ok(Aggregate { metric, scope, threshold, carry_remainder: flag(map.get("carry-remainder"), false) })
}

fn read_cooldown(map: &BTreeMap<String, Val>, where_: &str) -> Result<CooldownSpec, String> {
    let duration = parse_duration_ms(&text(map.get("duration")));
    if duration < 0 {
        return Err(format!("{where_}.duration must be a duration such as '10s', '2m' or '500ms'"));
    }
    if duration > 86_400_000 {
        return Err(format!("{where_}.duration must be at most 24 hours"));
    }
    let scope_text = text(map.get("scope"));
    let scope = CooldownScope::parse(if scope_text.is_empty() { "global" } else { &scope_text })
        .ok_or_else(|| format!("{where_}.scope must be 'global', 'user', 'rule' or 'user-and-rule'"))?;
    Ok(CooldownSpec { duration_ms: duration, scope })
}

fn read_actions(value: Option<&Val>, where_: &str, catalog: &dyn ActionCatalog) -> Result<Vec<ActionSpec>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let Val::List(entries) = value else {
        return Err(format!("{where_} must be a list"));
    };
    let mut actions = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let path = format!("{where_}[{index}]");
        let Val::Map(map) = entry else {
            return Err(format!("{path} must be a mapping"));
        };
        let kind = text(map.get("type"));
        if kind.is_empty() {
            return Err(format!("{path}.type is required"));
        }
        if !catalog.contains(&kind) {
            return Err(format!("{path}: unknown action type '{kind}'; known types are {}", catalog.type_list()));
        }
        let mut spec = ActionSpec::new(&kind);
        for (name, parameter) in map {
            if name != "type" {
                spec.params.insert(name.clone(), parameter.clone());
            }
        }
        catalog.validate(&spec).map_err(|e| format!("{path}: {e}"))?;
        actions.push(spec);
    }
    if actions.is_empty() {
        return Err(format!("{where_} must not be empty"));
    }
    Ok(actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Catalog;
    impl ActionCatalog for Catalog {
        fn contains(&self, kind: &str) -> bool {
            kind == "broadcast"
        }
        fn type_list(&self) -> String {
            "broadcast".into()
        }
        fn validate(&self, _spec: &ActionSpec) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration_ms("10s"), 10_000);
        assert_eq!(parse_duration_ms("500ms"), 500);
        assert_eq!(parse_duration_ms("2m"), 120_000);
        assert_eq!(parse_duration_ms("750"), 750);
        assert_eq!(parse_duration_ms("1x"), -1);
    }

    #[test]
    fn loads_a_chat_rule_and_rejects_bad_triggers() {
        let ok = "rules:\n  - id: chat-hello\n    trigger: chat\n    match:\n      text:\n        equals-ignore-case: \"!hello\"\n    cooldown:\n      duration: 10s\n      scope: user\n    actions:\n      - type: broadcast\n        message: hi\n";
        let rules = load_rules(ok, "chat.yml", &Catalog).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].cooldown.as_ref().unwrap().duration_ms, 10_000);
        let bad = "rules:\n  - id: broken\n    trigger: not-a-trigger\n    actions:\n      - type: broadcast\n";
        let error = load_rules(bad, "chat.yml", &Catalog).unwrap_err();
        assert!(error.contains("unknown trigger 'not-a-trigger'"), "{error}");
    }
}
