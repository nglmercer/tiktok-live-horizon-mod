//! Browser-free discovery for the TikTok LIVE endpoints that need no signature.
//!
//! Phase 4 of `docs/11-webview-removal.md`. Discovery is one of the three dependencies the WebView
//! carried, and it turns out to need no signing at all:
//!
//! | Operation | Requirement | Implemented |
//! |---|---|---|
//! | `unique_id` → `room_id` | none | [`DiscoveryClient::room_lookup`] |
//! | `/webcast/room/info/` | none | [`DiscoveryClient::room_info`] |
//! | `/webcast/gift/list/` | none | [`DiscoveryClient::gift_list`] |
//! | who is live now | none | [`DiscoveryClient::live_channels`] |
//!
//! The last three were signed until 2026-08-18, when a one-character tamper test showed that none
//! of them verifies a signature: a correct signature, one with a single character changed, and no
//! signature at all return the same data. Run `scripts/headless/verify-probe.mjs` to reproduce it.
//! Dropping the signature removed a signer subprocess from every read, and removed a false success
//! signal — "room/info accepts our signature" was the evidence the transport diagnosis rested on,
//! and it never meant anything.
//!
//! The one request that *is* verified — the direct message socket — is signed by
//! [`UrlSigner`], which `ttl-sign-embedded` implements by running the real bundle in an embedded
//! engine. This crate never signs and never spawns a process to do it.
//!
//! "Who is live now" was previously believed to need a rendering engine, because the `/live` page
//! ships no channel data in its HTML. It does not: `/api/search/live/full/` returns the same
//! information as JSON. See also `scripts/headless/find-live.mjs`.
//!
//! Parsing and URL construction live in [`ttl_sign_core::room`] and are shared with the WebView
//! path, so a native lookup and a page lookup cannot disagree about what "live" means.
//!
//! This crate never signs and never opens a browser. Its dependency tree contains no `wry`.

use std::sync::Arc;
use std::time::Duration;

mod http;
pub use http::{HttpError, HttpFuture, HttpGet, HttpRequest, HttpResponse};

use ttl_sign_core::room::{self, RoomLookup};
use ttl_sign_core::Preset;

/// Default ceiling on one discovery request.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest response body accepted from a discovery endpoint.
///
/// The lookup response is a few kilobytes. A much larger body means the endpoint returned
/// something other than what this crate parses, and reading it to completion would be the only
/// harm done.
const MAX_BODY_BYTES: usize = 1024 * 1024;

fn bounded_text(response: HttpResponse, limit: usize) -> Result<String, DiscoveryError> {
    if response.body.len() > limit {
        return Err(DiscoveryError::TooLarge(limit));
    }
    Ok(String::from_utf8_lossy(&response.body).into_owned())
}

/// A discovery operation and what it needs beyond plain HTTP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DiscoveryOperation {
    /// `unique_id` → `room_id` and live status.
    RoomLookup,
    /// Full room metadata: title, owner, counters.
    RoomInfo,
    /// The room's gift table, needed to price gift events.
    GiftList,
    /// Which creators are live right now.
    LiveChannels,
}

impl DiscoveryOperation {
    pub const ALL: [DiscoveryOperation; 4] = [
        DiscoveryOperation::RoomLookup,
        DiscoveryOperation::RoomInfo,
        DiscoveryOperation::GiftList,
        DiscoveryOperation::LiveChannels,
    ];
}

/// What an operation needs on top of an ordinary HTTP client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Requirement {
    /// Plain HTTP is enough. This crate implements it.
    None,
    /// The request must carry a webmssdk signature, so it depends on the signing workstream.
    Signature,
    /// The data is produced by client-side rendering and is absent from any JSON endpoint. No
    /// amount of signing progress makes this native.
    ///
    /// Nothing currently carries this requirement: the one operation that appeared to — listing
    /// who is live — turned out to have a signable JSON endpoint. The variant is kept because the
    /// distinction is real and the next capability audited may need it.
    Renderer,
}

/// What an operation needs. This is the WebView-removal boundary, in one place and testable.
pub fn requirement(operation: DiscoveryOperation) -> Requirement {
    match operation {
        DiscoveryOperation::RoomLookup => Requirement::None,
        // These endpoints were independently verified to work without signing
        // (2026-08-18). Keep the capability table aligned with actual HTTP calls.
        DiscoveryOperation::RoomInfo
        | DiscoveryOperation::GiftList
        | DiscoveryOperation::LiveChannels => Requirement::None,
    }
}

/// Operations that need no signer at all.
///
/// All currently supported discovery operations are unsigned; the Requirement
/// enum remains extensible if a future endpoint proves to require a signature.
pub fn native_operations() -> Vec<DiscoveryOperation> {
    DiscoveryOperation::ALL
        .into_iter()
        .filter(|operation| requirement(*operation) == Requirement::None)
        .collect()
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiscoveryError {
    #[error("discovery transport failed: {0}")]
    Transport(String),
    #[error("discovery endpoint answered {status}")]
    Status { status: u16 },
    #[error("discovery response could not be parsed: {0}")]
    Decode(String),
    /// Valid JSON with no `data.user`: the handle does not resolve. Kept distinct from
    /// [`DiscoveryError::Decode`] so a TikTok schema change (unparseable body) can never
    /// be reported as "user does not exist".
    #[error("lookup answered without a user: {0}")]
    UserNotFound(String),
    /// The endpoint answered, but the user has no live room. This is a normal outcome, not a
    /// failure of the client, and is distinguished so a caller does not retry it.
    #[error("@{0} has no live room")]
    NoRoom(String),
    #[error("response exceeded the {0} byte discovery limit")]
    TooLarge(usize),
    /// The signer could not produce a signed URL.
    #[error("signing failed: {0}")]
    Signer(String),
    /// TikTok refused the request with a webcast status code.
    #[error("webcast refused the request: {0}")]
    Refused(String),
    /// Accepted but answered with an empty body — the identity was not sufficient.
    #[error("endpoint accepted the request but returned nothing")]
    EmptyResponse,
}

/// Native discovery client.
///
/// Holds the HTTP transport (an `HttpGet`) shared by every lookup.
#[derive(Debug, Clone)]
pub struct DiscoveryClient {
    http: Arc<dyn HttpGet>,
    user_agent: String,
    timeout: Duration,
    /// Cookie header sent with signed requests.
    ///
    /// Signing and requesting must present the same identity: the signer sees these cookies, so
    /// the request has to carry them too, or the service is asked to validate a signature made
    /// under a different identity.
    session: String,
}

impl DiscoveryClient {
    /// Build a client that presents the same User-Agent as the signing identity.
    ///
    /// The lookup endpoint does not require a matching User-Agent, but presenting two different
    /// ones from the same logical client is a needless inconsistency.
    pub fn new(preset: &Preset, http: Arc<dyn HttpGet>) -> Result<Self, DiscoveryError> {
        Self::with_timeout(preset, http, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(
        preset: &Preset,
        http: Arc<dyn HttpGet>,
        timeout: Duration,
    ) -> Result<Self, DiscoveryError> {
        Ok(Self {
            http,
            user_agent: preset.user_agent().to_owned(),
            timeout,
            session: String::new(),
        })
    }

    async fn fetch(
        &self,
        url: &str,
        headers: Vec<(String, String)>,
        max_body: usize,
    ) -> Result<HttpResponse, DiscoveryError> {
        self.http
            .get(HttpRequest {
                url: url.to_owned(),
                headers,
                user_agent: self.user_agent.clone(),
                timeout: self.timeout,
                max_body,
                truncate: false,
            })
            .await
            .map_err(|error| match error {
                HttpError::TooLarge => DiscoveryError::TooLarge(max_body),
                HttpError::Transport(message) => DiscoveryError::Transport(message),
            })
    }

    /// Attach the session presented with signed requests.
    ///
    /// The same jar must be given to the signer, so that what is signed and what is sent agree.
    pub fn with_session(mut self, cookie_header: impl Into<String>) -> Self {
        self.session = cookie_header.into();
        self
    }

    /// Resolve `unique_id` → `room_id`. Unsigned, no cookies, no browser.
    ///
    /// A leading `@` is tolerated; [`ttl_sign_core::room::room_lookup_url`] normalizes it.
    pub async fn room_lookup(&self, unique_id: &str) -> Result<RoomLookup, DiscoveryError> {
        let url = room::room_lookup_url(unique_id);
        tracing::debug!(unique_id, "native room lookup");

        let response = self.fetch(&url, Vec::new(), MAX_BODY_BYTES).await?;
        let status = response.status;
        let body = bounded_text(response, MAX_BODY_BYTES)?;

        interpret_room_lookup(unique_id, status, &body)
    }
}

/// Turn a lookup response into an outcome.
///
/// Separated from the request so every branch is testable without a network: the interesting
/// behaviour is the classification, not the socket.
pub fn interpret_room_lookup(
    unique_id: &str,
    status: u16,
    body: &str,
) -> Result<RoomLookup, DiscoveryError> {
    if !(200..300).contains(&status) {
        return Err(DiscoveryError::Status { status });
    }
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        DiscoveryError::Decode(format!("unexpected lookup response for @{unique_id}"))
    })?;
    // Valid JSON without a user means the handle does not resolve. This check runs
    // before field parsing so only genuinely absent users — never a schema change,
    // which fails the parse above — take the not-found path.
    let has_user = value
        .get("data")
        .and_then(|data| data.get("user"))
        .is_some_and(|user| !user.is_null());
    if !has_user {
        return Err(DiscoveryError::UserNotFound(
            unique_id.trim_start_matches('@').to_string(),
        ));
    }
    let lookup = room::RoomLookup::from_value(&value).ok_or_else(|| {
        DiscoveryError::Decode(format!("unexpected lookup response for @{unique_id}"))
    })?;
    // The endpoint reports an absent room as an empty string or a literal `0`, never by omitting
    // the field. Report that as its own outcome so a caller does not treat a normal offline
    // creator as a transport problem — and does not go on to sign a request for room `0`.
    //
    // A real room id with a non-live status is *not* an error: the caller reads `is_live()`.
    if !room::is_usable_room_id(&lookup.room_id) {
        return Err(DiscoveryError::NoRoom(
            unique_id.trim_start_matches('@').to_string(),
        ));
    }
    Ok(lookup)
}


// --- Signed discovery ---------------------------------------------------------------------
//
// `room/info` and `gift/list` need a webmssdk signature. This crate does not sign: it takes a
// [`UrlSigner`], so the same code works against the WebView, the headless signer, or a future
// native one, and the choice stays with the caller.

use std::future::Future;
use std::pin::Pin;

use ttl_sign_core::room::{Gift, RoomInfo};

pub type SignFuture<'a> = Pin<Box<dyn Future<Output = Result<String, DiscoveryError>> + Send + 'a>>;

/// Something that can turn an unsigned webcast URL into a signed one.
pub trait UrlSigner: Send + Sync {
    fn sign<'a>(&'a self, url: &'a str) -> SignFuture<'a>;
}

/// Which signing product to request.
///
/// These are not interchangeable. The patched-fetch suffix is what `room/info` and `gift/list`
/// accept; `im/fetch` rejects it with 403 and wants the public `frontierSign` product instead.
/// Sending the wrong one looks exactly like a broken signer, so it is chosen explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningProduct {
    /// Patched-fetch suffix: `X-Dynosaur`, `msToken`, `X-Bogus=1`, `X-Gnarly`.
    FetchPatch,
    /// Public `frontierSign`: a real 16-byte `X-Bogus`.
    FrontierSign,
    /// `registerWsSigner` over the query bytes, appended as `X-Gnarly`. This is what the direct
    /// message socket verifies; it signs the query exactly as sent, so the URL must not be
    /// re-encoded on the way in or out.
    WsDirect,
}

/// URL of the live search endpoint, which lists rooms that are broadcasting now.
///
/// The `/live` page renders its list client-side, so its HTML carries nothing. This endpoint
/// returns the same rooms as JSON and is merely signed, which is why listing live channels needs
/// no rendering engine.
pub fn live_search_url(keyword: &str, offset: usize) -> String {
    live_search_url_with_context(keyword, offset, "")
}

/// Continue the same search using the first page's impression ID.
pub fn live_search_url_with_context(keyword: &str, offset: usize, search_id: &str) -> String {
    // Built from pairs rather than one long literal: a line-continued string silently keeps the
    // indentation as spaces inside the query, which TikTok answers with a body that has no data.
    let mut url = String::from("https://www.tiktok.com/api/search/live/full/?");
    let offset = offset.to_string();
    let pairs: [(&str, &str); 24] = [
        ("aid", "1988"),
        ("app_language", "en"),
        ("app_name", "tiktok_web"),
        ("browser_language", "en-US"),
        ("browser_name", "Mozilla"),
        ("browser_platform", "Linux x86_64"),
        ("browser_version", "5.0 (X11)"),
        ("cookie_enabled", "true"),
        ("count", "20"),
        ("device_platform", "web_pc"),
        ("focus_state", "true"),
        ("from_page", "search"),
        ("history_len", "4"),
        ("is_fullscreen", "false"),
        ("is_page_visible", "true"),
        ("keyword", keyword),
        ("offset", &offset),
        ("os", "linux"),
        ("priority_region", ""),
        ("referer", ""),
        ("region", "US"),
        ("screen_height", "1080"),
        ("screen_width", "1920"),
        ("search_id", search_id),
    ];
    for (index, (name, value)) in pairs.iter().enumerate() {
        if index > 0 {
            url.push('&');
        }
        url.push_str(name);
        url.push('=');
        url.push_str(&percent_encode(value));
    }
    url.push_str("&tz_name=America%2FNew_York&webcast_language=en");
    url
}

/// Percent-encode a query value, leaving only the unreserved set intact.
fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// One room returned by the live search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveRoom {
    pub unique_id: String,
    pub room_id: String,
    pub nickname: String,
    pub title: String,
    pub viewers: u64,
}

/// Extract broadcasting rooms from a live-search response.
///
/// Each item nests the room as a JSON *string* under `live_info.raw_data`. Only `status == 2`
/// counts as live: an unrecognized status is skipped rather than assumed to be broadcasting.
pub fn interpret_live_search(body: &str) -> Result<Vec<LiveRoom>, DiscoveryError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| DiscoveryError::Decode(error.to_string()))?;
    interpret_live_search_value(&value)
}

/// Server continuation for a search. The search ID remains from its first page;
/// per-request impression IDs from later pages must not replace that context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSearchCursor {
    pub offset: usize,
    pub search_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSearchPage {
    pub rooms: Vec<LiveRoom>,
    pub next: Option<LiveSearchCursor>,
}

pub fn interpret_live_search_page(
    body: &str,
    previous: Option<&LiveSearchCursor>,
) -> Result<LiveSearchPage, DiscoveryError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| DiscoveryError::Decode(error.to_string()))?;
    let more = match value.get("has_more") {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(value) if value.as_u64() == Some(1) => true,
        Some(value) if value.as_u64() == Some(0) => false,
        None => false, // legacy non-paginated response; no continuation is asserted
        _ => {
            return Err(DiscoveryError::Decode(
                "invalid live search has_more".into(),
            ))
        }
    };
    // A terminal continuation may omit data entirely. Do not apply this exception
    // to unstructured first-page bodies or to refusals that merely omit data.
    let rooms = if previous.is_some()
        && value.get("data").is_none()
        && value.get("status_code").and_then(|v| v.as_i64()) == Some(0)
        && value.get("has_more").is_some()
        && !more
    {
        Vec::new()
    } else {
        interpret_live_search_value(&value)?
    };
    let next = if more {
        let offset = value
            .get("cursor")
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| DiscoveryError::Decode("missing live search cursor".into()))?;
        if offset <= previous.map_or(0, |p| p.offset) {
            return Err(DiscoveryError::Decode(
                "live search cursor did not advance".into(),
            ));
        }
        let search_id = previous
            .map(|p| p.search_id.as_str())
            .filter(|id| !id.is_empty())
            .or_else(|| value.get("log_pb")?.get("impr_id")?.as_str())
            .filter(|id| !id.is_empty() && id.len() <= 256)
            .ok_or_else(|| DiscoveryError::Decode("missing live search context".into()))?
            .to_owned();
        Some(LiveSearchCursor { offset, search_id })
    } else {
        None
    };
    Ok(LiveSearchPage { rooms, next })
}

fn interpret_live_search_value(value: &serde_json::Value) -> Result<Vec<LiveRoom>, DiscoveryError> {
    // TikTok sometimes answers HTTP 200 with a login/rate-limit refusal in
    // status_code. Do not turn that into an empty list of available rooms.
    if let Some(status_code) = value.get("status_code").and_then(serde_json::Value::as_i64) {
        if status_code != 0 {
            let message = value
                .get("status_msg")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("request refused");
            return Err(DiscoveryError::Refused(format!(
                "live search status_code={status_code}: {message}"
            )));
        }
    }
    let items = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| DiscoveryError::Decode("live search has no data array".into()))?;

    let mut rooms = Vec::new();
    for item in items {
        let Some(raw) = item
            .get("live_info")
            .and_then(|info| info.get("raw_data"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Ok(room) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        if room.get("status").and_then(serde_json::Value::as_i64) != Some(2) {
            continue;
        }
        let owner = room.get("owner");
        let unique_id = owner
            .and_then(|owner| owner.get("display_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let room_id = room
            .get("id_str")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if unique_id.is_empty() || !room::is_usable_room_id(&room_id) {
            continue;
        }
        rooms.push(LiveRoom {
            unique_id,
            room_id,
            nickname: owner
                .and_then(|owner| owner.get("nickname"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: room
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            viewers: room
                .get("user_count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default(),
        });
    }
    Ok(rooms)
}

impl DiscoveryClient {
    /// Rooms broadcasting right now, most watched first. Unsigned.
    ///
    /// Results depend on the keyword, so this samples live rooms rather than enumerating them.
    pub async fn live_channels(&self, keyword: &str) -> Result<Vec<LiveRoom>, DiscoveryError> {
        self.live_channels_page(keyword, 0).await
    }

    /// Offset-only legacy search. For reliable pagination use `live_channels_context`,
    /// which propagates the server's search ID and cursor across pages.
    ///
    /// The caller controls pagination/rate limits and selects a creator explicitly.
    /// An empty page is not proof that no one is broadcasting; the upstream search
    /// may be incomplete. The returned rooms are ordered by viewer count.
    /// Fetch a search page while retaining the server cursor and original search ID.
    pub async fn live_channels_context(
        &self,
        keyword: &str,
        cursor: Option<&LiveSearchCursor>,
    ) -> Result<LiveSearchPage, DiscoveryError> {
        let url = live_search_url_with_context(
            keyword,
            cursor.map_or(0, |c| c.offset),
            cursor.map_or("", |c| c.search_id.as_str()),
        );
        let body = self.read(&url).await?;
        let mut page = interpret_live_search_page(&body, cursor)?;
        page.rooms
            .sort_by_key(|room| std::cmp::Reverse(room.viewers));
        Ok(page)
    }

    pub async fn live_channels_page(
        &self,
        keyword: &str,
        offset: usize,
    ) -> Result<Vec<LiveRoom>, DiscoveryError> {
        let body = self.read(&live_search_url(keyword, offset)).await?;
        let mut rooms = interpret_live_search(&body)?;
        rooms.sort_by_key(|room| std::cmp::Reverse(room.viewers));
        Ok(rooms)
    }

    /// Full room metadata. Unsigned.
    pub async fn room_info(&self, room_id: &str) -> Result<RoomInfo, DiscoveryError> {
        let body = self.read(&room::room_info_url(room_id)).await?;
        interpret_room_info(room_id, &body)
    }

    /// Every gift the room offers, with its diamond cost. Unsigned.
    ///
    /// The response is several megabytes; the client's timeout applies to it.
    pub async fn gift_list(&self, room_id: &str) -> Result<Vec<Gift>, DiscoveryError> {
        let body = self.read(&room::gift_list_url(room_id)).await?;
        interpret_gift_list(room_id, &body)
    }

    /// Read a webcast endpoint that does not verify a signature.
    ///
    /// These three were signed until it was measured that they do not check: `room/info`,
    /// `gift/list` and the live search each return identical data whether the request carries a
    /// correct signature, one with a single character changed, or none at all
    /// (`scripts/headless/verify-probe.mjs`, 2026-08-18). Signing them cost a subprocess per call
    /// and, worse, produced a success that was mistaken for evidence that the signer works —
    /// which is how the transport diagnosis went wrong. Only `/webcast/im/fetch/` evaluates a
    /// signature, and that path lives in `ttl-sign-headless`.
    async fn read(&self, url: &str) -> Result<String, DiscoveryError> {
        let mut headers = vec![
            ("referer".to_owned(), "https://www.tiktok.com/".to_owned()),
            ("origin".to_owned(), "https://www.tiktok.com".to_owned()),
        ];
        if !self.session.is_empty() {
            headers.push(("cookie".to_owned(), self.session.clone()));
        }
        let response = self.fetch(url, headers, 16 * 1024 * 1024).await?;
        let status = response.status;
        if !(200..300).contains(&status) {
            return Err(DiscoveryError::Status { status });
        }
        let body = bounded_text(response, 16 * 1024 * 1024)?;
        if body.is_empty() {
            // A signed request that is accepted but answered with nothing means the identity was
            // insufficient, not that the room is empty. Reported distinctly so a caller does not
            // read it as "no gifts".
            return Err(DiscoveryError::EmptyResponse);
        }
        Ok(body)
    }
}

/// Classify a `room/info` response. Separated from the request so every branch is testable.
pub fn interpret_room_info(room_id: &str, body: &str) -> Result<RoomInfo, DiscoveryError> {
    if let Some(refusal) = room::webcast_refusal(body) {
        return Err(DiscoveryError::Refused(refusal.to_string()));
    }
    RoomInfo::from_json(body).ok_or_else(|| {
        DiscoveryError::Decode(format!("unexpected room/info response for room {room_id}"))
    })
}

/// Classify a `gift/list` response.
pub fn interpret_gift_list(room_id: &str, body: &str) -> Result<Vec<Gift>, DiscoveryError> {
    if let Some(refusal) = room::webcast_refusal(body) {
        return Err(DiscoveryError::Refused(refusal.to_string()));
    }
    room::parse_gift_list(body).ok_or_else(|| {
        DiscoveryError::Decode(format!("unexpected gift/list response for room {room_id}"))
    })
}

pub mod identity;
