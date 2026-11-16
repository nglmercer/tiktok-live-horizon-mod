//! Resolution and validation of a function's arguments (`ParameterResolver`).
use std::collections::BTreeMap;

use super::model::Param;
use super::{FnError, FnResult};
use crate::value::Val;

fn is_resource_location(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    match lower.split_once(':') {
        Some((ns, path)) => {
            !ns.is_empty()
                && !path.is_empty()
                && ns.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-'))
                && path.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '/' | '.' | '-'))
        }
        None => false,
    }
}

fn is_player_name(text: &str) -> bool {
    (1..=16).contains(&text.len()) && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_selector(text: &str) -> bool {
    if is_player_name(text) {
        return true;
    }
    let Some(rest) = text.strip_prefix('@') else {
        return false;
    };
    let mut chars = rest.chars();
    let Some(kind) = chars.next() else {
        return false;
    };
    if !matches!(kind, 'a' | 'p' | 'r' | 'e' | 's') {
        return false;
    }
    let tail: String = chars.collect();
    tail.is_empty() || (tail.starts_with('[') && tail.ends_with(']') && !tail[1..tail.len() - 1].contains(']'))
}

fn is_position(text: &str) -> bool {
    if text.is_empty() || text.len() > 64 {
        return false;
    }
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.is_empty() || parts.len() > 3 {
        return false;
    }
    parts.iter().all(|part| {
        if part.len() > 16 {
            return false;
        }
        let body = part.strip_prefix(['~', '^']).unwrap_or(part);
        let body = body.strip_prefix('-').unwrap_or(body);
        let mut dots = 0;
        body.chars().all(|c| {
            if c == '.' {
                dots += 1;
                dots <= 1
            } else {
                c.is_ascii_digit()
            }
        })
    })
}

fn as_string(raw: &Val) -> FnResult<String> {
    match raw {
        Val::Str(s) => Ok(s.clone()),
        Val::Int(_) | Val::Float(_) | Val::Bool(_) => Ok(raw.text()),
        _ => Err(FnError::new(format!("Expected a string, got: {}", raw.text()), "INVALID_VALUE")),
    }
}

fn check_string(name: &str, value: String) -> FnResult<String> {
    if value.contains(['\n', '\r', '\0']) {
        return Err(FnError::new(format!("Invalid value for parameter '{name}': must not contain newlines"), "INVALID_VALUE"));
    }
    if value.chars().count() > 512 {
        return Err(FnError::new(format!("Invalid value for parameter '{name}': too long"), "VALUE_TOO_LONG"));
    }
    Ok(value)
}

fn as_long(name: &str, raw: &Val) -> FnResult<i64> {
    let bad = || FnError::new(format!("Invalid integer for parameter '{name}'"), "INVALID_VALUE");
    match raw {
        Val::Int(i) => Ok(*i),
        Val::Float(f) if f.fract() == 0.0 => Ok(*f as i64),
        Val::Str(s) => s.trim().parse().map_err(|_| bad()),
        _ => Err(bad()),
    }
}

fn as_double(name: &str, raw: &Val) -> FnResult<f64> {
    let bad = || FnError::new(format!("Invalid number for parameter '{name}'"), "INVALID_VALUE");
    match raw {
        Val::Int(i) => Ok(*i as f64),
        Val::Float(f) => Ok(*f),
        Val::Str(s) => s.trim().parse().map_err(|_| bad()),
        _ => Err(bad()),
    }
}

fn check_range(name: &str, param: &Param, value: f64) -> FnResult<()> {
    if param.min.is_some_and(|m| value < m) {
        return Err(FnError::new(format!("Parameter '{name}' below minimum {}", param.min.unwrap_or(0.0)), "OUT_OF_RANGE"));
    }
    if param.max.is_some_and(|m| value > m) {
        return Err(FnError::new(format!("Parameter '{name}' above maximum {}", param.max.unwrap_or(0.0)), "OUT_OF_RANGE"));
    }
    Ok(())
}

/// Converts a raw value to the parameter's type, or fails with the same code as the plugin.
pub fn coerce(name: &str, param: &Param, raw: &Val) -> FnResult<Val> {
    match param.ptype.as_str() {
        "string" => Ok(Val::Str(check_string(name, as_string(raw)?)?)),
        "boolean" => match raw {
            Val::Bool(b) => Ok(Val::Bool(*b)),
            Val::Str(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" => Ok(Val::Bool(true)),
                "false" => Ok(Val::Bool(false)),
                _ => Err(FnError::new(format!("Invalid boolean for parameter '{name}'"), "INVALID_VALUE")),
            },
            _ => Err(FnError::new(format!("Invalid boolean for parameter '{name}'"), "INVALID_VALUE")),
        },
        "integer" => {
            let value = as_long(name, raw)?;
            check_range(name, param, value as f64)?;
            Ok(Val::Int(value))
        }
        "number" => {
            let value = as_double(name, raw)?;
            check_range(name, param, value)?;
            Ok(Val::Float(value))
        }
        "enum" => {
            let value = as_string(raw)?;
            if !param.values.contains(&value) {
                return Err(FnError::new(
                    format!("Invalid value for parameter '{name}': expected one of {:?}", param.values),
                    "INVALID_ENUM",
                ));
            }
            Ok(Val::Str(value))
        }
        "entity" | "resource_location" => {
            let value = as_string(raw)?;
            if !is_resource_location(&value) {
                return Err(FnError::new(format!("Invalid resource location for parameter '{name}': {value}"), "INVALID_RESOURCE"));
            }
            Ok(Val::Str(value))
        }
        "player" => {
            let value = as_string(raw)?;
            if !is_player_name(&value) {
                return Err(FnError::new(format!("Invalid player name for parameter '{name}': {value}"), "INVALID_PLAYER"));
            }
            Ok(Val::Str(value))
        }
        "selector" => {
            let value = as_string(raw)?;
            if value.contains(['\n', '\r']) || !is_selector(&value) {
                return Err(FnError::new(format!("Invalid selector for parameter '{name}': {value}"), "INVALID_SELECTOR"));
            }
            Ok(Val::Str(value))
        }
        "position" => {
            let value = as_string(raw)?;
            if value.contains(['\n', '\r']) || !is_position(&value) {
                return Err(FnError::new(format!("Invalid position for parameter '{name}': {value}"), "INVALID_POSITION"));
            }
            Ok(Val::Str(value))
        }
        other => Err(FnError::new(format!("Unknown parameter type: {other}"), "UNKNOWN_TYPE")),
    }
}

/// Validates the supplied arguments against the declared parameters.
pub fn resolve(declared: &[Param], supplied: &BTreeMap<String, Val>) -> FnResult<BTreeMap<String, Val>> {
    for key in supplied.keys() {
        if !declared.iter().any(|p| &p.name == key) {
            return Err(FnError::new(format!("Unknown parameter: {key}"), "UNKNOWN_PARAMETER"));
        }
    }
    let mut resolved = BTreeMap::new();
    for param in declared {
        let raw = match supplied.get(&param.name).filter(|v| !v.is_null()) {
            Some(raw) => raw.clone(),
            None => match &param.default {
                Some(default) => default.clone(),
                None if param.required => {
                    return Err(FnError::new(format!("Missing required parameter: {}", param.name), "MISSING_PARAMETER"));
                }
                None => continue,
            },
        };
        resolved.insert(param.name.clone(), coerce(&param.name, param, &raw)?);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(name: &str, ptype: &str) -> Param {
        Param {
            name: name.into(),
            ptype: ptype.into(),
            required: true,
            default: None,
            min: Some(1.0),
            max: Some(5.0),
            values: vec!["happy".into(), "angry".into()],
            description: String::new(),
        }
    }

    #[test]
    fn integers_are_range_checked_and_selectors_validated() {
        assert_eq!(coerce("count", &param("count", "integer"), &Val::Str("3".into())).unwrap(), Val::Int(3));
        assert_eq!(coerce("count", &param("count", "integer"), &Val::Int(9)).unwrap_err().code, "OUT_OF_RANGE");
        assert!(coerce("t", &param("t", "selector"), &Val::Str("@a[distance=..5]".into())).is_ok());
        assert_eq!(coerce("t", &param("t", "selector"), &Val::Str("@e; op x".into())).unwrap_err().code, "INVALID_SELECTOR");
        assert!(coerce("p", &param("p", "position"), &Val::Str("~ ~1 ~".into())).is_ok());
        assert_eq!(coerce("m", &param("m", "enum"), &Val::Str("sad".into())).unwrap_err().code, "INVALID_ENUM");
    }

    #[test]
    fn unknown_and_missing_parameters_are_errors() {
        let declared = vec![param("count", "integer")];
        let mut args = BTreeMap::new();
        assert_eq!(resolve(&declared, &args).unwrap_err().code, "MISSING_PARAMETER");
        args.insert("nope".to_owned(), Val::Int(1));
        assert_eq!(resolve(&declared, &args).unwrap_err().code, "UNKNOWN_PARAMETER");
    }
}
