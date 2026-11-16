//! Reads an event's normalized fields by name and fills `{field}` templates.
use crate::event::{display_name, Event};
use crate::value::Val;

pub const ALIASES: [(&str, &str); 23] = [
    ("text", "chat.text"),
    ("comment", "chat.text"),
    ("emotes", "chat.emotes"),
    ("emote-only", "chat.emoteOnly"),
    ("gift-name", "gift.name"),
    ("gift-id", "gift.id"),
    ("diamonds", "gift.diamonds"),
    ("gift-diamonds", "gift.diamonds"),
    ("repeat-count", "gift.repeatCount"),
    ("combo-count", "gift.comboCount"),
    ("group-id", "gift.groupId"),
    ("repeat-end", "gift.repeatEnd"),
    ("like-count", "like.count"),
    ("like-total", "like.total"),
    ("kind", "social.kind"),
    ("social-action", "social.action"),
    ("follow-count", "social.followCount"),
    ("share-count", "social.shareCount"),
    ("nickname", "user.nickname"),
    ("unique-id", "user.uniqueId"),
    ("user-id", "user.id"),
    ("member-count", "member.count"),
    ("viewers", "room.viewers"),
];

/// Resolves an alias to its canonical path, or returns the path unchanged.
pub fn resolve(field: &str) -> String {
    let key = field.trim().to_ascii_lowercase();
    for (alias, path) in ALIASES {
        if alias == key {
            return path.to_owned();
        }
    }
    field.trim().to_owned()
}

pub fn is_known(field: &str) -> bool {
    let path = resolve(field);
    ["event.", "user.", "chat.", "gift.", "like.", "social.", "room.", "member.", "unknown."]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// Reads a field, or `None` when the event does not carry it.
pub fn read(event: &Event, field: &str) -> Option<Val> {
    let path = resolve(field);
    if let Some(name) = path.strip_prefix("event.") {
        return match name {
            "type" => Some(Val::Str(event.kind.clone())),
            "id" => Some(Val::Str(event.id.clone())),
            "roomId" => Some(Val::Str(event.room_id.clone())),
            "timestamp" => Some(Val::Int(event.timestamp)),
            "isHistory" => Some(Val::Bool(event.history)),
            _ => None,
        };
    }
    if let Some(name) = path.strip_prefix("user.") {
        match name {
            "id" => return Some(Val::Str(event.user_id.clone())),
            "uniqueId" => return Some(Val::Str(event.unique_id.clone())),
            "nickname" => return Some(Val::Str(event.nickname.clone())),
            _ => {}
        }
    }
    event.payload.get(&path).cloned()
}

/// Fills `{field}` with event data. A missing field is an empty string; an unknown
/// name is left untouched so that a typo is noticed.
pub fn render(template: &str, event: &Event) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close_rel) = rest[open + 1..].find('}') else {
            break;
        };
        let close = open + 1 + close_rel;
        out.push_str(&rest[..open]);
        let name = rest[open + 1..close].trim();
        let value = if name.is_empty() {
            None
        } else if name.eq_ignore_ascii_case("viewer") {
            Some(display_name(&event.nickname, &event.unique_id))
        } else if is_known(name) {
            Some(read(event, name).map(|v| v.text()).unwrap_or_default())
        } else {
            None
        };
        match value {
            Some(text) => out.push_str(&text),
            None => out.push_str(&rest[open..=close]),
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_and_templates_render() {
        let mut event = Event::new("gift");
        event.nickname = "Ana".into();
        let event = event.with("gift.name", Val::Str("Rose".into()));
        assert_eq!(resolve("gift-name"), "gift.name");
        assert_eq!(render("{gift.name} from {viewer}!", &event), "Rose from Ana!");
        assert_eq!(render("{typo} {gift.diamonds}", &event), "{typo} ");
    }
}
