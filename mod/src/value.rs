//! A dynamic value (YAML/JSON/event payload) with the plugin's text and number semantics.
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Val>),
    Map(BTreeMap<String, Val>),
}

impl Val {
    /// Equivalent of Java's `String.valueOf` (lists joined with ", " as in `Comparison.text`).
    pub fn text(&self) -> String {
        match self {
            Val::Null => String::new(),
            Val::Bool(b) => b.to_string(),
            Val::Int(i) => i.to_string(),
            Val::Float(f) => {
                if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e7 {
                    format!("{f:.1}")
                } else {
                    f.to_string()
                }
            }
            Val::Str(s) => s.clone(),
            Val::List(items) => items.iter().map(Val::text).collect::<Vec<_>>().join(", "),
            Val::Map(map) => {
                let inner: Vec<String> = map.iter().map(|(k, v)| format!("{k}={}", v.text())).collect();
                format!("{{{}}}", inner.join(", "))
            }
        }
    }

    /// A number, or a text that reads as a number. A boolean is never a number.
    pub fn number(&self) -> Option<f64> {
        match self {
            Val::Int(i) => Some(*i as f64),
            Val::Float(f) => Some(*f),
            Val::Str(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    /// Only the real numeric types (`instanceof Number` in Java).
    pub fn is_number_type(&self) -> bool {
        matches!(self, Val::Int(_) | Val::Float(_))
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(*i),
            Val::Float(f) => Some(*f as i64),
            Val::Str(s) => s.trim().parse::<i64>().ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Val::Bool(b) => Some(*b),
            Val::Str(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" => Some(true),
                "false" | "no" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Val::Null)
    }

    pub fn get(&self, key: &str) -> Option<&Val> {
        match self {
            Val::Map(map) => map.get(key),
            _ => None,
        }
    }

    pub fn from_yaml(value: serde_norway::Value) -> Val {
        use serde_norway::Value as Y;
        match value {
            Y::Null => Val::Null,
            Y::Bool(b) => Val::Bool(b),
            Y::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Val::Int(i)
                } else if let Some(u) = n.as_u64() {
                    Val::Int(u.min(i64::MAX as u64) as i64)
                } else {
                    Val::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            Y::String(s) => Val::Str(s),
            Y::Sequence(items) => Val::List(items.into_iter().map(Val::from_yaml).collect()),
            Y::Mapping(map) => {
                let mut out = BTreeMap::new();
                for (k, v) in map {
                    let key = match Val::from_yaml(k) {
                        Val::Str(s) => s,
                        other => other.text(),
                    };
                    out.insert(key, Val::from_yaml(v));
                }
                Val::Map(out)
            }
            Y::Tagged(tagged) => Val::from_yaml(tagged.value),
        }
    }

    pub fn from_json(value: serde_json::Value) -> Val {
        use serde_json::Value as J;
        match value {
            J::Null => Val::Null,
            J::Bool(b) => Val::Bool(b),
            J::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Val::Int(i)
                } else {
                    Val::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            J::String(s) => Val::Str(s),
            J::Array(items) => Val::List(items.into_iter().map(Val::from_json).collect()),
            J::Object(map) => Val::Map(map.into_iter().map(|(k, v)| (k, Val::from_json(v))).collect()),
        }
    }
}
