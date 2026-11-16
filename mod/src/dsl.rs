//! The optional `.ttl` DSL: it compiles to exactly the same rule model as YAML.
//! There is no other execution path: a script can only write `cooldown`, `call` and `broadcast`.
use std::collections::BTreeMap;

use crate::rules::compare::Operator;
use crate::rules::loader::{parse_duration_ms, ActionCatalog};
use crate::rules::model::{
    ActionSpec, AggScope, Aggregate, Condition, CooldownScope, CooldownSpec, EventRule, FieldMatcher, Metric,
};
use crate::rules::fields;
use crate::value::Val;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DslError {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
    pub expected: Option<String>,
}

impl std::fmt::Display for DslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.file.is_empty() {
            write!(f, "{}:", self.file)?;
        }
        write!(f, "{}:{}: {}", self.line, self.column, self.message)?;
        if let Some(expected) = self.expected.as_ref().filter(|e| !e.is_empty()) {
            write!(f, "; expected {expected}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    Comma,
    Assign,
    Equal,
    NotEqual,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
    Str,
    Number,
    Duration,
    Identifier,
    End,
}

#[derive(Debug, Clone)]
struct Token {
    kind: Kind,
    text: String,
    line: usize,
    column: usize,
}

impl Token {
    fn is_keyword(&self, word: &str) -> bool {
        self.kind == Kind::Identifier && self.text == word
    }
}

struct Lexer<'a> {
    chars: Vec<char>,
    index: usize,
    line: usize,
    column: usize,
    file: &'a str,
}

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || matches!(c, '_' | '.' | ':' | '-')
}

fn ident_part(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '-')
}

impl Lexer<'_> {
    fn error(&self, line: usize, column: usize, message: &str, expected: Option<&str>) -> DslError {
        DslError { file: self.file.to_owned(), line, column, message: message.to_owned(), expected: expected.map(str::to_owned) }
    }

    fn advance(&mut self) {
        self.index += 1;
        self.column += 1;
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                self.index += 1;
                self.line += 1;
                self.column = 1;
            } else if c.is_whitespace() {
                self.advance();
            } else if c == '#' {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.advance();
                }
            } else {
                return;
            }
        }
    }

    fn tokens(mut self) -> Result<Vec<Token>, DslError> {
        let mut out = Vec::new();
        loop {
            self.skip_whitespace();
            if self.index >= self.chars.len() {
                out.push(Token { kind: Kind::End, text: String::new(), line: self.line, column: self.column });
                return Ok(out);
            }
            out.push(self.token()?);
        }
    }

    fn token(&mut self) -> Result<Token, DslError> {
        let (line, column) = (self.line, self.column);
        let c = self.chars[self.index];
        let make = |kind, text: &str| Token { kind, text: text.to_owned(), line, column };
        let simple = match c {
            '(' => Some(make(Kind::LeftParen, "(")),
            ')' => Some(make(Kind::RightParen, ")")),
            '{' => Some(make(Kind::LeftBrace, "{")),
            '}' => Some(make(Kind::RightBrace, "}")),
            ',' => Some(make(Kind::Comma, ",")),
            _ => None,
        };
        if let Some(token) = simple {
            self.advance();
            return Ok(token);
        }
        match c {
            '=' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    return Ok(make(Kind::Equal, "=="));
                }
                Ok(make(Kind::Assign, "="))
            }
            '!' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    return Ok(make(Kind::NotEqual, "!="));
                }
                Err(self.error(line, column, "'!' is only valid as '!='", Some("'!='")))
            }
            '>' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    return Ok(make(Kind::GreaterOrEqual, ">="));
                }
                Ok(make(Kind::Greater, ">"))
            }
            '<' => {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    return Ok(make(Kind::LessOrEqual, "<="));
                }
                Ok(make(Kind::Less, "<"))
            }
            '"' | '\'' => self.string(c, line, column),
            c if c.is_ascii_digit() => self.number(line, column),
            c if ident_start(c) => {
                let mut text = String::new();
                while self.peek().is_some_and(ident_part) {
                    text.push(self.chars[self.index]);
                    self.advance();
                }
                Ok(Token { kind: Kind::Identifier, text, line, column })
            }
            other => Err(self.error(line, column, &format!("Unexpected character '{other}'"), None)),
        }
    }

    fn string(&mut self, quote: char, line: usize, column: usize) -> Result<Token, DslError> {
        self.advance();
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if c == '\\' && self.index + 1 < self.chars.len() {
                let escaped = self.chars[self.index + 1];
                match escaped {
                    'n' => text.push('\n'),
                    't' => text.push('\t'),
                    '\\' => text.push('\\'),
                    '"' => text.push('"'),
                    '\'' => text.push('\''),
                    other => {
                        text.push('\\');
                        text.push(other);
                    }
                }
                self.advance();
                self.advance();
                continue;
            }
            if c == quote {
                self.advance();
                return Ok(Token { kind: Kind::Str, text, line, column });
            }
            if c == '\n' {
                break;
            }
            text.push(c);
            self.advance();
        }
        Err(self.error(line, column, "The string is not closed", Some("a closing quote")))
    }

    fn number(&mut self, line: usize, column: usize) -> Result<Token, DslError> {
        let mut digits = String::new();
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            digits.push(self.chars[self.index]);
            self.advance();
        }
        if self.peek().is_some_and(ident_start) {
            let mut unit = String::new();
            while self.peek().is_some_and(ident_part) {
                unit.push(self.chars[self.index]);
                self.advance();
            }
            if ["ms", "s", "m", "h", "d"].contains(&unit.as_str()) {
                return Ok(Token { kind: Kind::Duration, text: format!("{digits}{unit}"), line, column });
            }
            return Err(self.error(line, column, &format!("Unknown unit '{unit}'"), Some("ms, s, m, h or d")));
        }
        Ok(Token { kind: Kind::Number, text: digits, line, column })
    }
}

struct Comparison {
    field: String,
    operator: String,
    value: Val,
}

struct Condition_ {
    comparisons: Vec<Comparison>,
    connectors: Vec<String>,
}

struct Threshold {
    value: i64,
    scope: String,
}

enum Statement {
    Cooldown { duration: String, scope: String },
    Call { function: String, args: BTreeMap<String, Val> },
    Broadcast(String),
}

struct RuleAst {
    trigger: String,
    condition: Option<Condition_>,
    threshold: Option<Threshold>,
    statements: Vec<Statement>,
}

struct Parser<'a> {
    tokens: Vec<Token>,
    index: usize,
    file: &'a str,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.tokens[self.index.min(self.tokens.len() - 1)]
    }

    fn next(&mut self) -> Token {
        let token = self.peek().clone();
        if self.index < self.tokens.len() - 1 {
            self.index += 1;
        }
        token
    }

    fn error(&self, token: &Token, message: &str, expected: &str) -> DslError {
        DslError { file: self.file.to_owned(), line: token.line, column: token.column, message: message.to_owned(), expected: Some(expected.to_owned()) }
    }

    fn expect(&mut self, kind: Kind, expected: &str) -> Result<Token, DslError> {
        if self.peek().kind != kind {
            let token = self.peek().clone();
            return Err(self.error(&token, &format!("Unexpected '{}'", token.text), expected));
        }
        Ok(self.next())
    }

    fn script(&mut self) -> Result<Vec<RuleAst>, DslError> {
        let mut rules = Vec::new();
        while self.peek().kind != Kind::End {
            rules.push(self.rule()?);
        }
        Ok(rules)
    }

    fn rule(&mut self) -> Result<RuleAst, DslError> {
        let on = self.peek().clone();
        if !on.is_keyword("on") {
            return Err(self.error(&on, &format!("Unexpected '{}'", on.text), "'on'"));
        }
        self.next();
        let trigger = self.expect(Kind::Identifier, "a trigger name")?;
        let mut condition = None;
        if self.peek().is_keyword("where") {
            self.next();
            condition = Some(self.condition()?);
        }
        let mut threshold = None;
        if self.peek().is_keyword("every") {
            self.next();
            threshold = Some(self.threshold()?);
        }
        self.expect(Kind::LeftBrace, "'{'")?;
        let mut statements = Vec::new();
        while self.peek().kind != Kind::RightBrace {
            if self.peek().kind == Kind::End {
                let token = self.peek().clone();
                return Err(self.error(&token, "The rule is not closed", "'}'"));
            }
            statements.push(self.statement()?);
        }
        self.expect(Kind::RightBrace, "'}'")?;
        if statements.is_empty() {
            return Err(self.error(&on, "The rule has no statements", "at least one statement"));
        }
        Ok(RuleAst { trigger: trigger.text, condition, threshold, statements })
    }

    fn condition(&mut self) -> Result<Condition_, DslError> {
        let mut comparisons = vec![self.comparison()?];
        let mut connectors = Vec::new();
        while self.peek().is_keyword("and") || self.peek().is_keyword("or") {
            connectors.push(self.next().text);
            comparisons.push(self.comparison()?);
        }
        Ok(Condition_ { comparisons, connectors })
    }

    fn comparison(&mut self) -> Result<Comparison, DslError> {
        let field = self.expect(Kind::Identifier, "a field name")?;
        let operator = self.next();
        if !matches!(operator.kind, Kind::Equal | Kind::NotEqual | Kind::Greater | Kind::GreaterOrEqual | Kind::Less | Kind::LessOrEqual) {
            return Err(self.error(&operator, &format!("Unsupported comparison '{}'", operator.text), "'==', '!=', '>', '>=', '<' or '<='"));
        }
        let value = self.next();
        let parsed = match value.kind {
            Kind::Number => Val::Int(value.text.parse().unwrap_or(i64::MAX)),
            Kind::Str | Kind::Identifier => Val::Str(value.text.clone()),
            _ => return Err(self.error(&value, "A comparison needs a value", "a string, a number or a word")),
        };
        Ok(Comparison { field: field.text, operator: operator.text, value: parsed })
    }

    /// `per user`, `per user and rule`, `global`, `user`: the scope after `every N` or `cooldown`.
    fn scope(&mut self, default: &str, allow_bare_user: bool) -> Result<String, DslError> {
        if self.peek().is_keyword("per") {
            self.next();
            let word = self.expect(Kind::Identifier, "a scope")?;
            let mut scope = word.text;
            if self.peek().is_keyword("and") {
                self.next();
                let second = self.expect(Kind::Identifier, "a scope")?;
                if second.text != "rule" {
                    return Err(self.error(&second, "Only 'rule' can follow 'and'", "'rule'"));
                }
                scope = "user-and-rule".to_owned();
            }
            return Ok(scope);
        }
        if self.peek().kind == Kind::Identifier && (self.peek().text == "global" || (allow_bare_user && self.peek().text == "user")) {
            return Ok(self.next().text);
        }
        Ok(default.to_owned())
    }

    fn threshold(&mut self) -> Result<Threshold, DslError> {
        let count = self.expect(Kind::Number, "a number")?;
        let scope = self.scope("global", true)?;
        let value = count.text.parse().map_err(|_| self.error(&count, "The threshold is not a whole number", "a whole number"))?;
        Ok(Threshold { value, scope })
    }

    fn statement(&mut self) -> Result<Statement, DslError> {
        let keyword = self.expect(Kind::Identifier, "a statement")?;
        match keyword.text.as_str() {
            "cooldown" => {
                let duration = self.expect(Kind::Duration, "a duration such as '10s'")?;
                let scope = self.scope("global", false)?;
                Ok(Statement::Cooldown { duration: duration.text, scope })
            }
            "broadcast" => Ok(Statement::Broadcast(self.expect(Kind::Str, "a message in quotes")?.text)),
            "call" => {
                let function = self.expect(Kind::Identifier, "a function id")?;
                let mut args = BTreeMap::new();
                if self.peek().kind == Kind::LeftParen {
                    self.next();
                    if self.peek().kind != Kind::RightParen {
                        loop {
                            let name = self.expect(Kind::Identifier, "an argument name")?;
                            self.expect(Kind::Assign, "'='")?;
                            let value = self.next();
                            let parsed = match value.kind {
                                Kind::Number => Val::Int(value.text.parse().unwrap_or(i64::MAX)),
                                Kind::Str | Kind::Identifier => Val::Str(value.text.clone()),
                                _ => return Err(self.error(&value, "An argument needs a value", "a string, a number or a word")),
                            };
                            args.insert(name.text, parsed);
                            if self.peek().kind == Kind::Comma {
                                self.next();
                                continue;
                            }
                            break;
                        }
                    }
                    self.expect(Kind::RightParen, "')'")?;
                }
                Ok(Statement::Call { function: function.text, args })
            }
            other => Err(self.error(&keyword, &format!("Unknown statement '{other}'"), "'cooldown', 'call' or 'broadcast'")),
        }
    }
}

fn stem(file: &str) -> String {
    let name = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let name = name.strip_suffix(".ttl").unwrap_or(name);
    let stem: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '.' }).collect();
    if stem.is_empty() { "script".to_owned() } else { stem }
}

fn plain(file: &str, message: String) -> DslError {
    DslError { file: file.to_owned(), line: 1, column: 1, message, expected: None }
}

fn build_condition(file: &str, source: &Condition_) -> Result<Condition, DslError> {
    let mut groups: Vec<Vec<&Comparison>> = Vec::new();
    let mut current: Vec<&Comparison> = Vec::new();
    for (index, comparison) in source.comparisons.iter().enumerate() {
        if index > 0 && source.connectors[index - 1] == "or" {
            groups.push(std::mem::take(&mut current));
        }
        current.push(comparison);
    }
    groups.push(current);
    let mut alternatives = Vec::new();
    for group in groups {
        let mut leaves = Vec::new();
        for comparison in group {
            if !fields::is_known(&comparison.field) {
                return Err(plain(file, format!("unknown field '{}'", comparison.field)));
            }
            let operator = Operator::parse(&comparison.operator).ok_or_else(|| plain(file, format!("unsupported comparison '{}'", comparison.operator)))?;
            leaves.push(Condition::Leaf(FieldMatcher::new(&comparison.field, operator, comparison.value.clone())));
        }
        alternatives.push(Condition::All(leaves));
    }
    Ok(if alternatives.len() == 1 { alternatives.remove(0) } else { Condition::Any(alternatives) })
}

/// Compiles a script. Rules are named `<file>.<n>`.
pub fn compile(file: &str, source: &str, catalog: &dyn ActionCatalog) -> Result<Vec<EventRule>, DslError> {
    let lexer = Lexer { chars: source.chars().collect(), index: 0, line: 1, column: 1, file };
    let tokens = lexer.tokens()?;
    let mut parser = Parser { tokens, index: 0, file };
    let parsed = parser.script()?;
    let stem = stem(file);
    let mut rules = Vec::new();
    for (index, rule) in parsed.iter().enumerate() {
        let id = format!("{stem}.{}", index + 1);
        let trigger = rule.trigger.to_ascii_lowercase();
        if !crate::event::is_known_trigger(&trigger) {
            return Err(plain(file, format!("unknown trigger '{}'", rule.trigger)));
        }
        let condition = rule.condition.as_ref().map(|c| build_condition(file, c)).transpose()?;
        let aggregate = match &rule.threshold {
            None => None,
            Some(threshold) => {
                let scope = AggScope::parse(&threshold.scope).ok_or_else(|| plain(file, format!("unknown scope '{}'", threshold.scope)))?;
                if threshold.value <= 0 {
                    return Err(plain(file, "the threshold must be positive".to_owned()));
                }
                if trigger != "like" {
                    return Err(plain(file, "'every' can only be used with the 'like' trigger".to_owned()));
                }
                Some(Aggregate { metric: Metric::Count, scope, threshold: threshold.value, carry_remainder: true })
            }
        };
        let mut cooldown = None;
        let mut actions = Vec::new();
        for statement in &rule.statements {
            match statement {
                Statement::Cooldown { duration, scope } => {
                    let ms = parse_duration_ms(duration);
                    if ms < 0 {
                        return Err(plain(file, format!("'{duration}' is not a valid duration")));
                    }
                    let scope = CooldownScope::parse(scope).ok_or_else(|| plain(file, format!("unknown cooldown scope '{scope}'")))?;
                    cooldown = Some(CooldownSpec { duration_ms: ms, scope });
                }
                Statement::Call { function, args } => {
                    let mut spec = ActionSpec::new("function");
                    spec.params.insert("name".to_owned(), Val::Str(function.clone()));
                    if !args.is_empty() {
                        spec.params.insert("args".to_owned(), Val::Map(args.clone()));
                    }
                    actions.push(spec);
                }
                Statement::Broadcast(message) => {
                    let mut spec = ActionSpec::new("broadcast");
                    spec.params.insert("message".to_owned(), Val::Str(message.clone()));
                    actions.push(spec);
                }
            }
        }
        if actions.is_empty() {
            return Err(plain(file, "the rule has no actions".to_owned()));
        }
        for spec in &actions {
            catalog.validate(spec).map_err(|message| plain(file, message))?;
        }
        rules.push(EventRule {
            id,
            enabled: true,
            trigger,
            matchers: Vec::new(),
            aggregate,
            cooldown,
            condition,
            actions,
            priority: 0,
            stop_processing: false,
            source: file.to_owned(),
        });
    }
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Catalog;
    impl ActionCatalog for Catalog {
        fn contains(&self, _kind: &str) -> bool {
            true
        }
        fn type_list(&self) -> String {
            String::new()
        }
        fn validate(&self, _spec: &ActionSpec) -> Result<(), String> {
            Ok(())
        }
    }

    const EXAMPLE: &str = r#"
# comentario
on chat where chat.text == "!hello" {
    cooldown 10s per user
    broadcast "Hello from TikTok!"
}
on gift.final where gift.name == "Rose" {
    cooldown 2s per user
    call gifts:spawn_zombies(count = 1)
}
on like every 50 global {
    call gifts:spawn_chicken()
}
on social.follow {
    call effects:welcome()
}
"#;

    #[test]
    fn the_shipped_example_compiles_with_generated_ids() {
        let rules = compile("example.ttl", EXAMPLE, &Catalog).unwrap();
        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["example.1", "example.2", "example.3", "example.4"]);
        assert_eq!(rules[0].cooldown.as_ref().unwrap().scope, CooldownScope::User);
        assert_eq!(rules[0].cooldown.as_ref().unwrap().duration_ms, 10_000);
        let aggregate = rules[2].aggregate.as_ref().unwrap();
        assert_eq!((aggregate.threshold, aggregate.scope, aggregate.carry_remainder), (50, AggScope::Global, true));
        assert_eq!(rules[1].actions[0].kind, "function");
        assert_eq!(rules[1].actions[0].params["args"], Val::Map([("count".to_owned(), Val::Int(1))].into()));
    }

    #[test]
    fn and_or_build_groups() {
        let rules = compile("c.ttl", r#"on gift where diamonds >= 10 and gift.name == "Rose" or gift.id == 5655 { broadcast "x" }"#, &Catalog).unwrap();
        assert!(matches!(rules[0].condition, Some(Condition::Any(ref groups)) if groups.len() == 2));
    }

    #[test]
    fn errors_name_file_line_column_and_what_was_expected() {
        let error = compile("example.ttl", "on chat {\n  cooldown 10s\n  nonsense \"x\"\n}\n", &Catalog).unwrap_err();
        assert_eq!(error.to_string(), "example.ttl:3:3: Unknown statement 'nonsense'; expected 'cooldown', 'call' or 'broadcast'");
        assert!(compile("a.ttl", "on chat { broadcast \"open", &Catalog).unwrap_err().to_string().contains("The string is not closed"));
        assert!(compile("a.ttl", "on chat where text ~ 3 { broadcast \"x\" }", &Catalog).is_err());
        assert!(compile("a.ttl", "on chat { }", &Catalog).unwrap_err().to_string().contains("no statements"));
        assert!(compile("a.ttl", "on nope { broadcast \"x\" }", &Catalog).unwrap_err().to_string().contains("unknown trigger"));
    }
}
