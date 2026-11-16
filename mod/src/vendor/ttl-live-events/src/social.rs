//! Canonical classification of `WebcastSocialMessage`.
//!
//! TikTok delivers follows and shares on the *same* message type and tells
//! them apart with the numeric `action` code. Two values are confirmed against
//! captured traffic (`1` follow, `3` share); every other value stays
//! unclassified so a new TikTok code is *observable* instead of being silently
//! counted as a follow or a share.
//!
//! Classification is deliberately driven by `action` alone. The counters are
//! room-wide running totals, not per-event increments:
//!
//! * `follow_count` is the current follower total and TikTok still sends a
//!   non-zero value on *share* messages.
//! * `share_count` is likewise cumulative.
//!
//! Inferring from the counters (`follow_count > 0` ⇒ follow) therefore
//! mislabels every share in a room that has any follower. That inference bug
//! existed in the Android client and is the reason this module exists.
//!
//! The auxiliary fields (`share_type`, `share_target`, `share_display_style`,
//! `follow_type`, `scene`, `target_user_id`) are attached to the classification
//! as *evidence* only. None of them changes the kind yet: a rule may only be
//! added once anonymised captures validate what a value means. Until then an
//! unrecognised value produces an unclassified event with its raw code intact
//! so the operator can see it.

use serde::{Deserialize, Serialize};

use crate::event::SocialEvent;

/// Confirmed `WebcastSocialMessage.action` values.
pub mod action {
    /// Confirmed follow action (`action == 1`).
    pub const FOLLOW: i64 = 1;
    /// Confirmed share action (`action == 3`).
    pub const SHARE: i64 = 3;
}

/// What a social message semantically is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialKind {
    /// A viewer followed the creator.
    Follow,
    /// A viewer shared the LIVE.
    Share,
    /// A social message with an `action` code we have observed but not yet
    /// confirmed. Never folded into [`SocialKind::Follow`] or
    /// [`SocialKind::Share`].
    Other,
    /// A social message with no usable `action` code. With proto3 scalars an
    /// explicit `0` and an absent field are indistinguishable on the wire, so
    /// `0` is reported as unknown rather than guessed.
    Unknown,
}

/// How much we trust a [`SocialKind`].
///
/// This is evidence provenance, *not* a made-up probability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocialConfidence {
    /// Mapped from a value validated against captured traffic.
    Confirmed,
    /// Derived from a rule that has some evidence but is not yet validated.
    Inferred,
    /// No confirmed rule matches; the raw code is preserved for diagnosis.
    Unknown,
}

/// One observed field that contributed to (or merely accompanies) a
/// classification. Serialised as `{"field": "...", "value": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "snake_case")]
pub enum SocialEvidence {
    /// The `action` field, always present (even when `0`).
    Action(i64),
    /// `share_type`, when non-zero.
    ShareType(i64),
    /// `share_target`, when non-empty.
    ShareTarget(String),
    /// `share_display_style`, when non-zero.
    ShareDisplayStyle(i64),
    /// `follow_type`, when non-zero.
    FollowType(i32),
    /// `scene`, when non-empty.
    Scene(String),
    /// `target_user_id`, when non-zero.
    TargetUserId(i64),
}

/// The result of classifying one social message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SocialClassification {
    pub kind: SocialKind,
    pub confidence: SocialConfidence,
    /// The raw `action` value exactly as it arrived.
    pub raw_action: i64,
    /// All observed fields, in a stable order.
    pub evidence: Vec<SocialEvidence>,
}

/// Classify a raw `action` code. This is the single mapping table every
/// language binding must agree with.
pub fn classify_action(raw_action: i64) -> SocialClassification {
    let (kind, confidence) = match raw_action {
        action::FOLLOW => (SocialKind::Follow, SocialConfidence::Confirmed),
        action::SHARE => (SocialKind::Share, SocialConfidence::Confirmed),
        0 => (SocialKind::Unknown, SocialConfidence::Unknown),
        _ => (SocialKind::Other, SocialConfidence::Unknown),
    };
    SocialClassification {
        kind,
        confidence,
        raw_action,
        evidence: vec![SocialEvidence::Action(raw_action)],
    }
}

impl SocialEvent {
    /// Classify this message from its confirmed `action`, attaching every other
    /// observed social field as evidence.
    pub fn classify(&self) -> SocialClassification {
        let mut classification = classify_action(self.action);
        if self.share_type != 0 {
            classification
                .evidence
                .push(SocialEvidence::ShareType(self.share_type));
        }
        if !self.share_target.is_empty() {
            classification
                .evidence
                .push(SocialEvidence::ShareTarget(self.share_target.clone()));
        }
        if self.share_display_style != 0 {
            classification
                .evidence
                .push(SocialEvidence::ShareDisplayStyle(self.share_display_style));
        }
        if self.follow_type != 0 {
            classification
                .evidence
                .push(SocialEvidence::FollowType(self.follow_type));
        }
        if !self.scene.is_empty() {
            classification
                .evidence
                .push(SocialEvidence::Scene(self.scene.clone()));
        }
        if self.target_user_id != 0 {
            classification
                .evidence
                .push(SocialEvidence::TargetUserId(self.target_user_id));
        }
        classification
    }

    /// Convenience: the classified kind on its own.
    pub fn kind(&self) -> SocialKind {
        self.classify().kind
    }

    /// Convenience: whether this message is an *unclassified* social action
    /// (not a confirmed follow or share).
    pub fn is_unclassified(&self) -> bool {
        !matches!(self.kind(), SocialKind::Follow | SocialKind::Share)
    }
}

