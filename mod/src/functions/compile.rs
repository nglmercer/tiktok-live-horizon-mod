//! Expands a function into a flat list of commands and waits (`FunctionCompiler`).
use std::collections::{BTreeMap, BTreeSet};

use super::model::{Definition, Param, Step};
use super::params::resolve;
use super::render::{check_references, render};
use super::{FnError, FnResult, Limits};
use crate::value::Val;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A command with a leading slash.
    Command(String),
    Delay(u32),
}

#[derive(Debug, Clone)]
pub struct Plan {
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub function_id: String,
    pub items: Vec<Item>,
}

/// Allowlist of command roots (`CommandPolicy`). `*` allows any.
pub struct Policy<'a> {
    pub allowed: &'a [String],
}

impl Policy<'_> {
    pub fn allows(&self, text: &str) -> bool {
        if text.contains(['\n', '\r', '\0']) {
            return false;
        }
        let Some(command) = text.strip_prefix('/') else {
            return true;
        };
        if command.is_empty() || command.starts_with(char::is_whitespace) {
            return false;
        }
        let root = command.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        self.allowed.iter().any(|a| a == "*" || *a == root)
    }
}

pub struct Compiler<'a> {
    pub registry: &'a BTreeMap<String, Definition>,
    pub limits: &'a Limits,
    pub policy: &'a Policy<'a>,
}

fn bounded_int(text: &str, min: i64, max: i64, what: &str) -> FnResult<i64> {
    let value: i64 = text.trim().parse().map_err(|_| FnError::new(format!("Invalid {what}: {text}"), "INVALID_VALUE"))?;
    if value < min || value > max {
        return Err(FnError::new(format!("Field '{what}' out of range: {text}"), "OUT_OF_RANGE"));
    }
    Ok(value)
}

fn bounded_double(text: &str, min: f64, max: f64, min_inclusive: bool, what: &str) -> FnResult<f64> {
    let value: f64 = text.trim().parse().map_err(|_| FnError::new(format!("Invalid {what}: {text}"), "INVALID_VALUE"))?;
    let low = if min_inclusive { value >= min } else { value > min };
    if !low || value > max || value.is_nan() {
        return Err(FnError::new(format!("Field '{what}' out of range: {text}"), "OUT_OF_RANGE"));
    }
    Ok(value)
}

fn trim_number(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() { (value as i64).to_string() } else { value.to_string() }
}

impl Compiler<'_> {
    pub fn compile(&self, function_id: &str, args: &BTreeMap<String, Val>) -> FnResult<Plan> {
        self.compile_inner(function_id, args, &BTreeSet::new(), 0)
    }

    fn compile_inner(&self, function_id: &str, raw_args: &BTreeMap<String, Val>, stack: &BTreeSet<String>, depth: usize) -> FnResult<Plan> {
        if stack.contains(function_id) {
            return Err(FnError::new(format!("Recursive function call: {function_id} (cycle {stack:?})"), "RECURSION"));
        }
        if depth > self.limits.max_nesting_depth {
            return Err(FnError::new(format!("Maximum nesting depth exceeded at {function_id}"), "LIMIT_EXCEEDED"));
        }
        let definition = self
            .registry
            .get(function_id)
            .ok_or_else(|| FnError::new(format!("Unknown function: {function_id}"), "UNKNOWN_FUNCTION"))?;
        if raw_args.len() > self.limits.max_args {
            return Err(FnError::new("Too many arguments", "TOO_MANY_ARGUMENTS"));
        }
        let resolved = resolve(&definition.params, raw_args)?;
        let mut stack = stack.clone();
        stack.insert(function_id.to_owned());
        let mut items = Vec::new();
        self.expand(&definition.steps, definition, &resolved, &stack, depth, &mut items)?;
        if items.len() > self.limits.max_expanded_commands {
            return Err(FnError::new(
                format!("Function {function_id} expands to {} items, above the limit of {}", items.len(), self.limits.max_expanded_commands),
                "LIMIT_EXCEEDED",
            ));
        }
        for item in &items {
            if let Item::Command(command) = item {
                if command.chars().count() > 512 {
                    return Err(FnError::new(format!("Expanded command too long ({} chars)", command.chars().count()), "COMMAND_TOO_LONG"));
                }
                if !self.policy.allows(command) {
                    return Err(FnError::new(format!("Command denied by policy: {command}"), "COMMAND_DENIED"));
                }
            }
        }
        Ok(Plan { function_id: function_id.to_owned(), items })
    }

    fn expand(&self, steps: &[Step], owner: &Definition, values: &BTreeMap<String, Val>, stack: &BTreeSet<String>, depth: usize, items: &mut Vec<Item>) -> FnResult<()> {
        let declared: Option<&[Param]> = Some(&owner.params);
        let r = |template: &str| render(template, values, declared);
        for step in steps {
            match step {
                Step::Command(value) => {
                    let rendered = r(value)?;
                    items.push(Item::Command(if rendered.starts_with('/') { rendered } else { format!("/{rendered}") }));
                }
                Step::Summon { entity, position, nbt } => {
                    let (entity, position, nbt) = (r(entity)?, r(position)?, r(nbt)?);
                    let mut command = format!("/summon {entity} {position}");
                    if !nbt.is_empty() {
                        command.push(' ');
                        command.push_str(&nbt);
                    }
                    items.push(Item::Command(command));
                }
                Step::Title { target, title, subtitle, fade_in, stay, fade_out } => {
                    let target = r(target)?;
                    let (title, subtitle) = (r(title)?, r(subtitle)?);
                    let fade_in = bounded_int(&r(fade_in)?, 0, 1200, "fadeInTicks")?;
                    let stay = bounded_int(&r(stay)?, 0, 12000, "stayTicks")?;
                    let fade_out = bounded_int(&r(fade_out)?, 0, 1200, "fadeOutTicks")?;
                    items.push(Item::Command(format!("/title {target} times {fade_in} {stay} {fade_out}")));
                    if !title.is_empty() {
                        items.push(Item::Command(format!("/title {target} title {}", serde_json::to_string(&title).unwrap_or_default())));
                    }
                    if !subtitle.is_empty() {
                        items.push(Item::Command(format!("/title {target} subtitle {}", serde_json::to_string(&subtitle).unwrap_or_default())));
                    }
                }
                Step::Sound { sound, source, target, volume, pitch, position } => {
                    let (sound, source, target, position) = (r(sound)?, r(source)?, r(target)?, r(position)?);
                    let volume = bounded_double(&r(volume)?, 0.0, 16.0, true, "volume")?;
                    let pitch = bounded_double(&r(pitch)?, 0.0, 2.0, false, "pitch")?;
                    items.push(Item::Command(format!("/playsound {sound} {source} {target} {position} {} {}", trim_number(volume), trim_number(pitch))));
                }
                Step::Particle { particle, position, count } => {
                    let (particle, position) = (r(particle)?, r(position)?);
                    let count = bounded_int(&r(count)?, 1, 10000, "count")?;
                    items.push(Item::Command(format!("/particle {particle} {position} 0 0 0 0 {count}")));
                }
                Step::Delay(ticks) => {
                    let rendered = r(ticks)?;
                    let ticks: i64 = rendered.trim().parse().map_err(|_| FnError::new(format!("Invalid delay ticks: {rendered}"), "INVALID_VALUE"))?;
                    if ticks < 0 {
                        return Err(FnError::new(format!("Invalid delay ticks: {rendered}"), "INVALID_VALUE"));
                    }
                    if ticks > i64::from(self.limits.max_delay_ticks) {
                        return Err(FnError::new(format!("Delay of {ticks} ticks exceeds the limit of {}", self.limits.max_delay_ticks), "LIMIT_EXCEEDED"));
                    }
                    if ticks > 0 {
                        items.push(Item::Delay(ticks as u32));
                    }
                }
                Step::Repeat { times, steps } => {
                    let rendered = r(times)?;
                    let times: i64 = rendered.trim().parse().map_err(|_| FnError::new(format!("Invalid repeat times: {rendered}"), "INVALID_VALUE"))?;
                    if times < 1 || times as usize > self.limits.max_loop_iterations {
                        return Err(FnError::new(
                            format!("Repeat count {rendered} out of range (max {})", self.limits.max_loop_iterations),
                            "LIMIT_EXCEEDED",
                        ));
                    }
                    for _ in 0..times {
                        self.expand(steps, owner, values, stack, depth + 1, items)?;
                        if items.len() > self.limits.max_expanded_commands {
                            break; // the limit is reported later with a clear message
                        }
                    }
                }
                Step::Call { name, args } => {
                    let mut child_args = BTreeMap::new();
                    for (key, value) in args {
                        child_args.insert(key.clone(), Val::Str(r(value)?));
                    }
                    let child = self.compile_inner(name, &child_args, stack, depth + 1)?;
                    items.extend(child.items);
                }
                Step::If { parameter, operator, value, then, otherwise } => {
                    let Some(actual) = values.get(parameter) else {
                        return Err(FnError::new(format!("Condition parameter '{parameter}' was not provided"), "MISSING_PARAMETER"));
                    };
                    let expected = render(value, values, None)?;
                    let branch = if evaluate(operator, &actual.text(), &expected)? { then } else { otherwise };
                    self.expand(branch, owner, values, stack, depth + 1, items)?;
                }
            }
        }
        Ok(())
    }
}

fn evaluate(operator: &str, left: &str, right: &str) -> FnResult<bool> {
    let (ln, rn) = (left.trim().parse::<f64>().ok(), right.trim().parse::<f64>().ok());
    match operator {
        "eq" => Ok(match (ln, rn) { (Some(a), Some(b)) => a == b, _ => left == right }),
        "ne" => Ok(match (ln, rn) { (Some(a), Some(b)) => a != b, _ => left != right }),
        "gt" | "gte" | "lt" | "lte" => {
            let ordering = match (ln, rn) {
                (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
                _ => left.cmp(right),
            };
            Ok(match operator {
                "gt" => ordering.is_gt(),
                "gte" => ordering.is_ge(),
                "lt" => ordering.is_lt(),
                _ => ordering.is_le(),
            })
        }
        other => Err(FnError::new(format!("Unknown condition operator: {other}"), "INVALID_CONDITION")),
    }
}

/// Validation at load time (`FunctionValidator`): references, static limits and expanded size.
pub fn validate(definition: &Definition, limits: &Limits) -> FnResult<()> {
    if definition.steps.is_empty() {
        return Err(FnError::new(format!("Function {} has no steps", definition.id), "EMPTY_FUNCTION"));
    }
    let expanded = count(&definition.steps, &definition.params, 0, limits)?;
    if expanded > limits.max_expanded_commands {
        return Err(FnError::new(
            format!("Function {} expands to {expanded} commands, above the limit of {}", definition.id, limits.max_expanded_commands),
            "LIMIT_EXCEEDED",
        ));
    }
    Ok(())
}

fn static_long(text: &str) -> Option<i64> {
    if text.contains("${") { None } else { text.trim().parse().ok() }
}

fn count(steps: &[Step], params: &[Param], depth: usize, limits: &Limits) -> FnResult<usize> {
    if depth > limits.max_nesting_depth {
        return Err(FnError::new("Maximum nesting depth exceeded", "LIMIT_EXCEEDED"));
    }
    let mut total = 0usize;
    for (index, step) in steps.iter().enumerate() {
        let at = format!("step {index} ({})", step.type_name());
        match step {
            Step::Command(value) => {
                if value.chars().count() > 512 {
                    return Err(FnError::new(format!("Command too long in {at}"), "COMMAND_TOO_LONG"));
                }
                check_references(value, params, &at)?;
                total += 1;
            }
            Step::Summon { entity, position, .. } => {
                check_references(entity, params, &at)?;
                check_references(position, params, &at)?;
                total += 1;
            }
            Step::Title { target, title, subtitle, fade_in, stay, fade_out } => {
                for text in [target, title, subtitle, fade_in, stay, fade_out] {
                    check_references(text, params, &at)?;
                }
                for (text, max, field) in [(fade_in, 1200, "fadeInTicks"), (stay, 12000, "stayTicks"), (fade_out, 1200, "fadeOutTicks")] {
                    if let Some(value) = static_long(text) {
                        if !(0..=max).contains(&value) {
                            return Err(FnError::new(format!("Title {field} out of range in {at}"), "OUT_OF_RANGE"));
                        }
                    }
                }
                total += 1 + usize::from(!title.is_empty()) + usize::from(!subtitle.is_empty());
            }
            Step::Sound { sound, target, volume, pitch, .. } => {
                check_references(sound, params, &at)?;
                check_references(target, params, &at)?;
                if let Ok(v) = volume.parse::<f64>() {
                    if !(0.0..=16.0).contains(&v) {
                        return Err(FnError::new(format!("Field 'volume' out of range in {at}"), "OUT_OF_RANGE"));
                    }
                }
                if let Ok(p) = pitch.parse::<f64>() {
                    if p <= 0.0 || p > 2.0 {
                        return Err(FnError::new(format!("Field 'pitch' out of range in {at}"), "OUT_OF_RANGE"));
                    }
                }
                total += 1;
            }
            Step::Particle { particle, count: n, .. } => {
                check_references(particle, params, &at)?;
                if let Some(value) = static_long(n) {
                    if !(1..=10000).contains(&value) {
                        return Err(FnError::new(format!("Field 'count' out of range in {at}"), "OUT_OF_RANGE"));
                    }
                }
                total += 1;
            }
            Step::Delay(ticks) => {
                if let Some(value) = static_long(ticks) {
                    if value > i64::from(limits.max_delay_ticks) {
                        return Err(FnError::new(format!("Delay too long in {at}: max {} ticks", limits.max_delay_ticks), "LIMIT_EXCEEDED"));
                    }
                }
            }
            Step::Repeat { times, steps } => {
                let inner = count(steps, params, depth + 1, limits)?;
                match static_long(times) {
                    Some(t) => {
                        if t < 1 || t as usize > limits.max_loop_iterations {
                            return Err(FnError::new(format!("Repeat count out of range in {at}: max {}", limits.max_loop_iterations), "LIMIT_EXCEEDED"));
                        }
                        total += inner * t as usize;
                    }
                    None => {
                        check_references(times, params, &at)?;
                        total += inner;
                    }
                }
            }
            Step::Call { args, .. } => {
                for (key, value) in args {
                    check_references(value, params, &format!("{at} arg {key}"))?;
                }
                total += 1;
            }
            Step::If { parameter, operator, then, otherwise, .. } => {
                if !["eq", "ne", "gt", "gte", "lt", "lte"].contains(&operator.as_str()) {
                    return Err(FnError::new(format!("Unknown condition operator '{operator}' in {at}"), "INVALID_CONDITION"));
                }
                if !params.iter().any(|p| &p.name == parameter) {
                    return Err(FnError::new(format!("Unknown condition parameter '{parameter}' in {at}"), "UNKNOWN_PARAMETER"));
                }
                total += count(then, params, depth + 1, limits)?.max(count(otherwise, params, depth + 1, limits)?);
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::parse::parse_json;

    fn registry(texts: &[(&str, &str)]) -> BTreeMap<String, Definition> {
        texts.iter().map(|(id, text)| ((*id).to_owned(), parse_json(id, text, "t").unwrap())).collect()
    }

    const ZOMBIES: &str = r#"{"name":"gifts:spawn_zombies","parameters":{"count":{"type":"integer","min":1,"max":5,"default":1}},
        "steps":[{"type":"repeat","times":"${count}","steps":[{"type":"summon","entity":"minecraft:zombie","position":"~ ~1 ~"}]},
        {"type":"title","target":"@a","title":"Zombie Attack!","subtitle":"A TikTok viewer summoned zombies!"}]}"#;

    #[test]
    fn expands_repeat_and_title() {
        let registry = registry(&[("gifts:spawn_zombies", ZOMBIES)]);
        let allowed = vec!["summon".to_owned(), "title".to_owned()];
        let compiler = Compiler { registry: &registry, limits: &Limits::default(), policy: &Policy { allowed: &allowed } };
        let mut args = BTreeMap::new();
        args.insert("count".to_owned(), Val::Int(2));
        let plan = compiler.compile("gifts:spawn_zombies", &args).unwrap();
        let commands: Vec<String> = plan.items.iter().map(|i| match i { Item::Command(c) => c.clone(), Item::Delay(t) => format!("delay {t}") }).collect();
        assert_eq!(commands[0], "/summon minecraft:zombie ~ ~1 ~");
        assert_eq!(commands[1], commands[0]);
        assert_eq!(commands[2], "/title @a times 10 60 20");
        assert!(commands[3].starts_with("/title @a title \"Zombie Attack!\""));
        assert_eq!(commands.len(), 5);
    }

    #[test]
    fn policy_and_limits_are_enforced() {
        let registry = registry(&[("gifts:spawn_zombies", ZOMBIES)]);
        let allowed = vec!["title".to_owned()];
        let compiler = Compiler { registry: &registry, limits: &Limits::default(), policy: &Policy { allowed: &allowed } };
        assert_eq!(compiler.compile("gifts:spawn_zombies", &BTreeMap::new()).unwrap_err().code, "COMMAND_DENIED");
        let mut args = BTreeMap::new();
        args.insert("count".to_owned(), Val::Int(50));
        assert_eq!(compiler.compile("gifts:spawn_zombies", &args).unwrap_err().code, "OUT_OF_RANGE");
    }

    #[test]
    fn recursion_is_detected() {
        let a = r#"{"name":"t:a","steps":[{"type":"function","name":"t:b"}]}"#;
        let b = r#"{"name":"t:b","steps":[{"type":"function","name":"t:a"}]}"#;
        let registry = registry(&[("t:a", a), ("t:b", b)]);
        let allowed = vec!["*".to_owned()];
        let compiler = Compiler { registry: &registry, limits: &Limits::default(), policy: &Policy { allowed: &allowed } };
        assert_eq!(compiler.compile("t:a", &BTreeMap::new()).unwrap_err().code, "RECURSION");
    }

    #[test]
    fn conditions_choose_a_branch() {
        let text = r#"{"name":"t:c","parameters":{"n":{"type":"integer","default":3}},
            "steps":[{"type":"if","condition":{"parameter":"n","operator":"gt","value":"2"},
            "then":[{"type":"command","value":"say big"}],"else":[{"type":"command","value":"say small"}]}]}"#;
        let registry = registry(&[("t:c", text)]);
        let allowed = vec!["say".to_owned()];
        let compiler = Compiler { registry: &registry, limits: &Limits::default(), policy: &Policy { allowed: &allowed } };
        let plan = compiler.compile("t:c", &BTreeMap::new()).unwrap();
        assert_eq!(plan.items, vec![Item::Command("/say big".into())]);
    }
}
