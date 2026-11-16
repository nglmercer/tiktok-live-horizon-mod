//! Staying connected.
//!
//! A socket signature ages out, rooms restart their push servers, and TikTok closes connections it
//! considers stale. [`LiveConnection`] deliberately does none of that thinking: it reports the close
//! reason and stops. Something has to decide what happens next, and until now nothing did — every
//! caller either ran for a fixed few seconds or was expected to invent its own policy.
//!
//! [`ReconnectingConnection`] is that decision, made once. It takes a [`SignerBackend`] — the same
//! one the sign server uses, so a fix to the signing path reaches both — and re-derives a fresh
//! signed URI on every attempt. Re-signing is the whole point: reopening the *same* URI after its
//! signature has aged is the failure this is meant to avoid, not a shortcut worth taking.
//!
//! What it will not do is retry forever, or retry something that cannot succeed. A rejection is a
//! verdict about the request, not a transient fault; retrying it produces a loop that looks like a
//! connection problem and is really a signing one.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use ttl_sign_core::{RejectReason, SignError, SignOutcome, SignerBackend, TransportRequest};

use crate::{ConnectConfig, LiveConnection, LiveMessage, WsError};

/// How hard to try, and how long to wait between attempts.
///
/// The defaults are deliberately patient rather than aggressive: a room that closed our socket is
/// not helped by an immediate reconnect, and a signing subprocess costs seconds of its own.
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    /// Consecutive failed attempts before giving up. `0` disables reconnection entirely.
    pub max_attempts: u32,
    /// Delay before the first retry. Doubles per consecutive failure.
    pub initial_backoff: Duration,
    /// Ceiling on that doubling.
    pub max_backoff: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
        }
    }
}

impl ReconnectPolicy {
    /// Never reconnect: one connection, and the caller handles the close.
    pub fn none() -> Self {
        Self {
            max_attempts: 0,
            ..Self::default()
        }
    }

    /// Delay before the `attempt`-th consecutive retry, counting from 1.
    ///
    /// Exposed because a policy whose timing cannot be asserted is a policy nobody can review.
    pub fn backoff(&self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return self.initial_backoff.min(self.max_backoff);
        }
        let doubled = self
            .initial_backoff
            .checked_mul(1u32 << (attempt - 1).min(16))
            .unwrap_or(self.max_backoff);
        doubled.min(self.max_backoff)
    }
    /// Positive bounded jitter; caller supplies independent sample/fraction in 0..1.
    /// Existing backoff remains deterministic. Invalid fractions/samples become zero.
    pub fn backoff_with_jitter(&self, attempt: u32, fraction: f64, sample: f64) -> Duration {
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let sample = if sample.is_finite() {
            sample.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Duration::try_from_secs_f64(self.backoff(attempt).as_secs_f64() * (1.0 + fraction * sample))
            .unwrap_or(self.max_backoff)
            .min(self.max_backoff)
    }
}

/// Why a stream stopped for good.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    /// The signer or the service refused. Not retried: the next attempt would be refused too.
    #[error("the transport was refused: {0}")]
    Refused(RejectReason),

    /// The backend could not produce a signature at all.
    #[error("the signer failed: {0}")]
    Signer(SignError),

    /// Reconnection ran out of attempts. Carries the failure that ended the last one.
    #[error("gave up after {attempts} reconnection attempt(s): {last}")]
    Exhausted { attempts: u32, last: WsError },

    /// The first connection never opened.
    #[error(transparent)]
    Connect(WsError),
}

/// A message stream that outlives any single socket.
///
/// Use it exactly like [`LiveConnection`]: call [`ReconnectingConnection::next_message`] in a loop.
/// The difference is what happens on a close — instead of ending the stream, it re-signs, reopens,
/// and keeps going until the policy runs out.
pub struct ReconnectingConnection {
    backend: Arc<dyn SignerBackend>,
    room_id: String,
    config: ConnectConfig,
    policy: ReconnectPolicy,
    connection: Option<LiveConnection>,
    reconnects: u64,
    jitter: Option<(f64, Box<dyn Fn() -> f64 + Send + Sync>)>,
}

impl ReconnectingConnection {
    /// Open the first connection, or fail with the reason it could not open.
    pub async fn open(
        backend: Arc<dyn SignerBackend>,
        room_id: impl Into<String>,
        config: ConnectConfig,
        policy: ReconnectPolicy,
    ) -> Result<Self, StreamError> {
        let room_id = room_id.into();
        let connection = connect_once(backend.as_ref(), &room_id, &config).await?;
        Ok(Self {
            backend,
            room_id,
            config,
            policy,
            connection: Some(connection),
            reconnects: 0,
            jitter: None,
        })
    }

    /// Opt into jitter without altering the public policy struct or default behavior.
    pub fn with_jitter(
        mut self,
        fraction: f64,
        sample: impl Fn() -> f64 + Send + Sync + 'static,
    ) -> Self {
        self.jitter = Some((fraction, Box::new(sample)));
        self
    }

    /// How many times the stream has re-established itself. Worth logging: a climbing count with
    /// messages still arriving is a healthy stream on a flaky path, and one without is not.
    pub fn reconnects(&self) -> u64 {
        self.reconnects
    }

    /// The next event frame, reconnecting as needed.
    ///
    /// Returns `None` only when the stream is finished and will not resume.
    pub async fn next_message(&mut self) -> Option<Result<LiveMessage, StreamError>> {
        loop {
            let connection = self.connection.as_mut()?;
            match connection.next_message().await {
                Some(Ok(message)) => return Some(Ok(message)),
                // A clean end and a closed socket are the same thing here: the socket is gone and
                // the only question is whether to build another.
                None => {
                    debug!(room_id = %self.room_id, "socket closed cleanly");
                    match self
                        .reconnect(WsError::Closed("closed by peer".into()))
                        .await
                    {
                        Ok(()) => continue,
                        Err(error) => {
                            self.connection = None;
                            return Some(Err(error));
                        }
                    }
                }
                Some(Err(error)) => match self.reconnect(error).await {
                    Ok(()) => continue,
                    Err(error) => {
                        self.connection = None;
                        return Some(Err(error));
                    }
                },
            }
        }
    }

    /// Re-sign and reopen, backing off between attempts.
    async fn reconnect(&mut self, cause: WsError) -> Result<(), StreamError> {
        self.connection = None;
        if self.policy.max_attempts == 0 {
            return Err(StreamError::Exhausted {
                attempts: 0,
                last: cause,
            });
        }

        warn!(room_id = %self.room_id, %cause, "connection lost; reconnecting");
        let mut last = cause;
        for attempt in 1..=self.policy.max_attempts {
            let delay = self.jitter.as_ref().map_or_else(
                || self.policy.backoff(attempt),
                |(fraction, sample)| {
                    self.policy
                        .backoff_with_jitter(attempt, *fraction, sample())
                },
            );
            debug!(room_id = %self.room_id, attempt, ?delay, "waiting before reconnect");
            tokio::time::sleep(delay).await;

            match connect_once(self.backend.as_ref(), &self.room_id, &self.config).await {
                Ok(connection) => {
                    self.connection = Some(connection);
                    self.reconnects += 1;
                    info!(
                        room_id = %self.room_id,
                        attempt,
                        reconnects = self.reconnects,
                        "reconnected"
                    );
                    return Ok(());
                }
                // A refusal will be refused again. Failing here, with the reason, beats spending
                // the remaining attempts to arrive at the same place with a timing error instead.
                Err(error @ (StreamError::Refused(_) | StreamError::Signer(_))) => {
                    return Err(error)
                }
                Err(StreamError::Connect(error)) => {
                    warn!(room_id = %self.room_id, attempt, %error, "reconnect attempt failed");
                    last = error;
                }
                Err(other) => return Err(other),
            }
        }

        Err(StreamError::Exhausted {
            attempts: self.policy.max_attempts,
            last,
        })
    }

    /// Close the current socket, if one is open.
    pub async fn close(mut self) {
        if let Some(connection) = self.connection.take() {
            connection.close().await;
        }
    }
}

/// Sign a fresh URI and open it. One attempt, no retries, no waiting.
async fn connect_once(
    backend: &dyn SignerBackend,
    room_id: &str,
    config: &ConnectConfig,
) -> Result<LiveConnection, StreamError> {
    let signed = match backend.transport(TransportRequest::new(room_id)).await {
        SignOutcome::Ok(signed) => signed,
        SignOutcome::Rejected(reason) => return Err(StreamError::Refused(reason)),
        SignOutcome::Transport(error) => return Err(StreamError::Signer(error)),
    };
    LiveConnection::open_uri(
        &signed.signed_url,
        &signed.cookies,
        &signed.user_agent,
        "",
        config,
    )
    .await
    .map_err(StreamError::Connect)
}

