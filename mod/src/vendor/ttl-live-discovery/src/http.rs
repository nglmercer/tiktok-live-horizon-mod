//! The HTTP transport of discovery. This crate ships no client: whoever embeds it (the Horizon
//! mod) implements `HttpGet` on top of tokio-rustls. It replaces `reqwest`.
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    Transport(String),
    TooLarge,
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub user_agent: String,
    pub timeout: Duration,
    /// Limit of the decoded body.
    pub max_body: usize,
    /// `true`: at the limit, what was read is returned instead of `TooLarge`.
    pub truncate: bool,
}

#[derive(Debug, Clone, Default)]
pub struct HttpResponse {
    pub status: u16,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>>;

pub trait HttpGet: Send + Sync + std::fmt::Debug {
    fn get<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a>;
}
