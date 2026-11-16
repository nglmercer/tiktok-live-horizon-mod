//! Minimal HTTP/1.1 client on tokio + rustls: no reqwest, no hyper, no aws-lc.
//! Supports Content-Length, chunked, `Connection: close`, gzip and up to 3 redirects.
use std::io::Read;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use ttl_live_discovery::{HttpError, HttpFuture, HttpGet, HttpRequest, HttpResponse};

const MAX_REDIRECTS: usize = 3;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_HEADER_LINES: usize = 256;

#[derive(Clone)]
pub struct NativeHttp {
    config: Arc<rustls::ClientConfig>,
}

impl std::fmt::Debug for NativeHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeHttp")
    }
}

impl NativeHttp {
    pub fn new() -> Result<NativeHttp, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(NativeHttp { config: Arc::new(config) })
    }

    async fn fetch(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let timeout = request.timeout;
        match tokio::time::timeout(timeout, self.follow(request)).await {
            Ok(result) => result,
            Err(_) => Err(HttpError::Transport("request timed out".to_owned())),
        }
    }

    async fn follow(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut url = url::Url::parse(&request.url).map_err(|e| HttpError::Transport(format!("bad url: {e}")))?;
        let mut headers: Vec<(String, String)> = Vec::new();
        for _ in 0..=MAX_REDIRECTS {
            let response = self.exchange_url(&url, &request).await?;
            headers.extend(response.headers.iter().cloned());
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                let Some(location) = response.headers.iter().find(|(k, _)| k == "location").map(|(_, v)| v.clone()) else {
                    return Ok(HttpResponse { status: response.status, headers, body: response.body });
                };
                url = url.join(&location).map_err(|e| HttpError::Transport(format!("bad redirect: {e}")))?;
                continue;
            }
            return Ok(HttpResponse { status: response.status, headers, body: response.body });
        }
        Err(HttpError::Transport("too many redirects".to_owned()))
    }

    async fn exchange_url(&self, url: &url::Url, request: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let transport = |m: String| HttpError::Transport(m);
        let host = url.host_str().ok_or_else(|| transport("url has no host".to_owned()))?.to_owned();
        let tls = match url.scheme() {
            "https" => true,
            "http" => false,
            other => return Err(transport(format!("unsupported scheme {other}"))),
        };
        let port = url.port_or_known_default().unwrap_or(if tls { 443 } else { 80 });
        let mut target = url.path().to_owned();
        if let Some(query) = url.query() {
            target.push('?');
            target.push_str(query);
        }
        let mut head = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nAccept-Encoding: gzip\r\nConnection: close\r\n");
        head.push_str(&format!("User-Agent: {}\r\n", clean_header(&request.user_agent)));
        for (name, value) in &request.headers {
            if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                head.push_str(&format!("{name}: {}\r\n", clean_header(value)));
            }
        }
        head.push_str("\r\n");

        let tcp = TcpStream::connect((host.as_str(), port)).await.map_err(|e| transport(format!("connect {host}: {e}")))?;
        let _ = tcp.set_nodelay(true);
        if tls {
            let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| transport(format!("bad host: {e}")))?;
            let stream = TlsConnector::from(self.config.clone())
                .connect(name, tcp)
                .await
                .map_err(|e| transport(format!("tls {host}: {e}")))?;
            exchange(stream, head.as_bytes(), request).await
        } else {
            exchange(tcp, head.as_bytes(), request).await
        }
    }
}

/// A header can never carry line breaks (header injection).
fn clean_header(value: &str) -> String {
    value.chars().filter(|c| *c != '\r' && *c != '\n').collect()
}

async fn read_line<R: AsyncBufReadExt + Unpin>(reader: &mut R, budget: &mut usize) -> Result<String, HttpError> {
    let mut line = String::new();
    let read = reader.read_line(&mut line).await.map_err(|e| HttpError::Transport(e.to_string()))?;
    if read == 0 {
        return Err(HttpError::Transport("connection closed early".to_owned()));
    }
    if read > *budget {
        return Err(HttpError::Transport("response headers too large".to_owned()));
    }
    *budget -= read;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(stream: S, head: &[u8], request: &HttpRequest) -> Result<HttpResponse, HttpError> {
    let transport = |e: std::io::Error| HttpError::Transport(e.to_string());
    let mut reader = BufReader::new(stream);
    reader.get_mut().write_all(head).await.map_err(transport)?;
    reader.get_mut().flush().await.map_err(transport)?;

    let mut budget = MAX_HEADER_BYTES;
    let status_line = read_line(&mut reader, &mut budget).await?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| HttpError::Transport(format!("bad status line: {status_line}")))?;
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let line = read_line(&mut reader, &mut budget).await?;
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADER_LINES {
            return Err(HttpError::Transport("too many response headers".to_owned()));
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.to_ascii_lowercase());

    let limit = request.max_body;
    let mut raw: Vec<u8> = Vec::new();
    let mut truncated = false;
    let no_body = status == 204 || status == 304 || (100..200).contains(&status);
    if !no_body {
        if header("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
            loop {
                let size_line = read_line(&mut reader, &mut budget_for_chunks()).await?;
                let size_text = size_line.split(';').next().unwrap_or("").trim();
                let size = usize::from_str_radix(size_text, 16).map_err(|_| HttpError::Transport("bad chunk size".to_owned()))?;
                if size == 0 {
                    // Trailer: lines up to an empty one.
                    for _ in 0..MAX_HEADER_LINES {
                        if read_line(&mut reader, &mut budget_for_chunks()).await?.is_empty() {
                            break;
                        }
                    }
                    break;
                }
                if raw.len().saturating_add(size) > limit {
                    if request.truncate {
                        let room = limit - raw.len();
                        let mut part = vec![0u8; room];
                        reader.read_exact(&mut part).await.map_err(transport)?;
                        raw.extend_from_slice(&part);
                        truncated = true;
                        break;
                    }
                    return Err(HttpError::TooLarge);
                }
                let start = raw.len();
                raw.resize(start + size, 0);
                reader.read_exact(&mut raw[start..]).await.map_err(transport)?;
                let mut crlf = [0u8; 2];
                reader.read_exact(&mut crlf).await.map_err(transport)?;
            }
        } else if let Some(length) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
            if length > limit && !request.truncate {
                return Err(HttpError::TooLarge);
            }
            let take = length.min(limit);
            raw.resize(take, 0);
            reader.read_exact(&mut raw).await.map_err(transport)?;
            truncated = length > limit;
        } else {
            let mut limited = (&mut reader).take(limit as u64 + 1);
            limited.read_to_end(&mut raw).await.map_err(transport)?;
            if raw.len() > limit {
                if !request.truncate {
                    return Err(HttpError::TooLarge);
                }
                raw.truncate(limit);
                truncated = true;
            }
        }
    }

    // With `truncate` the body does not matter (headers only): it is returned without decompressing.
    let body = if !truncated && header("content-encoding").is_some_and(|v| v.contains("gzip")) {
        let mut decoder = flate2::read::GzDecoder::new(raw.as_slice()).take(limit as u64 + 1);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).map_err(|e| HttpError::Transport(format!("gzip: {e}")))?;
        if out.len() > limit {
            return Err(HttpError::TooLarge);
        }
        out
    } else {
        raw
    };
    Ok(HttpResponse { status, headers, body })
}

fn budget_for_chunks() -> usize {
    MAX_HEADER_BYTES
}

impl HttpGet for NativeHttp {
    fn get<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a> {
        Box::pin(self.fetch(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;
    use tokio::net::TcpListener;

    async fn serve(responses: Vec<Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 4096];
                let _ = socket.read(&mut buffer).await;
                socket.write_all(&response).await.unwrap();
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{address}/path?x=1")
    }

    fn request(url: String, max_body: usize, truncate: bool) -> HttpRequest {
        HttpRequest { url, headers: Vec::new(), user_agent: "test".into(), timeout: Duration::from_secs(5), max_body, truncate }
    }

    #[tokio::test]
    async fn reads_content_length_and_headers() {
        let url = serve(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nSet-Cookie: a=b; Path=/\r\n\r\nhello".to_vec()]).await;
        let response = NativeHttp::new().unwrap().get(request(url, 1024, false)).await.unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello");
        assert_eq!(response.header_all("set-cookie").next(), Some("a=b; Path=/"));
    }

    #[tokio::test]
    async fn reads_chunked_and_bounds_it() {
        let wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n12345678\r\n8\r\nabcdefgh\r\n0\r\n\r\n".to_vec();
        let url = serve(vec![wire.clone(), wire]).await;
        let http = NativeHttp::new().unwrap();
        let ok = http.get(request(url.clone(), 1024, false)).await.unwrap();
        assert_eq!(ok.body, b"12345678abcdefgh");
        let too_big = http.get(request(url, 12, false)).await;
        assert_eq!(too_big.unwrap_err(), HttpError::TooLarge);
    }

    #[tokio::test]
    async fn decodes_gzip_within_the_limit() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&vec![b'x'; 10_000]).unwrap();
        let gz = encoder.finish().unwrap();
        let mut wire = format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n", gz.len()).into_bytes();
        wire.extend_from_slice(&gz);
        let url = serve(vec![wire.clone(), wire]).await;
        let http = NativeHttp::new().unwrap();
        assert_eq!(http.get(request(url.clone(), 100_000, false)).await.unwrap().body.len(), 10_000);
        assert_eq!(http.get(request(url, 1_000, false)).await.unwrap_err(), HttpError::TooLarge, "a gzip bomb is bounded");
    }

    #[tokio::test]
    async fn follows_redirects_and_merges_cookies() {
        let second = serve(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nSet-Cookie: b=2\r\n\r\nok".to_vec()]).await;
        let first = serve(vec![format!("HTTP/1.1 302 Found\r\nLocation: {second}\r\nSet-Cookie: a=1\r\nContent-Length: 0\r\n\r\n").into_bytes()]).await;
        let response = NativeHttp::new().unwrap().get(request(first, 1024, false)).await.unwrap();
        assert_eq!(response.body, b"ok");
        assert_eq!(response.header_all("set-cookie").count(), 2);
    }

    #[tokio::test]
    async fn truncate_returns_headers_without_failing() {
        let url = serve(vec![b"HTTP/1.1 200 OK\r\nSet-Cookie: ttwid=zz\r\nContent-Length: 100\r\n\r\n".iter().copied().chain(std::iter::repeat(b'a').take(100)).collect()]).await;
        let response = NativeHttp::new().unwrap().get(request(url, 10, true)).await.unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body.len(), 10);
        assert_eq!(response.header_all("set-cookie").next(), Some("ttwid=zz"));
    }
}
