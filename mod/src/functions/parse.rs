//! Function parsers: JSON (`schemaVersion` 1) and extended `.mcfunction`.
use serde_json::Value as Json;

use super::model::{Definition, Format, Param, Step, TYPES};
use super::params::coerce;
use super::{valid_function_id, FnError, FnResult};
use crate::value::Val;

const STEP_TYPES: [&str; 9] = ["command", "summon", "title", "sound", "particle", "repeat", "delay", "function", "if"];

fn json_text(value: &Json) -> String {
    match value {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn str_field(object: &serde_json::Map<String, Json>, field: &str, at: &str) -> FnResult<String> {
    match object.get(field) {
        None | Some(Json::Null) => Err(FnError::new(format!("Missing '{field}' at {at}"), "MISSING_FIELD")),
        Some(Json::String(s)) => Ok(s.clone()),
        Some(_) => Err(FnError::new(format!("'{field}' must be a string at {at}"), "INVALID_FIELD")),
    }
}

fn opt_str(object: &serde_json::Map<String, Json>, field: &str, fallback: &str) -> String {
    match object.get(field) {
        None | Some(Json::Null) => fallback.to_owned(),
        Some(value) => json_text(value),
    }
}

fn valid_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn parse_json(fallback_id: &str, text: &str, origin: &str) -> FnResult<Definition> {
    let root: Json = serde_json::from_str(text).map_err(|e| FnError::new(format!("Malformed function JSON: {e}"), "INVALID_JSON"))?;
    let Json::Object(object) = root else {
        return Err(FnError::new("Function JSON must be an object", "INVALID_JSON"));
    };
    let schema = match object.get("schemaVersion") {
        None => 1,
        Some(v) => v.as_i64().ok_or_else(|| FnError::new("'schemaVersion' must be an integer at root", "INVALID_FIELD"))?,
    };
    if schema != 1 {
        return Err(FnError::new(format!("Unsupported schemaVersion {schema}; expected 1"), "UNSUPPORTED_SCHEMA"));
    }
    let name = if object.contains_key("name") { Some(str_field(&object, "name", "root")?) } else { Some(fallback_id.to_owned()) };
    let id = match name {
        Some(n) if n.contains(':') => n,
        _ if !fallback_id.is_empty() => fallback_id.to_owned(),
        Some(n) => n,
        None => String::new(),
    };
    if !valid_function_id(&id) {
        return Err(FnError::new("Function needs an id like 'namespace:name'", "INVALID_ID"));
    }
    let description = if object.contains_key("description") { str_field(&object, "description", "root")? } else { String::new() };
    let params = parse_parameters(&object)?;
    let steps = parse_steps(&object, "steps")?;
    if steps.is_empty() {
        return Err(FnError::new(format!("Function {id} has no steps"), "EMPTY_FUNCTION"));
    }
    Ok(Definition { id, description, params, steps, format: Format::Json, origin: origin.to_owned() })
}

fn parse_parameters(root: &serde_json::Map<String, Json>) -> FnResult<Vec<Param>> {
    let Some(value) = root.get("parameters") else {
        return Ok(Vec::new());
    };
    let Json::Object(map) = value else {
        return Err(FnError::new("'parameters' must be an object", "INVALID_PARAMETERS"));
    };
    let mut params = Vec::new();
    for (name, declaration) in map {
        if !valid_param_name(name) {
            return Err(FnError::new(format!("Invalid parameter name: {name}"), "INVALID_PARAMETER"));
        }
        let Json::Object(decl) = declaration else {
            return Err(FnError::new(format!("Parameter '{name}' must be an object"), "INVALID_PARAMETER"));
        };
        let ptype = decl
            .get("type")
            .and_then(Json::as_str)
            .ok_or_else(|| FnError::new(format!("Parameter '{name}' needs a type"), "INVALID_PARAMETER"))?
            .to_ascii_lowercase();
        if !TYPES.contains(&ptype.as_str()) {
            return Err(FnError::new(format!("Unknown type '{ptype}' for parameter '{name}'"), "UNKNOWN_TYPE"));
        }
        let default = decl.get("default").filter(|v| !v.is_null()).map(|v| Val::from_json(v.clone()));
        let values = match decl.get("values") {
            Some(Json::Array(items)) => items.iter().map(json_text).collect(),
            _ => Vec::new(),
        };
        let required = decl.get("required").and_then(Json::as_bool).unwrap_or(default.is_none());
        let param = Param {
            name: name.clone(),
            ptype,
            required,
            default: default.clone(),
            min: decl.get("min").and_then(Json::as_f64),
            max: decl.get("max").and_then(Json::as_f64),
            values,
            description: decl.get("description").and_then(Json::as_str).unwrap_or("").to_owned(),
        };
        if let Some(default) = &default {
            coerce(name, &param, default).map_err(|e| FnError::new(format!("Invalid parameter '{name}': {}", e.message), "INVALID_PARAMETER"))?;
        }
        params.push(param);
    }
    Ok(params)
}

fn parse_steps(object: &serde_json::Map<String, Json>, field: &str) -> FnResult<Vec<Step>> {
    let Some(value) = object.get(field) else {
        return Err(FnError::new(format!("Missing '{field}' array"), "MISSING_STEPS"));
    };
    let Json::Array(items) = value else {
        return Err(FnError::new(format!("'{field}' must be an array"), "INVALID_STEPS"));
    };
    parse_step_list(items, field)
}

fn parse_step_list(items: &[Json], at: &str) -> FnResult<Vec<Step>> {
    items.iter().enumerate().map(|(i, item)| parse_step(item, &format!("{at}[{i}]"))).collect()
}

fn parse_step(value: &Json, at: &str) -> FnResult<Step> {
    let Json::Object(object) = value else {
        return Err(FnError::new(format!("Step {at} must be an object"), "INVALID_STEP"));
    };
    let Some(kind) = object.get("type").and_then(Json::as_str) else {
        return Err(FnError::new(format!("Step {at} needs a type"), "INVALID_STEP"));
    };
    if !STEP_TYPES.contains(&kind) {
        return Err(FnError::new(format!("Unknown step type '{kind}' at {at}"), "UNKNOWN_STEP"));
    }
    Ok(match kind {
        "command" => Step::Command(str_field(object, "value", at)?),
        "summon" => Step::Summon {
            entity: str_field(object, "entity", at)?,
            position: opt_str(object, "position", "~ ~ ~"),
            nbt: opt_str(object, "nbt", ""),
        },
        "title" => Step::Title {
            target: opt_str(object, "target", "@a"),
            title: opt_str(object, "title", ""),
            subtitle: opt_str(object, "subtitle", ""),
            fade_in: opt_str(object, "fadeInTicks", "10"),
            stay: opt_str(object, "stayTicks", "60"),
            fade_out: opt_str(object, "fadeOutTicks", "20"),
        },
        "sound" => Step::Sound {
            sound: str_field(object, "sound", at)?,
            source: opt_str(object, "source", "master"),
            target: opt_str(object, "target", "@a"),
            volume: opt_str(object, "volume", "1"),
            pitch: opt_str(object, "pitch", "1"),
            position: opt_str(object, "position", "~ ~ ~"),
        },
        "particle" => Step::Particle {
            particle: str_field(object, "particle", at)?,
            position: opt_str(object, "position", "~ ~ ~"),
            count: opt_str(object, "count", "20"),
        },
        "repeat" => {
            let times = object.get("times").map(json_text).ok_or_else(|| FnError::new(format!("Repeat {at} needs 'times'"), "INVALID_STEP"))?;
            let Some(Json::Array(inner)) = object.get("steps") else {
                return Err(FnError::new(format!("Repeat {at} needs 'steps'"), "INVALID_STEP"));
            };
            Step::Repeat { times, steps: parse_step_list(inner, &format!("{at}.steps"))? }
        }
        "delay" => {
            let ticks = object.get("ticks").map(json_text).ok_or_else(|| FnError::new(format!("Delay {at} needs 'ticks'"), "INVALID_STEP"))?;
            Step::Delay(ticks)
        }
        "function" => {
            let name = str_field(object, "name", at)?;
            let mut args = Vec::new();
            if let Some(Json::Object(map)) = object.get("args") {
                for (key, value) in map {
                    args.push((key.clone(), json_text(value)));
                }
            }
            Step::Call { name, args }
        }
        _ => {
            let Some(Json::Object(cond)) = object.get("condition") else {
                return Err(FnError::new(format!("Condition {at} needs 'condition'"), "INVALID_CONDITION"));
            };
            let parameter = str_field(cond, "parameter", &format!("{at}.condition"))?;
            let operator = str_field(cond, "operator", &format!("{at}.condition"))?.to_ascii_lowercase();
            if !["eq", "ne", "gt", "gte", "lt", "lte"].contains(&operator.as_str()) {
                return Err(FnError::new(format!("Unknown condition operator '{operator}' at {at}"), "INVALID_CONDITION"));
            }
            let value = cond.get("value").map(json_text).unwrap_or_default();
            let Some(Json::Array(then)) = object.get("then") else {
                return Err(FnError::new(format!("Condition {at} needs 'then'"), "INVALID_CONDITION"));
            };
            let otherwise = match object.get("else") {
                Some(Json::Array(items)) => parse_step_list(items, &format!("{at}.else"))?,
                _ => Vec::new(),
            };
            Step::If { parameter, operator, value, then: parse_step_list(then, &format!("{at}.then"))?, otherwise }
        }
    })
}

/// Extended `.mcfunction`: `# namespace:name`, `# @param <name> <type> [default]` and one line per command.
pub fn parse_mcfunction(fallback_id: &str, text: &str, origin: &str) -> FnResult<Definition> {
    let mut params: Vec<Param> = Vec::new();
    let mut steps = Vec::new();
    let mut declared_id: Option<String> = None;
    for (index, raw) in text.split('\n').enumerate() {
        let line = raw.trim();
        let number = index + 1;
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim();
            if let Some(rest) = comment.strip_prefix("@param") {
                let mut parts = rest.trim().splitn(3, char::is_whitespace);
                let (Some(name), Some(ptype)) = (parts.next(), parts.next()) else {
                    continue;
                };
                if !valid_param_name(name) {
                    continue;
                }
                if params.iter().any(|p| p.name == name) {
                    return Err(FnError::new(format!("Duplicate @param '{name}' at line {number}"), "DUPLICATE_PARAMETER"));
                }
                let ptype = ptype.to_ascii_lowercase();
                if !TYPES.contains(&ptype.as_str()) {
                    return Err(FnError::new(format!("Unknown @param type '{ptype}' at line {number}"), "UNKNOWN_TYPE"));
                }
                let default_text = parts.next().map(str::trim).filter(|s| !s.is_empty());
                let default = default_text.map(|t| parse_default(&ptype, t));
                let param = Param {
                    name: name.to_owned(),
                    ptype,
                    required: default.is_none(),
                    default: default.clone(),
                    min: None,
                    max: None,
                    values: Vec::new(),
                    description: String::new(),
                };
                if let Some(default) = &default {
                    coerce(name, &param, default).map_err(|e| FnError::new(format!("Invalid @param at line {number}: {}", e.message), "INVALID_PARAMETER"))?;
                }
                params.push(param);
            } else if declared_id.is_none() && steps.is_empty() && params.is_empty() && valid_function_id(comment) {
                declared_id = Some(comment.to_owned());
            }
            continue;
        }
        if line.chars().count() > 512 {
            return Err(FnError::new(format!("Line {number} too long"), "COMMAND_TOO_LONG"));
        }
        steps.push(Step::Command(line.to_owned()));
    }
    if steps.is_empty() {
        return Err(FnError::new("No commands in .mcfunction file", "EMPTY_FUNCTION"));
    }
    let id = declared_id.unwrap_or_else(|| fallback_id.to_owned());
    if !valid_function_id(&id) {
        return Err(FnError::new("Cannot determine function id for .mcfunction file", "INVALID_ID"));
    }
    Ok(Definition { id, description: String::new(), params, steps, format: Format::McFunction, origin: origin.to_owned() })
}

fn parse_default(ptype: &str, text: &str) -> Val {
    let unquoted = if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') { &text[1..text.len() - 1] } else { text };
    match ptype {
        "integer" => unquoted.trim().parse::<i64>().map(Val::Int).unwrap_or_else(|_| Val::Str(unquoted.to_owned())),
        "number" => unquoted.trim().parse::<f64>().map(Val::Float).unwrap_or_else(|_| Val::Str(unquoted.to_owned())),
        _ => Val::Str(unquoted.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_plugin_example_functions() {
        let zombies = r#"{"schemaVersion":1,"name":"gifts:spawn_zombies","parameters":{"count":{"type":"integer","min":1,"max":5,"default":1}},
            "steps":[{"type":"repeat","times":"${count}","steps":[{"type":"summon","entity":"minecraft:zombie","position":"~ ~1 ~"}]},
            {"type":"title","target":"@a","title":"Zombie Attack!","subtitle":"x"}]}"#;
        let definition = parse_json("gifts:spawn_zombies", zombies, "test").unwrap();
        assert_eq!(definition.params.len(), 1);
        assert_eq!(definition.steps.len(), 2);
        assert!(!definition.params[0].required);
    }

    #[test]
    fn rejects_bad_functions_with_the_plugin_codes() {
        assert_eq!(parse_json("a:b", "[]", "").unwrap_err().code, "INVALID_JSON");
        assert_eq!(parse_json("a:b", r#"{"schemaVersion":2,"steps":[]}"#, "").unwrap_err().code, "UNSUPPORTED_SCHEMA");
        assert_eq!(parse_json("a:b", r#"{"steps":[{"type":"nope"}]}"#, "").unwrap_err().code, "UNKNOWN_STEP");
        assert_eq!(parse_json("a:b", r#"{"steps":[]}"#, "").unwrap_err().code, "EMPTY_FUNCTION");
    }

    #[test]
    fn parses_mcfunction_headers() {
        let text = "# utilities:weather_change\n# @param mode string clear\n\nweather ${mode}\nsay Weather updated\n";
        let definition = parse_mcfunction("minecraft:x", text, "").unwrap();
        assert_eq!(definition.id, "utilities:weather_change");
        assert_eq!(definition.params[0].default, Some(Val::Str("clear".into())));
        assert_eq!(definition.steps.len(), 2);
    }
}
