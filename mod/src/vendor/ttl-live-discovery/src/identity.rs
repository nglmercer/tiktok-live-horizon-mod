use std::time::Duration;

use ttl_sign_core::CookieJar;

use crate::{HttpError, HttpGet, HttpRequest};

const DEFAULT_GUEST_URL: &str = "https://www.tiktok.com/live";

#[derive(Debug, Clone)]
pub struct GuestIdentity {
    pub cookies: CookieJar,
    pub device_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("guest bootstrap transport failed: {0}")]
    Transport(String),
    #[error("TikTok refused guest bootstrap with HTTP {0}")]
    Refused(u16),
    #[error("TikTok returned no usable guest cookies")]
    NoCookies,
}

/// Bootstrap one reusable anonymous web identity.
///
/// The identity is created once at process startup and passed to both discovery and the warm
/// signer. It is intentionally not persisted by this helper; deployments that want a reusable
/// account/session should supply `TTL_SESSION_COOKIE` or `TTL_SESSION_FILE` instead.
pub async fn bootstrap_guest_identity(
    http: &dyn HttpGet,
    user_agent: &str,
    timeout: Duration,
) -> Result<GuestIdentity, IdentityError> {
    let response = http
        .get(HttpRequest {
            url: DEFAULT_GUEST_URL.to_owned(),
            headers: vec![("accept-language".to_owned(), "en-US,en;q=0.9".to_owned())],
            user_agent: user_agent.to_owned(),
            timeout,
            // The body is discarded: reading up to 2 MiB is enough.
            max_body: 2 * 1024 * 1024,
            truncate: true,
        })
        .await
        .map_err(|error| match error {
            HttpError::Transport(message) => IdentityError::Transport(message),
            HttpError::TooLarge => IdentityError::Transport("response too large".to_owned()),
        })?;
    let status = response.status;
    let mut cookies = CookieJar::new();
    for line in response.header_all("set-cookie") {
        let pair = line.split(';').next().unwrap_or_default();
        let Some((name, cookie_value)) = pair.split_once('=') else {
            continue;
        };
        if !name.trim().is_empty() {
            cookies.set(name.trim(), cookie_value.trim());
        }
    }

    if !(200..300).contains(&status) {
        return Err(IdentityError::Refused(status));
    }
    if cookies.get("ttwid").is_none_or(str::is_empty) {
        return Err(IdentityError::NoCookies);
    }
    let device_id = cookies
        .get("tt_webid_v2")
        .or_else(|| cookies.get("tt_webid"))
        .map(str::to_owned);
    Ok(GuestIdentity { cookies, device_id })
}

