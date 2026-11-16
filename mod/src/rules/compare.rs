//! A rule's typed comparisons. An expression is never evaluated.
use crate::value::Val;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Equals,
    EqualsIgnoreCase,
    NotEqual,
    Contains,
    StartsWith,
    EndsWith,
    GreaterThan,
    GreaterOrEqual,
    LessThan,
    LessOrEqual,
    In,
}

pub const NAMES: [&str; 11] = [
    "equals",
    "equals-ignore-case",
    "not-equal",
    "contains",
    "starts-with",
    "ends-with",
    "greater-than",
    "greater-or-equal",
    "less-than",
    "less-or-equal",
    "in",
];

impl Operator {
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub fn wire_name(self) -> &'static str {
        match self {
            Operator::Equals => "equals",
            Operator::EqualsIgnoreCase => "equals-ignore-case",
            Operator::NotEqual => "not-equal",
            Operator::Contains => "contains",
            Operator::StartsWith => "starts-with",
            Operator::EndsWith => "ends-with",
            Operator::GreaterThan => "greater-than",
            Operator::GreaterOrEqual => "greater-or-equal",
            Operator::LessThan => "less-than",
            Operator::LessOrEqual => "less-or-equal",
            Operator::In => "in",
        }
    }

    /// Accepts the names and the symbols of the script format (`==`, `!=`, `>`, ...).
    pub fn parse(raw: &str) -> Option<Operator> {
        let name = raw.trim().to_ascii_lowercase();
        match name.as_str() {
            "==" => Some(Operator::Equals),
            "!=" => Some(Operator::NotEqual),
            ">" => Some(Operator::GreaterThan),
            ">=" => Some(Operator::GreaterOrEqual),
            "<" => Some(Operator::LessThan),
            "<=" => Some(Operator::LessOrEqual),
            "equals" => Some(Operator::Equals),
            "equals-ignore-case" => Some(Operator::EqualsIgnoreCase),
            "not-equal" => Some(Operator::NotEqual),
            "contains" => Some(Operator::Contains),
            "starts-with" => Some(Operator::StartsWith),
            "ends-with" => Some(Operator::EndsWith),
            "greater-than" => Some(Operator::GreaterThan),
            "greater-or-equal" => Some(Operator::GreaterOrEqual),
            "less-than" => Some(Operator::LessThan),
            "less-or-equal" => Some(Operator::LessOrEqual),
            "in" => Some(Operator::In),
            _ => None,
        }
    }
}

pub fn test(op: Operator, actual: &Val, expected: &Val) -> bool {
    match op {
        Operator::In => contains_in(actual, expected),
        Operator::Equals => equal(actual, expected, false),
        Operator::EqualsIgnoreCase => equal(actual, expected, true),
        Operator::NotEqual => !equal(actual, expected, false),
        Operator::Contains => actual.text().contains(&expected.text()),
        Operator::StartsWith => actual.text().starts_with(&expected.text()),
        Operator::EndsWith => actual.text().ends_with(&expected.text()),
        Operator::GreaterThan => is_numeric(actual, expected) && compare(actual, expected).is_gt(),
        Operator::GreaterOrEqual => is_numeric(actual, expected) && compare(actual, expected).is_ge(),
        Operator::LessThan => is_numeric(actual, expected) && compare(actual, expected).is_lt(),
        Operator::LessOrEqual => is_numeric(actual, expected) && compare(actual, expected).is_le(),
    }
}

fn equal(actual: &Val, expected: &Val, ignore_case: bool) -> bool {
    if actual.is_number_type() && expected.is_number_type() {
        return actual.as_i64() == expected.as_i64();
    }
    let left = actual.text();
    let right = expected.text();
    if ignore_case {
        left.to_lowercase() == right.to_lowercase()
    } else {
        left == right
    }
}

fn contains_in(actual: &Val, expected: &Val) -> bool {
    match expected {
        Val::List(items) => items.iter().any(|candidate| equal(actual, candidate, false)),
        _ => false,
    }
}

pub fn is_numeric(actual: &Val, expected: &Val) -> bool {
    number_of(actual).is_some() && number_of(expected).is_some()
}

fn number_of(value: &Val) -> Option<f64> {
    match value {
        Val::Bool(_) => None,
        other => other.number(),
    }
}

fn compare(actual: &Val, expected: &Val) -> std::cmp::Ordering {
    match (number_of(actual), number_of(expected)) {
        (Some(l), Some(r)) => l.partial_cmp(&r).unwrap_or(std::cmp::Ordering::Equal),
        _ => actual.text().cmp(&expected.text()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_compare_numerically_and_text_does_not_satisfy_numeric() {
        assert!(test(Operator::GreaterOrEqual, &Val::Int(100), &Val::Int(100)));
        assert!(test(Operator::GreaterThan, &Val::Str("10".into()), &Val::Int(9)));
        assert!(!test(Operator::GreaterThan, &Val::Str("abc".into()), &Val::Int(9)));
        assert!(!test(Operator::GreaterThan, &Val::Bool(true), &Val::Int(0)));
    }

    #[test]
    fn equals_ignore_case_and_in() {
        assert!(test(Operator::EqualsIgnoreCase, &Val::Str("!Hello".into()), &Val::Str("!hello".into())));
        let list = Val::List(vec![Val::Str("5655".into()), Val::Int(1)]);
        assert!(test(Operator::In, &Val::Int(5655), &list));
        assert!(!test(Operator::In, &Val::Int(7), &list));
    }
}
