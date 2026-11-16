//! The mirror of TikTok chat in Minecraft: ingest limit, bounded queue and sanitizing.
use std::collections::VecDeque;

use crate::config::Config;
use crate::event::{displayable, display_name, safe, truncate, Event, CHAT, GIFT_FINAL, SOCIAL_FOLLOW, SOCIAL_SHARE};

pub const MAX_BROADCASTS_PER_TICK: usize = 8;

#[derive(Default)]
pub struct ChatBridge {
    pending: VecDeque<String>,
    pub received: u64,
    pub suppressed: u64,
    pub dropped: u64,
    window_start: i64,
    window_count: i64,
}

/// What the chat layer decides to do with a message.
pub struct ChatLine {
    /// Line for the server console.
    pub console: Option<String>,
}

impl ChatBridge {
    pub fn accept(&mut self, config: &Config, event: &Event, now_ms: i64) -> Option<ChatLine> {
        if !config.chat_enabled {
            return None;
        }
        let clean = |text: &str| {
            let text = safe(text);
            if config.chat_strip_unsupported { displayable(&text) } else { text }
        };
        let who = clean(&display_name(&event.nickname, &event.unique_id));
        let who = if who.is_empty() { "viewer".to_owned() } else { who };
        let (console, minecraft) = match event.kind.as_str() {
            CHAT => {
                let text = truncate(&clean(&event.payload.get("chat.text").map(|v| v.text()).unwrap_or_default()), config.chat_max_length as usize);
                if text.is_empty() {
                    return None;
                }
                (format!("Chat {who}: {text}"), format!("{who}: {text}"))
            }
            GIFT_FINAL if config.chat_show_gifts => {
                let name = clean(&event.payload.get("gift.name").map(|v| v.text()).unwrap_or_default());
                let name = if name.is_empty() { "a gift".to_owned() } else { name };
                let count = event.payload.get("gift.repeatCount").map(|v| v.text()).and_then(|t| t.parse::<i64>().ok()).unwrap_or(1).max(1);
                let diamonds = event.payload.get("gift.diamonds").map(|v| v.text()).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0);
                let value = if diamonds > 0 { format!(" ({} diamonds)", diamonds * count) } else { String::new() };
                let line = format!("{who} sent {name} x{count}{value}");
                (format!("Gift {line}"), line)
            }
            SOCIAL_FOLLOW if config.chat_show_social => {
                let line = format!("{who} followed the creator");
                (format!("Follow {line}"), line)
            }
            SOCIAL_SHARE if config.chat_show_social => {
                let line = format!("{who} shared the LIVE");
                (format!("Share {line}"), line)
            }
            _ => return None,
        };
        self.received += 1;
        if !self.allow(config, now_ms) {
            self.suppressed += 1;
            return None;
        }
        let console = config.chat_console.then_some(console);
        if config.chat_minecraft {
            self.push_system(config, &minecraft);
        }
        Some(ChatLine { console })
    }

    /// A line from the mod itself (connection state, notices) with the configured prefix.
    pub fn push_system(&mut self, config: &Config, text: &str) {
        let line = if config.chat_prefix.is_empty() { text.to_owned() } else { format!("{} {text}", config.chat_prefix) };
        if self.pending.len() as i64 >= config.chat_max_queue {
            self.dropped += 1;
        } else {
            self.pending.push_back(line);
        }
    }

    /// Up to `MAX_BROADCASTS_PER_TICK` messages ready to broadcast.
    pub fn drain(&mut self) -> Vec<String> {
        let n = self.pending.len().min(MAX_BROADCASTS_PER_TICK);
        self.pending.drain(..n).collect()
    }

    pub fn queued(&self) -> usize {
        self.pending.len()
    }

    pub fn take_suppressed(&mut self) -> u64 {
        std::mem::take(&mut self.suppressed)
    }

    fn allow(&mut self, config: &Config, now_ms: i64) -> bool {
        if now_ms - self.window_start >= 1000 {
            self.window_start = now_ms;
            self.window_count = 0;
        }
        if self.window_count >= config.chat_messages_per_second {
            return false;
        }
        self.window_count += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Val;

    #[test]
    fn mirrors_chat_with_prefix_and_rate_limit() {
        let config = Config::parse(crate::config::DEFAULT_CONFIG).unwrap();
        let mut bridge = ChatBridge::default();
        let mut event = Event::new("chat");
        event.nickname = "Ana".into();
        let event = event.with("chat.text", Val::Str("hello\nworld".into()));
        for _ in 0..8 {
            bridge.accept(&config, &event, 5_000);
        }
        assert_eq!(bridge.suppressed, 3, "5 messages per second by default");
        let lines = bridge.drain();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "[TikTok] Ana: hello world");
    }

    #[test]
    fn emojis_are_stripped_and_gifts_and_follows_are_shown() {
        let config = Config::parse(crate::config::DEFAULT_CONFIG).unwrap();
        let mut bridge = ChatBridge::default();
        let mut chat = Event::new("chat");
        chat.nickname = "Kevin❤️😀".into();
        bridge.accept(&config, &chat.with("chat.text", Val::Str("hello 😀 world".into())), 1_000);
        let mut gift = Event::new("gift.final");
        gift.nickname = "Ana".into();
        let gift = gift
            .with("gift.name", Val::Str("Rose".into()))
            .with("gift.repeatCount", Val::Int(3))
            .with("gift.diamonds", Val::Int(1));
        bridge.accept(&config, &gift, 1_000);
        let mut follow = Event::new("social.follow");
        follow.nickname = "Luis".into();
        bridge.accept(&config, &follow, 1_000);
        bridge.accept(&config, &Event::new("like"), 1_000);
        assert_eq!(
            bridge.drain(),
            vec![
                "[TikTok] Kevin❤: hello world".to_owned(),
                "[TikTok] Ana sent Rose x3 (3 diamonds)".to_owned(),
                "[TikTok] Luis followed the creator".to_owned(),
            ]
        );
    }
}
