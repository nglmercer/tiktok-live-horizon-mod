//! `${parameter}` interpolation with the same protections as the plugin (`CommandTemplateRenderer`).
use std::collections::BTreeMap;

use super::model::Param;
use super::{FnError, FnResult};
use crate::value::Val;

const ESCAPED: &str = "\u{0}ESCAPED-DOLLAR\u{0}";

fn plain_text(value: &Val) -> String {
    match value {
        Val::Float(f) if f.fract() == 0.0 && f.is_finite() => (*f as i64).to_string(),
        other => other.text(),
    }
}

fn render_value(name: &str, param: Option<&Param>, value: &Val) -> FnResult<String> {
    let text = plain_text(value);
    if text.contains(['\n', '\r', '\0']) {
        return Err(FnError::new(format!("Parameter '{name}' must not contain newlines"), "UNSAFE_VALUE"));
    }
    if text.contains(';') || text.contains('\u{a7}') {
        return Err(FnError::new(format!("Parameter '{name}' contains a forbidden character"), "UNSAFE_VALUE"));
    }
    match param {
        Some(p) if matches!(p.ptype.as_str(), "selector" | "player" | "entity" | "resource_location" | "position") => {
            if text.contains(['"', '\'', '`']) || text.contains("$(") || text.contains("&&") || text.contains("||") {
                return Err(FnError::new(format!("Parameter '{name}' contains unsafe characters"), "UNSAFE_VALUE"));
            }
        }
        Some(_) => {}
        None => {
            if text.contains(['"', '`']) || text.contains("$(") {
                return Err(FnError::new(
                    format!("Untyped interpolation of '{name}' contains unsafe characters; declare the parameter type"),
                    "UNSAFE_VALUE",
                ));
            }
        }
    }
    Ok(text)
}

/// Names of the `${name}` present (ignoring `$${`).
pub fn placeholders(template: &str) -> Vec<String> {
    let guarded = template.replace("$${", "");
    let mut out = Vec::new();
    let mut rest = guarded.as_str();
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else { break };
        let name = &after[..end];
        let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if valid {
            out.push(name.to_owned());
        }
        rest = &after[end + 1..];
    }
    out
}

pub fn render(template: &str, values: &BTreeMap<String, Val>, declared: Option<&[Param]>) -> FnResult<String> {
    let guarded = template.replace("$${", ESCAPED);
    let mut out = String::with_capacity(guarded.len());
    let mut rest = guarded.as_str();
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else { break };
        let name = &after[..end];
        let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            out.push_str(&rest[..start + 2]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..start]);
        let Some(value) = values.get(name) else {
            if declared.is_some_and(|d| !d.iter().any(|p| p.name == name)) {
                return Err(FnError::new(format!("Unknown parameter: {name}"), "UNKNOWN_PARAMETER"));
            }
            return Err(FnError::new(format!("Missing value for parameter: {name}"), "MISSING_PARAMETER"));
        };
        let param = declared.and_then(|d| d.iter().find(|p| p.name == name));
        out.push_str(&render_value(name, param, value)?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out.replace(ESCAPED, "${"))
}

/// Checks at load time that every `${x}` points at a declared parameter.
pub fn check_references(template: &str, declared: &[Param], at: &str) -> FnResult<()> {
    for name in placeholders(template) {
        if !declared.iter().any(|p| p.name == name) {
            return Err(FnError::new(format!("Unknown parameter ${{{name}}} in {at}"), "UNKNOWN_PARAMETER"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared() -> Vec<Param> {
        vec![
            Param { name: "count".into(), ptype: "integer".into(), required: true, default: None, min: None, max: None, values: vec![], description: String::new() },
            Param { name: "who".into(), ptype: "player".into(), required: true, default: None, min: None, max: None, values: vec![], description: String::new() },
        ]
    }

    #[test]
    fn renders_and_escapes() {
        let mut values = BTreeMap::new();
        values.insert("count".to_owned(), Val::Int(3));
        values.insert("who".to_owned(), Val::Str("Steve".into()));
        assert_eq!(render("give ${who} stone ${count} $${literal}", &values, Some(&declared())).unwrap(), "give Steve stone 3 ${literal}");
    }

    #[test]
    fn unsafe_and_unknown_values_are_refused() {
        let mut values = BTreeMap::new();
        values.insert("who".to_owned(), Val::Str("a\"b".into()));
        assert_eq!(render("${who}", &values, Some(&declared())).unwrap_err().code, "UNSAFE_VALUE");
        assert_eq!(render("${nope}", &values, Some(&declared())).unwrap_err().code, "UNKNOWN_PARAMETER");
        values.insert("who".to_owned(), Val::Str("a;b".into()));
        assert_eq!(render("${who}", &values, Some(&declared())).unwrap_err().code, "UNSAFE_VALUE");
    }
}
