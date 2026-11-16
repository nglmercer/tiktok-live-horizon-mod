//! The connection to TikTok LIVE: one thread per account with its own single-threaded tokio runtime.
//! It never touches the `host`: it only writes to a bounded channel that the hub drains in `server_tick`.
use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::watch;
use ttl_live_core::{run_buffered, Error as LiveError, LiveSink, SignFuture, SocketSigner, State};
use ttl_live_discovery::identity::bootstrap_guest_identity;
use ttl_live_discovery::{DiscoveryClient, DiscoveryError, HttpGet, HttpRequest, SigningProduct, UrlSigner};
use ttl_live_events::{DecodedEvent, LiveEvent};
use ttl_sign_core::{CookieJar, DevicePreset, LocationPreset, Preset, ScreenPreset};
use ttl_sign_embedded::{EmbeddedSigner, Profile};

use crate::event::{self, Event};
use crate::http::NativeHttp;
use crate::value::Val;

/// Capacity of the channel to the hub: what overflows is dropped and counted.
pub const CHANNEL_CAPACITY: usize = 4096;
/// How often a creator that is not live is checked again.
const OFFLINE_POLL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const BUNDLE_MAX_BYTES: usize = 16 * 1024 * 1024;

/// What the network thread sends to the hub.
pub enum Inbound {
    Event(Box<Event>),
    /// The freshly downloaded and verified bundle, to store it in `horizon-mod-data`.
    Bundle(String),
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub state: String,
    pub detail: String,
    pub room_id: String,
    pub events: u64,
    pub dropped: u64,
    pub reconnects: u32,
    pub last_error: String,
}

pub enum BundleSource {
    /// Already read and verified.
    Ready(String),
    /// It must be downloaded and its SHA-256 verified.
    Download { url: String, sha256: String },
}

pub struct Settings {
    pub unique_id: String,
    pub target_player: String,
    pub max_retries: u32,
    pub reconnect: bool,
    pub fetch_gifts: bool,
    pub session_cookie: String,
    pub bundle: BundleSource,
}

pub struct LiveHandle {
    pub unique_id: String,
    #[allow(dead_code)] // reserved: part of the configuration's API/semantics
    pub target_player: String,
    pub status: Arc<Mutex<Status>>,
    pub rx: Receiver<Inbound>,
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

impl LiveHandle {
    pub fn spawn(settings: Settings) -> Result<LiveHandle, String> {
        let (tx, rx) = sync_channel(CHANNEL_CAPACITY);
        let (stop, stop_rx) = watch::channel(false);
        let status = Arc::new(Mutex::new(Status { state: "connecting".into(), ..Status::default() }));
        let handle_status = status.clone();
        let unique_id = settings.unique_id.clone();
        let target_player = settings.target_player.clone();
        let thread = std::thread::Builder::new()
            .name(format!("tiktok-live-{unique_id}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        set(&status, "error", &format!("no async runtime: {error}"));
                        return;
                    }
                };
                runtime.block_on(run(settings, tx, status, stop_rx));
            })
            .map_err(|e| e.to_string())?;
        Ok(LiveHandle { unique_id, target_player, status: handle_status, rx, stop, thread: Some(thread) })
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Asks to stop. It does not wait: the thread ends on its own (see `join_timeout`).
    pub fn request_stop(&self) {
        self.stop.send_replace(true);
    }

    /// Waits up to `timeout` for the thread to end; if it does not, it is left running (it does not block the stop).
    pub fn join_timeout(&mut self, timeout: Duration) -> bool {
        self.request_stop();
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.is_finished() {
                if let Some(thread) = self.thread.take() {
                    let _ = thread.join();
                }
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }
}

fn set(status: &Mutex<Status>, state: &str, detail: &str) {
    let mut guard = status.lock().unwrap_or_else(|p| p.into_inner());
    guard.state = state.to_owned();
    guard.detail = detail.to_owned();
}

fn fail(status: &Mutex<Status>, message: &str) {
    let mut guard = status.lock().unwrap_or_else(|p| p.into_inner());
    guard.state = "error".to_owned();
    guard.last_error = message.to_owned();
    guard.detail = message.to_owned();
}

async fn pause(duration: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        _ = ttl_live_core::cancelled(stop) => true,
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

async fn load_bundle(
    source: &BundleSource,
    http: &dyn HttpGet,
    tx: &SyncSender<Inbound>,
) -> Result<String, String> {
    match source {
        BundleSource::Ready(text) => Ok(text.clone()),
        BundleSource::Download { url, sha256 } => {
            let response = http
                .get(HttpRequest {
                    url: url.clone(),
                    headers: Vec::new(),
                    user_agent: "Mozilla/5.0".to_owned(),
                    timeout: Duration::from_secs(60),
                    max_body: BUNDLE_MAX_BYTES,
                    truncate: false,
                })
                .await
                .map_err(|e| format!("could not download the signing bundle: {e:?}"))?;
            if response.status != 200 {
                return Err(format!("the signing bundle answered HTTP {}", response.status));
            }
            let actual = sha256_hex(&response.body);
            if &actual != sha256 {
                return Err(format!(
                    "the signing bundle changed (sha256 {actual}, expected {sha256}); update the mod or signing.bundle-sha256 after reviewing it"
                ));
            }
            let text = String::from_utf8(response.body).map_err(|_| "the signing bundle is not UTF-8".to_owned())?;
            let _ = tx.try_send(Inbound::Bundle(text.clone()));
            Ok(text)
        }
    }
}

struct Adapter(EmbeddedSigner);

impl SocketSigner for Adapter {
    fn sign<'a>(&'a self, url: &'a str) -> SignFuture<'a> {
        Box::pin(async move { UrlSigner::sign(&self.0, url).await.map_err(|_| ()) })
    }
}

struct Sink {
    tx: SyncSender<Inbound>,
    status: Arc<Mutex<Status>>,
    room_id: String,
    unique_id: String,
    target_player: String,
    /// The room's gift catalog (empty if `fetch-gifts` is off or it failed).
    catalog: Arc<HashMap<u64, ttl_sign_core::room::Gift>>,
}

impl LiveSink for Sink {
    fn on_state(&self, state: State, detail: &str) {
        let name = match state {
            State::Disconnected => "disconnected",
            State::Connecting => "connecting",
            State::Connected => "connected",
            State::Reconnecting => "reconnecting",
            State::Offline => "offline",
            State::Error => "error",
        };
        let mut guard = self.status.lock().unwrap_or_else(|p| p.into_inner());
        if state == State::Reconnecting {
            guard.reconnects += 1;
        }
        guard.state = name.to_owned();
        guard.detail = detail.to_owned();
    }

    fn on_error(&self, error: LiveError, retryable: bool) {
        let mut guard = self.status.lock().unwrap_or_else(|p| p.into_inner());
        guard.last_error = format!("{error:?} (retryable: {retryable})");
    }

    fn on_batch(&self, batch: ttl_live_events::EventBatch) {
        let now = crate::hub::now_ms();
        for decoded in &batch.events {
            let event = normalize(decoded, &self.room_id, &self.unique_id, &self.target_player, now, &self.catalog);
            match self.tx.try_send(Inbound::Event(Box::new(event))) {
                Ok(()) => self.status.lock().unwrap_or_else(|p| p.into_inner()).events += 1,
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                    self.status.lock().unwrap_or_else(|p| p.into_inner()).dropped += 1;
                }
            }
        }
    }
}

async fn run(settings: Settings, tx: SyncSender<Inbound>, status: Arc<Mutex<Status>>, mut stop: watch::Receiver<bool>) {
    let http = match NativeHttp::new() {
        Ok(http) => Arc::new(http),
        Err(error) => return fail(&status, &format!("tls setup failed: {error}")),
    };
    let preset = Preset::new(DevicePreset::chrome_linux(), LocationPreset::us_east(), ScreenPreset::FHD);
    let discovery = match DiscoveryClient::with_timeout(&preset, http.clone(), REQUEST_TIMEOUT) {
        Ok(discovery) => discovery,
        Err(error) => return fail(&status, &format!("discovery setup failed: {error}")),
    };
    let mut bundle: Option<String> = None;
    loop {
        if *stop.borrow() {
            break;
        }
        set(&status, "connecting", "looking up the room");
        let room = match discovery.room_lookup(&settings.unique_id).await {
            Ok(lookup) if lookup.is_live() => lookup.room_id,
            Ok(_) | Err(DiscoveryError::NoRoom(_)) => {
                set(&status, "offline", "the creator is not live");
                if !settings.reconnect || pause(OFFLINE_POLL, &mut stop).await {
                    break;
                }
                continue;
            }
            Err(DiscoveryError::UserNotFound(name)) => {
                fail(&status, &format!("TikTok user @{name} does not exist"));
                break;
            }
            Err(error) => {
                fail(&status, &format!("room lookup failed: {error}"));
                if !settings.reconnect || pause(OFFLINE_POLL, &mut stop).await {
                    break;
                }
                continue;
            }
        };
        status.lock().unwrap_or_else(|p| p.into_inner()).room_id = room.clone();

        if bundle.is_none() {
            set(&status, "connecting", "loading the signing bundle");
            match load_bundle(&settings.bundle, &*http, &tx).await {
                Ok(text) => bundle = Some(text),
                Err(error) => {
                    // A bundle other than the pinned one is a failure that retrying does not fix.
                    fail(&status, &error);
                    break;
                }
            }
        }
        let cookies = if settings.session_cookie.is_empty() {
            match bootstrap_guest_identity(&*http, &preset.user_agent(), REQUEST_TIMEOUT).await {
                Ok(identity) => identity.cookies,
                Err(error) => {
                    fail(&status, &format!("guest identity failed: {error}"));
                    if !settings.reconnect || pause(OFFLINE_POLL, &mut stop).await {
                        break;
                    }
                    continue;
                }
            }
        } else {
            CookieJar::parse(&settings.session_cookie)
        };
        let profile = Profile {
            user_agent: Some(preset.user_agent()),
            cookie: Some(cookies.to_cookie_string()),
            ..Profile::default()
        };
        let catalog: Arc<HashMap<u64, ttl_sign_core::room::Gift>> = if settings.fetch_gifts {
            set(&status, "connecting", "loading the gift catalog");
            match discovery.gift_list(&room).await {
                Ok(gifts) => Arc::new(gifts.into_iter().map(|g| (g.id, g)).collect()),
                Err(error) => {
                    // The catalog only enriches events: without it the connection still works.
                    status.lock().unwrap_or_else(|p| p.into_inner()).last_error = format!("gift catalog unavailable: {error}");
                    Arc::new(HashMap::new())
                }
            }
        } else {
            Arc::new(HashMap::new())
        };
        let signer = match EmbeddedSigner::with_product(bundle.clone().unwrap_or_default(), profile, SigningProduct::WsDirect) {
            Ok(signer) => Adapter(signer),
            Err(error) => {
                fail(&status, &format!("the signer could not start: {error}"));
                break;
            }
        };
        let sink = Sink {
            tx: tx.clone(),
            status: status.clone(),
            room_id: room.clone(),
            unique_id: settings.unique_id.clone(),
            target_player: settings.target_player.clone(),
            catalog,
        };
        let result = run_buffered(&signer, &cookies, &preset, &room, settings.max_retries, sink, stop.clone()).await;
        if let Err(error) = result {
            fail(&status, &format!("the live worker stopped: {error}"));
        }
        if *stop.borrow() || !settings.reconnect {
            break;
        }
        set(&status, "offline", "the stream ended; waiting to reconnect");
        if pause(OFFLINE_POLL, &mut stop).await {
            break;
        }
    }
    let mut guard = status.lock().unwrap_or_else(|p| p.into_inner());
    if guard.state != "error" {
        guard.state = "disconnected".to_owned();
    }
}

/// Converts a signer event to the plugin's normalized event (`LiveEventNormalizer`).
pub fn normalize(
    decoded: &DecodedEvent,
    room_id: &str,
    unique_id: &str,
    target_player: &str,
    now_ms: i64,
    catalog: &HashMap<u64, ttl_sign_core::room::Gift>,
) -> Event {
    let (kind, user, mut payload): (&str, Option<&ttl_live_events::EventUser>, Vec<(&str, Val)>) = match &decoded.event {
        LiveEvent::Chat(chat) => (
            event::CHAT,
            Some(&chat.user),
            vec![
                ("chat.text", Val::Str(chat.comment.clone())),
                ("chat.emotes", emotes(&chat.emotes)),
                ("chat.emoteOnly", Val::Bool(false)),
            ],
        ),
        LiveEvent::EmoteChat(chat) => (
            event::CHAT_EMOTE,
            Some(&chat.user),
            vec![
                ("chat.text", Val::Str(String::new())),
                ("chat.emotes", emotes(&chat.emotes)),
                ("chat.emoteOnly", Val::Bool(true)),
            ],
        ),
        LiveEvent::Gift(gift) => {
            let listed = catalog.get(&gift.gift_id);
            let name = if gift.gift_name.is_empty() { listed.map(|g| g.name.clone()).unwrap_or_default() } else { gift.gift_name.clone() };
            let diamonds = if gift.diamond_count == 0 { listed.map_or(0, |g| g.diamond_count) } else { gift.diamond_count };
            (
            if gift.repeat_end { event::GIFT_FINAL } else { event::GIFT },
            Some(&gift.user),
            vec![
                ("gift.id", Val::Str(gift.gift_id.to_string())),
                ("gift.name", Val::Str(name)),
                ("gift.streakable", Val::Bool(listed.is_some_and(|g| g.combo))),
                ("gift.diamonds", Val::Int(diamonds as i64)),
                ("gift.repeatCount", Val::Int(gift.repeat_count as i64)),
                ("gift.comboCount", Val::Int(gift.combo_count as i64)),
                ("gift.groupId", Val::Str(gift.group_id.to_string())),
                ("gift.repeatEnd", Val::Bool(gift.repeat_end)),
                ("gift.imageUrl", Val::Str(gift.gift_image_url.clone().unwrap_or_default())),
            ],
            )
        }
        LiveEvent::Like(like) => (
            event::LIKE,
            Some(&like.user),
            vec![("like.count", Val::Int(like.count as i64)), ("like.total", Val::Int(like.total as i64))],
        ),
        LiveEvent::Member(member) => (
            event::MEMBER,
            Some(&member.user),
            vec![("member.count", Val::Int(member.member_count as i64)), ("member.action", Val::Int(i64::from(member.action)))],
        ),
        LiveEvent::Social(social) => {
            use ttl_live_events::social::SocialKind;
            let (trigger, kind_name) = match social.classify().kind {
                SocialKind::Follow => (event::SOCIAL_FOLLOW, "follow"),
                SocialKind::Share => (event::SOCIAL_SHARE, "share"),
                SocialKind::Other => (event::SOCIAL_OTHER, "other"),
                SocialKind::Unknown => (event::SOCIAL_UNKNOWN, "unknown"),
            };
            (
                trigger,
                Some(&social.user),
                vec![
                    ("social.kind", Val::Str(kind_name.to_owned())),
                    ("social.action", Val::Int(social.action)),
                    ("social.followCount", Val::Int(social.follow_count as i64)),
                    ("social.shareCount", Val::Int(social.share_count as i64)),
                    ("social.shareType", Val::Int(social.share_type)),
                    ("social.followType", Val::Int(i64::from(social.follow_type))),
                    ("social.scene", Val::Str(social.scene.clone())),
                    ("social.shareTarget", Val::Str(social.share_target.clone())),
                    ("social.targetUserId", Val::Str(social.target_user_id.to_string())),
                ],
            )
        }
        LiveEvent::RoomUser(room) => (
            event::ROOM_USER,
            None,
            vec![
                ("room.viewers", Val::Int(room.total as i64)),
                ("room.popularity", Val::Int(room.popularity as i64)),
                ("room.totalUser", Val::Int(room.total_user as i64)),
                ("room.anonymous", Val::Int(room.anonymous as i64)),
                (
                    "room.topViewers",
                    Val::List(room.top_viewers.iter().map(|v| Val::Str(v.user.nickname.clone())).collect()),
                ),
            ],
        ),
        LiveEvent::Unknown { method, .. } => (event::UNKNOWN, None, vec![("unknown.method", Val::Str(method.clone()))]),
    };
    let mut event = Event::new(kind);
    event.id = if decoded.raw.msg_id != 0 { decoded.raw.msg_id.to_string() } else { format!("{kind}-{now_ms}") };
    event.room_id = room_id.to_owned();
    event.timestamp = now_ms;
    event.history = decoded.raw.is_history;
    if let Some(user) = user {
        event.user_id = user.id.to_string();
        event.unique_id = user.unique_id.clone();
        event.nickname = user.nickname.clone();
        payload.push(("user.secUid", Val::Str(user.sec_uid.clone())));
        payload.push(("user.avatarUrl", Val::Str(user.avatar_url.clone().unwrap_or_default())));
    }
    for (key, value) in payload {
        event.payload.insert(key.to_owned(), value);
    }
    event.payload.insert("connection.uniqueId".to_owned(), Val::Str(unique_id.to_owned()));
    event.payload.insert("connection.targetPlayer".to_owned(), Val::Str(target_player.to_owned()));
    event
}

fn emotes(list: &[ttl_live_events::LiveEmote]) -> Val {
    Val::List(
        list.iter()
            .map(|emote| {
                let mut map = std::collections::BTreeMap::new();
                map.insert("id".to_owned(), Val::Str(emote.id.clone()));
                map.insert("shortcode".to_owned(), Val::Str(emote.shortcode.clone().unwrap_or_default()));
                map.insert("imageUrl".to_owned(), Val::Str(emote.image_url.clone().unwrap_or_default()));
                map.insert("index".to_owned(), Val::Int(i64::from(emote.index.unwrap_or(0))));
                map.insert("emoteType".to_owned(), Val::Int(i64::from(emote.emote_type.unwrap_or(0))));
                map.insert("packageId".to_owned(), Val::Str(emote.package_id.clone().unwrap_or_default()));
                Val::Map(map)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ttl_live_events::{EventUser, GiftEvent, NormalizationStatus, RawEvent};

    fn decoded(event: LiveEvent, msg_id: u64) -> DecodedEvent {
        DecodedEvent {
            raw: RawEvent { method: "x".into(), msg_id, payload: Vec::new(), is_history: false },
            event,
            schema: Default::default(),
            normalization_status: NormalizationStatus::Normalized,
            timestamp: None,
        }
    }

    #[test]
    fn a_gift_is_normalized_like_the_plugin_does() {
        let user = EventUser { id: 7, nickname: "Ana".into(), unique_id: "ana_t".into(), ..EventUser::default() };
        let gift = GiftEvent {
            user,
            gift_id: 5655,
            gift_name: "Rose".into(),
            diamond_count: 1,
            repeat_count: 3,
            combo_count: 3,
            group_id: 99,
            repeat_end: true,
            gift_image_url: None,
        };
        let event = normalize(&decoded(LiveEvent::Gift(gift), 123), "room1", "creator", "Steve", 1000, &HashMap::new());
        assert_eq!(event.kind, "gift.final");
        assert_eq!(event.id, "123");
        assert_eq!(event.unique_id, "ana_t");
        assert_eq!(event.payload["gift.id"], Val::Str("5655".into()));
        assert_eq!(event.payload["gift.repeatCount"], Val::Int(3));
        assert_eq!(event.payload["connection.targetPlayer"], Val::Str("Steve".into()));
    }

    #[test]
    fn sha256_matches_the_known_digest() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    /// Real network test (costs one request): `cargo test -- --ignored live_room_lookup`.
    #[tokio::test]
    #[ignore]
    async fn live_room_lookup_reaches_tiktok() {
        let http = Arc::new(NativeHttp::new().unwrap());
        let preset = Preset::new(DevicePreset::chrome_linux(), LocationPreset::us_east(), ScreenPreset::FHD);
        let discovery = DiscoveryClient::with_timeout(&preset, http.clone(), REQUEST_TIMEOUT).unwrap();
        let result = discovery.room_lookup("tiktok").await;
        println!("room_lookup(@tiktok) = {result:?}");
        assert!(!matches!(result, Err(DiscoveryError::Transport(_))), "{result:?}");
        let identity = bootstrap_guest_identity(&*http, &preset.user_agent(), REQUEST_TIMEOUT).await;
        println!("guest identity ok = {}", identity.is_ok());
        assert!(identity.is_ok(), "{identity:?}");
    }

    /// End-to-end test against the real TikTok: `TTL_TEST_CREATOR=<username>` or, if missing, the
    /// first creator the live search returns. Ignored: it uses the network and waits ~40 s.
    #[tokio::test]
    #[ignore]
    async fn live_stream_end_to_end() {
        let http = Arc::new(NativeHttp::new().unwrap());
        let preset = Preset::new(DevicePreset::chrome_linux(), LocationPreset::us_east(), ScreenPreset::FHD);
        let identity = bootstrap_guest_identity(&*http, &preset.user_agent(), REQUEST_TIMEOUT).await.unwrap();
        let discovery = DiscoveryClient::with_timeout(&preset, http.clone(), REQUEST_TIMEOUT)
            .unwrap()
            .with_session(identity.cookies.to_cookie_string());
        let creator = match std::env::var("TTL_TEST_CREATOR") {
            Ok(name) if !name.is_empty() => name,
            _ => {
                let rooms = discovery.live_channels("live").await.expect("live search");
                println!("live rooms found: {}", rooms.len());
                let room = rooms.first().expect("no live rooms returned");
                println!("using @{} ({} viewers)", room.unique_id, room.viewers);
                room.unique_id.clone()
            }
        };
        let handle = LiveHandle::spawn(Settings {
            unique_id: creator.clone(),
            target_player: "Steve".into(),
            max_retries: 2,
            reconnect: false,
            fetch_gifts: false,
            session_cookie: String::new(),
            bundle: BundleSource::Download {
                url: crate::config::DEFAULT_BUNDLE_URL.to_owned(),
                sha256: crate::config::DEFAULT_BUNDLE_SHA256.to_owned(),
            },
        })
        .unwrap();
        let mut events = 0;
        let mut kinds = std::collections::BTreeSet::new();
        let mut bundle_cached = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        while std::time::Instant::now() < deadline && events < 20 {
            while let Ok(message) = handle.rx.try_recv() {
                match message {
                    Inbound::Event(event) => {
                        events += 1;
                        kinds.insert(event.kind.clone());
                        if events <= 3 {
                            println!("event {} user={} payload={:?}", event.kind, event.user_key(), event.payload.keys().collect::<Vec<_>>());
                        }
                    }
                    Inbound::Bundle(text) => bundle_cached = text.len() > 100_000,
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let status = handle.status();
        println!("status = {status:?}\nevents = {events}, kinds = {kinds:?}, bundle_cached = {bundle_cached}");
        let mut handle = handle;
        assert!(handle.join_timeout(Duration::from_secs(10)), "the worker must stop cleanly");
        assert!(bundle_cached, "the bundle was downloaded and verified");
        assert!(events > 0, "no events arrived: {status:?}");
    }
}
