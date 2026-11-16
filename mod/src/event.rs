//! The normalized TikTok LIVE event (the plugin's `TikTokEventContext`).
use std::collections::BTreeMap;

use crate::value::Val;

pub const CHAT: &str = "chat";
pub const CHAT_EMOTE: &str = "chat.emote";
pub const GIFT: &str = "gift";
pub const GIFT_FINAL: &str = "gift.final";
pub const LIKE: &str = "like";
pub const MEMBER: &str = "member";
pub const SOCIAL_FOLLOW: &str = "social.follow";
pub const SOCIAL_SHARE: &str = "social.share";
pub const SOCIAL_OTHER: &str = "social.other";
pub const SOCIAL_UNKNOWN: &str = "social.unknown";
pub const ROOM_USER: &str = "room.user";
pub const UNKNOWN: &str = "unknown";

/// Every known trigger, in the plugin's order.
pub const TRIGGERS: [&str; 12] = [
    CHAT,
    CHAT_EMOTE,
    GIFT,
    GIFT_FINAL,
    LIKE,
    MEMBER,
    SOCIAL_FOLLOW,
    SOCIAL_SHARE,
    SOCIAL_OTHER,
    SOCIAL_UNKNOWN,
    ROOM_USER,
    UNKNOWN,
];

pub fn is_known_trigger(trigger: &str) -> bool {
    TRIGGERS.contains(&trigger.trim().to_ascii_lowercase().as_str())
}

#[derive(Clone, Debug)]
pub struct Event {
    pub id: String,
    pub room_id: String,
    pub kind: String,
    pub user_id: String,
    pub unique_id: String,
    pub nickname: String,
    pub timestamp: i64,
    pub history: bool,
    pub payload: BTreeMap<String, Val>,
}

impl Event {
    pub fn new(kind: &str) -> Event {
        Event {
            id: String::new(),
            room_id: String::new(),
            kind: kind.to_owned(),
            user_id: String::new(),
            unique_id: String::new(),
            nickname: String::new(),
            timestamp: 0,
            history: false,
            payload: BTreeMap::new(),
        }
    }

    pub fn with(mut self, key: &str, value: Val) -> Event {
        self.payload.insert(key.to_owned(), value);
        self
    }

    pub fn number(&self, key: &str) -> i64 {
        match self.payload.get(key) {
            Some(Val::Int(i)) => *i,
            Some(Val::Float(f)) => *f as i64,
            Some(Val::Bool(b)) => i64::from(*b),
            Some(Val::Str(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    pub fn like_count(&self) -> i64 {
        self.number("like.count")
    }

    pub fn like_total(&self) -> i64 {
        self.number("like.total")
    }

    /// The user key for cooldowns and thresholds: `uniqueId`, then `id`, then empty.
    pub fn user_key(&self) -> &str {
        if !self.unique_id.is_empty() {
            &self.unique_id
        } else {
            &self.user_id
        }
    }
}

/// The name shown for a viewer (`ChatFormatter.displayName`).
pub fn display_name(nickname: &str, unique_id: &str) -> String {
    if !nickname.trim().is_empty() {
        return nickname.trim().to_owned();
    }
    if !unique_id.trim().is_empty() {
        return format!("@{}", unique_id.trim());
    }
    "unknown".to_owned()
}

/// Removes control characters that would break a console line or a command.
pub fn safe(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\n' || c == '\r' || c == '\t' {
            out.push(' ');
        } else if (c as u32) < 0x20 || c as u32 == 0x7f {
            continue;
        } else {
            out.push(c);
        }
    }
    out
}

/// Removes what Minecraft's font draws as a square: emojis and other supplementary-plane
/// symbols, variation selectors and zero-width joiners.
pub fn displayable(text: &str) -> String {
    let kept: String = text
        .chars()
        .filter(|&c| {
            let u = c as u32;
            u < 0x10000 && !matches!(u, 0x200B..=0x200F | 0x2060..=0x2064 | 0xFE00..=0xFE0F | 0x20E3 | 0xFEFF)
        })
        .collect();
    let trimmed = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    trimmed
}

/// Truncates to `max` characters without splitting one.
pub fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
