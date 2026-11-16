//! Text-level editing of `config.yml`, keeping comments (the commands that change the
//! configuration cannot rewrite the whole file with a YAML emitter).

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn is_blank_or_comment(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with('#')
}

/// `(first line of the body, exclusive end)` of the top-level `section:` block.
fn section_range(lines: &[String], section: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| indent_of(l) == 0 && l.trim_end() == format!("{section}:"))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| !is_blank_or_comment(l) && indent_of(l) == 0)
        .map_or(lines.len(), |(i, _)| i);
    Some((start + 1, end))
}

fn find_key(lines: &[String], range: (usize, usize), key: &str) -> Option<usize> {
    (range.0..range.1).find(|&i| indent_of(&lines[i]) == 2 && lines[i].trim_start().starts_with(&format!("{key}:")))
}

/// End (exclusive) of a key's value: its more indented lines and the child `- ` lines.
fn value_end(lines: &[String], key_line: usize, limit: usize) -> usize {
    let mut end = key_line + 1;
    while end < limit && (indent_of(&lines[end]) > 2 || (indent_of(&lines[end]) == 2 && lines[end].trim_start().starts_with("- "))) {
        end += 1;
    }
    end
}

fn finish(lines: Vec<String>, original: &str) -> String {
    let mut text = lines.join("\n");
    if original.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// A YAML text that is safe as a scalar: double quotes with escapes.
pub fn quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}

/// Changes the value of `section.key` (indented two spaces), keeping a trailing comment.
pub fn set_scalar(text: &str, section: &str, key: &str, value: &str) -> Option<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let range = section_range(&lines, section)?;
    let at = find_key(&lines, range, key)?;
    let line = lines[at].clone();
    let comment = line.split_once(" #").map(|(_, c)| format!(" #{c}")).unwrap_or_default();
    lines[at] = format!("  {key}: {value}{comment}");
    // If the previous value was a list, its items are removed.
    let end = value_end(&lines, at, range.1);
    lines.drain(at + 1..end);
    Some(finish(lines, text))
}

/// Inserts `line` at the end of `section`'s body (creating it if missing), before its trailing comments.
fn insert_line(text: &str, section: &str, new_lines: &[String]) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    match section_range(&lines, section) {
        Some((start, end)) => {
            let mut at = end;
            while at > start && is_blank_or_comment(&lines[at - 1]) {
                at -= 1;
            }
            for (i, l) in new_lines.iter().enumerate() {
                lines.insert(at + i, l.clone());
            }
        }
        None => {
            lines.push(String::new());
            lines.push(format!("{section}:"));
            lines.extend(new_lines.iter().cloned());
        }
    }
    finish(lines, text)
}

/// Like `set_scalar`, but if the key is not in the file (a `config.yml` from an earlier
/// version) it is added to its section.
pub fn upsert_scalar(text: &str, section: &str, key: &str, value: &str) -> Option<String> {
    Some(set_scalar(text, section, key, value).unwrap_or_else(|| insert_line(text, section, &[format!("  {key}: {value}")])))
}

/// Like `set_list`, adding the key if it is missing.
pub fn upsert_list(text: &str, section: &str, key: &str, items: &[String]) -> Option<String> {
    if let Some(done) = set_list(text, section, key, items) {
        return Some(done);
    }
    let mut new_lines = Vec::new();
    if items.is_empty() {
        new_lines.push(format!("  {key}: []"));
    } else {
        new_lines.push(format!("  {key}:"));
        new_lines.extend(items.iter().map(|i| format!("    - {}", quote(i))));
    }
    Some(insert_line(text, section, &new_lines))
}

/// Replaces a key with a list of items.
pub fn set_list(text: &str, section: &str, key: &str, items: &[String]) -> Option<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let range = section_range(&lines, section)?;
    let at = find_key(&lines, range, key)?;
    let end = value_end(&lines, at, range.1);
    let comment = lines[at].split_once(" #").map(|(_, c)| format!(" #{c}")).unwrap_or_default();
    let mut replacement = Vec::new();
    if items.is_empty() {
        replacement.push(format!("  {key}: []{comment}"));
    } else {
        replacement.push(format!("  {key}:{comment}"));
        replacement.extend(items.iter().map(|i| format!("    - {}", quote(i))));
    }
    lines.splice(at..end, replacement);
    Some(finish(lines, text))
}

/// Rewrites `tiktok.accounts` (additional accounts: `(unique-id, target-player)`).
pub fn set_accounts(text: &str, accounts: &[(String, String)]) -> Option<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let range = section_range(&lines, "tiktok")?;
    let at = find_key(&lines, range, "accounts")?;
    let end = value_end(&lines, at, range.1);
    let mut replacement = Vec::new();
    if accounts.is_empty() {
        replacement.push("  accounts: []".to_owned());
    } else {
        replacement.push("  accounts:".to_owned());
        for (id, target) in accounts {
            replacement.push(format!("    - unique-id: {}", quote(&format!("@{id}"))));
            replacement.push(format!("      target-player: {}", quote(target)));
        }
    }
    lines.splice(at..end, replacement);
    Some(finish(lines, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DEFAULT_CONFIG};

    #[test]
    fn missing_keys_are_added_to_their_section() {
        let text = "chat:\n  enabled: true\n  # nota\n\nevents:\n  enabled: true\n";
        let out = upsert_scalar(text, "chat", "show-gifts", "false").unwrap();
        assert_eq!(out, "chat:\n  enabled: true\n  show-gifts: false\n  # nota\n\nevents:\n  enabled: true\n");
        let out = upsert_scalar(text, "new", "key", "1").unwrap();
        assert!(out.ends_with("new:\n  key: 1\n"));
        let out = upsert_list(text, "events", "extra", &["a".into()]).unwrap();
        assert!(out.contains("  extra:\n    - \"a\""));
        assert_eq!(upsert_scalar(text, "chat", "enabled", "false").unwrap(), text.replace("enabled: true\n  # nota", "enabled: false\n  # nota"));
    }

    #[test]
    fn scalars_keep_comments_and_lists_are_replaced() {
        let text = set_scalar(DEFAULT_CONFIG, "security", "execute-live-actions", "true").unwrap();
        assert!(Config::parse(&text).unwrap().execute_live_actions);
        let text = set_list(&text, "actions", "allowed-entities", &["*".to_owned()]).unwrap();
        let config = Config::parse(&text).unwrap();
        assert_eq!(config.allowed_entities, vec!["*".to_owned()]);
        assert!(text.contains("# Entity types an action may spawn"), "comments survive");
        let text = set_scalar(&text, "actions", "max-entities-per-event", "64").unwrap();
        assert_eq!(Config::parse(&text).unwrap().max_entities_per_event, 64);
    }

    #[test]
    fn accounts_roundtrip() {
        let accounts = vec![("amigo".to_owned(), "Steve".to_owned()), ("otro".to_owned(), String::new())];
        let text = set_accounts(DEFAULT_CONFIG, &accounts).unwrap();
        let config = Config::parse(&text).unwrap();
        assert_eq!(config.accounts.len(), 3);
        assert_eq!(config.accounts[1].unique_id, "amigo");
        assert_eq!(config.accounts[1].target_player, "Steve");
        let back = set_accounts(&text, &[]).unwrap();
        assert_eq!(Config::parse(&back).unwrap().accounts.len(), 1);
    }
}
