//! Turning a burst of gift messages into one gift.
//!
//! A streakable gift does not arrive once. Holding the send button produces a run of
//! `WebcastGiftMessage`s for the same user and gift, each with a higher `repeat_count`, and
//! only the last carries the real total:
//!
//! ```text
//! repeat_count=1  repeat_end=false   ← in progress
//! repeat_count=5  repeat_end=false   ← in progress
//! repeat_count=9  repeat_end=true    ← the gift actually sent: 9
//! ```
//!
//! Treating each message as a gift reports 15 roses where 9 were sent, and multiplies the
//! diamond total in the same proportion. This tracker keeps the streak open until TikTok
//! ends it and emits exactly one [`CompletedGift`].
//!
//! Non-streakable gifts have no burst and complete on arrival.

use std::collections::HashMap;

/// A gift that finished: one send, with its true total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedGift {
    pub user_id: u64,
    pub gift_id: u64,
    /// How many were sent, taken from the final message of the streak.
    pub count: u64,
}

impl CompletedGift {
    /// Total diamond value, given the gift's unit price from the gift table.
    pub fn diamonds(&self, diamond_count: u64) -> u64 {
        diamond_count.saturating_mul(self.count)
    }
}

/// Collapses gift streaks into completed gifts.
///
/// Feed every gift event in; take the `Some` results. State is bounded by the number of
/// streaks open at once, and a streak is removed as soon as it ends.
#[derive(Debug, Default, Clone)]
pub struct GiftStreaks {
    /// Highest `repeat_count` seen per open streak, keyed by sender and gift.
    open: HashMap<(u64, u64), u64>,
}

impl GiftStreaks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one gift message and report the gift when its streak ends.
    ///
    /// `streakable` comes from the gift table ([`crate::Gift::is_streakable`]). A gift that
    /// cannot streak completes immediately, which is also the safe behaviour for a gift
    /// missing from the table: reporting it once is better than never reporting it.
    pub fn observe(
        &mut self,
        user_id: u64,
        gift_id: u64,
        repeat_count: u64,
        repeat_end: bool,
        streakable: bool,
    ) -> Option<CompletedGift> {
        let key = (user_id, gift_id);
        // A count of zero still means one gift was sent.
        let count = repeat_count.max(1);

        if !streakable {
            return Some(CompletedGift {
                user_id,
                gift_id,
                count,
            });
        }

        // TikTok can resend a lower count mid-streak; the highest seen is the truth.
        let running = self.open.entry(key).or_insert(0);
        *running = (*running).max(count);

        if repeat_end {
            let count = self.open.remove(&key).unwrap_or(count);
            return Some(CompletedGift {
                user_id,
                gift_id,
                count,
            });
        }
        None
    }

    /// Streaks still waiting for their final message.
    pub fn open_streaks(&self) -> usize {
        self.open.len()
    }

    /// Abandon open streaks, for example when the room closes.
    ///
    /// Returns what was in flight so a caller can decide to report or discard it: a viewer
    /// who leaves mid-streak did send those gifts, but TikTok never confirmed the total.
    pub fn take_open(&mut self) -> Vec<CompletedGift> {
        let mut pending: Vec<CompletedGift> = self
            .open
            .drain()
            .map(|((user_id, gift_id), count)| CompletedGift {
                user_id,
                gift_id,
                count,
            })
            .collect();
        // Deterministic order; `HashMap` iteration is not.
        pending.sort_by_key(|gift| (gift.user_id, gift.gift_id));
        pending
    }
}

