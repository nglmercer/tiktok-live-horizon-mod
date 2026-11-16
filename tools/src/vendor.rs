//! `cargo xtask vendor-signer <signer-checkout> <proto-out-dir>`: vendors the TikTok signer crates
//! into `mod/src/vendor/`.
//!
//! * `<signer-checkout>`: a checkout of `nglmercer/tiktok-signer` at the pinned commit (26ae095).
//! * `<proto-out-dir>`: the `out/` folder that `cargo build -p ttl-live-proto` generated in it.
//!
//! A native package only accepts `.rs` and `.toml` files under `src/` and rejects `build.rs`, so:
//! * `ttl-live-proto`: the code `build.rs` generated is copied in (`generated/`).
//! * `ttl-sign-embedded`: `bootstrap.js` becomes a Rust constant.
//! * `ttl-live-discovery`: `reqwest` is replaced by the `HttpGet` trait (the real client lives in
//!   the mod's own crate).
//!
//! The edits are mechanical and live here, in plain sight; the rest of the signer is untouched.
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::util::{files_below, raw_string, read, root, write, Result};

const CRATES: [&str; 7] = [
    "ttl-sign-core",
    "ttl-live-ws",
    "ttl-live-events",
    "ttl-live-proto",
    "ttl-live-core",
    "ttl-sign-embedded",
    "ttl-live-discovery",
];

/// Each crate's dependencies without `workspace = true`. The exact pins of the packages the game
/// precompiles live in mod/Cargo.toml; only version ranges are listed here.
fn dependencies(name: &str) -> &'static str {
    match name {
        "ttl-sign-core" => "rand = \"0.10\"\nserde = { version = \"1\", features = [\"derive\"] }\nserde_json = \"1\"\nthiserror = \"2\"\n",
        "ttl-live-ws" => "ttl-sign-core = { path = \"../ttl-sign-core\" }\nflate2 = \"1\"\nfutures-util = { version = \"0.3\", default-features = false, features = [\"sink\", \"std\"] }\nthiserror = \"2\"\ntokio = { version = \"1\", features = [\"macros\", \"net\", \"rt\", \"sync\", \"time\"] }\ntokio-tungstenite = { version = \"0.28\", features = [\"rustls-tls-webpki-roots\"] }\ntracing = \"0.1\"\n",
        "ttl-live-events" => "ttl-live-proto = { path = \"../ttl-live-proto\" }\nttl-sign-core = { path = \"../ttl-sign-core\" }\nprost = \"0.14\"\nserde = { version = \"1\", features = [\"derive\"] }\nthiserror = \"2\"\nserde_json = \"1\"\nbase64 = \"0.23\"\nurl = \"2.5\"\n",
        "ttl-live-proto" => "prost = \"0.14\"\n",
        "ttl-live-core" => "ttl-sign-core = { path = \"../ttl-sign-core\" }\nttl-live-ws = { path = \"../ttl-live-ws\" }\nttl-live-events = { path = \"../ttl-live-events\" }\ntokio = { version = \"1\", features = [\"macros\", \"rt\", \"sync\", \"time\"] }\nrustls = { version = \"0.23\", default-features = false, features = [\"ring\", \"std\", \"tls12\"] }\n",
        "ttl-sign-embedded" => "ttl-live-discovery = { path = \"../ttl-live-discovery\" }\nrand = \"0.10\"\nrquickjs = \"0.14\"\nthiserror = \"2\"\ntokio = { version = \"1\", features = [\"sync\"] }\n",
        "ttl-live-discovery" => "ttl-sign-core = { path = \"../ttl-sign-core\" }\nserde_json = \"1\"\nthiserror = \"2\"\ntracing = \"0.1\"\ntokio = { version = \"1\", features = [\"time\"] }\n",
        _ => "",
    }
}

fn manifest(name: &str) -> String {
    let license = if name == "ttl-live-proto" { "AGPL-3.0-only" } else { "MIT" };
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"{license}\"\npublish = false\n\n[dependencies]\n{}", dependencies(name))
}

const HTTP_RS: &str = "//! The HTTP transport of discovery. This crate ships no client: whoever embeds it (the Horizon\n//! mod) implements `HttpGet` on top of tokio-rustls. It replaces `reqwest`.\nuse std::future::Future;\nuse std::pin::Pin;\nuse std::time::Duration;\n\n#[derive(Debug, Clone, PartialEq, Eq)]\npub enum HttpError {\n    Transport(String),\n    TooLarge,\n}\n\n#[derive(Debug, Clone)]\npub struct HttpRequest {\n    pub url: String,\n    pub headers: Vec<(String, String)>,\n    pub user_agent: String,\n    pub timeout: Duration,\n    /// Limit of the decoded body.\n    pub max_body: usize,\n    /// `true`: at the limit, what was read is returned instead of `TooLarge`.\n    pub truncate: bool,\n}\n\n#[derive(Debug, Clone, Default)]\npub struct HttpResponse {\n    pub status: u16,\n    /// Lower-case names.\n    pub headers: Vec<(String, String)>,\n    pub body: Vec<u8>,\n}\n\nimpl HttpResponse {\n    pub fn header_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {\n        self.headers\n            .iter()\n            .filter(move |(key, _)| key.eq_ignore_ascii_case(name))\n            .map(|(_, value)| value.as_str())\n    }\n}\n\npub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>>;\n\npub trait HttpGet: Send + Sync + std::fmt::Debug {\n    fn get<'a>(&'a self, request: HttpRequest) -> HttpFuture<'a>;\n}\n";

fn read_file(path: &Path) -> Result<String> {
    read(path)
}

/// Copies every `.rs` file of a signer crate and writes its manifest.
fn copy_sources(signer: &Path, dest: &Path, name: &str) -> Result<()> {
    let source = signer.join(name).join("src");
    let target = dest.join(name).join("src");
    for path in files_below(&source, ".rs")? {
        let relative = path.strip_prefix(&source).map_err(|e| e.to_string())?;
        write(&target.join(relative), &read_file(&path)?)?;
    }
    write(&dest.join(name).join("Cargo.toml"), &manifest(name))
}

/// Replaces the first occurrence of `old` in the file; fails if it is not there.
fn patch(path: &Path, old: &str, new: &str) -> Result<()> {
    let text = read_file(path)?;
    if !text.contains(old) {
        return Err(format!("patch does not apply to {}: {:?}", path.display(), old.chars().take(60).collect::<String>()));
    }
    write(path, &text.replacen(old, new, 1))
}

/// Byte offset of `needle` in `text`, as a Result.
fn find(text: &str, needle: &str) -> Result<usize> {
    text.find(needle).ok_or_else(|| format!("marker not found: {:?}", needle.chars().take(60).collect::<String>()))
}

/// Removes the top-level `#[cfg(test)] mod name { ... }` blocks (they end at a line that is `}`).
fn strip_tests(path: &Path) -> Result<()> {
    let text = read_file(path)?;
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim() == "#[cfg(test)]" && lines.get(i + 1).is_some_and(|next| is_mod_open(next)) {
            let mut j = i + 2;
            while j < lines.len() && lines[j] != "}" {
                j += 1;
            }
            i = j + 1;
            continue;
        }
        out.push(lines[i]);
        i += 1;
    }
    write(path, &out.join("\n"))
}

/// `mod name {` at the start of the line.
fn is_mod_open(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("mod ") else { return false };
    let name_len = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').count();
    name_len > 0 && rest[name_len..].starts_with(" {")
}

/// `concat!(env!("OUT_DIR"), "/file.rs")` becomes `"file.rs"` in generated code.
fn strip_out_dir(text: &str) -> String {
    const PREFIX: &str = "concat!(env!(\"OUT_DIR\"), \"/";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(PREFIX) {
        let after = &rest[at + PREFIX.len()..];
        match after.find('"').filter(|&end| end > 0 && after[end + 1..].starts_with(')')) {
            Some(end) => {
                out.push_str(&rest[..at]);
                out.push('"');
                out.push_str(&after[..end]);
                out.push('"');
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[..at + PREFIX.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Drops the `DESCRIPTOR_BYTES` constant (only `ttl-live-native` uses it).
fn drop_descriptor(text: &str) -> String {
    const START: &str = "/// SHA-256 of the generated descriptor set";
    let Some(at) = text.find(START) else { return text.to_owned() };
    let Some(first_end) = text[at..].find('\n').map(|n| at + n + 1) else { return text.to_owned() };
    if !text[first_end..].starts_with("pub const DESCRIPTOR_BYTES") {
        return text.to_owned();
    }
    let second_end = text[first_end..].find('\n').map_or(text.len(), |n| first_end + n + 1);
    format!("{}// DESCRIPTOR_BYTES is not vendored: only ttl-live-native uses it.\n{}", &text[..at], &text[second_end..])
}

fn patch_discovery(root: &Path) -> Result<()> {
    let src = root.join("src");
    write(&src.join("http.rs"), HTTP_RS)?;
    let lib = src.join("lib.rs");
    let mut text = read_file(&lib)?;

    let start = find(&text, "// Check every decoded HTTP chunk")?;
    let end = find(&text, "/// A discovery operation and what it needs")?;
    text = format!(
        "{}fn bounded_text(response: HttpResponse, limit: usize) -> Result<String, DiscoveryError> {{\n    if response.body.len() > limit {{\n        return Err(DiscoveryError::TooLarge(limit));\n    }}\n    Ok(String::from_utf8_lossy(&response.body).into_owned())\n}}\n\n{}",
        &text[..start],
        &text[end..]
    );
    text = text.replacen("use std::time::Duration;\n", "use std::sync::Arc;\nuse std::time::Duration;\n\nmod http;\npub use http::{HttpError, HttpFuture, HttpGet, HttpRequest, HttpResponse};\n", 1);
    text = text.replace("/// Holds one `reqwest::Client` so connections are pooled across lookups.", "/// Holds the HTTP transport (an `HttpGet`) shared by every lookup.");
    text = text.replacen("    http: reqwest::Client,\n", "    http: Arc<dyn HttpGet>,\n    user_agent: String,\n    timeout: Duration,\n", 1);

    let (a, b) = (find(&text, "    pub fn new(preset: &Preset)")?, find(&text, "    /// Attach the session presented with signed requests.")?);
    let old_constructor = text[a..b].to_owned();
    let new_constructor = "    pub fn new(preset: &Preset, http: Arc<dyn HttpGet>) -> Result<Self, DiscoveryError> {\n        Self::with_timeout(preset, http, DEFAULT_TIMEOUT)\n    }\n\n    pub fn with_timeout(\n        preset: &Preset,\n        http: Arc<dyn HttpGet>,\n        timeout: Duration,\n    ) -> Result<Self, DiscoveryError> {\n        Ok(Self {\n            http,\n            user_agent: preset.user_agent().to_owned(),\n            timeout,\n            session: String::new(),\n        })\n    }\n\n    async fn fetch(\n        &self,\n        url: &str,\n        headers: Vec<(String, String)>,\n        max_body: usize,\n    ) -> Result<HttpResponse, DiscoveryError> {\n        self.http\n            .get(HttpRequest {\n                url: url.to_owned(),\n                headers,\n                user_agent: self.user_agent.clone(),\n                timeout: self.timeout,\n                max_body,\n                truncate: false,\n            })\n            .await\n            .map_err(|error| match error {\n                HttpError::TooLarge => DiscoveryError::TooLarge(max_body),\n                HttpError::Transport(message) => DiscoveryError::Transport(message),\n            })\n    }\n\n";
    text = text.replace(&old_constructor, new_constructor);

    let (a, b) = (find(&text, "        let response = self\n            .http\n            .get(&url)")?, find(&text, "        interpret_room_lookup(unique_id, status, &body)")?);
    text = format!(
        "{}        let response = self.fetch(&url, Vec::new(), MAX_BODY_BYTES).await?;\n        let status = response.status;\n        let body = bounded_text(response, MAX_BODY_BYTES)?;\n\n{}",
        &text[..a],
        &text[b..]
    );

    let (a, b) = (find(&text, "        let mut request = self\n            .http\n            .get(url)")?, find(&text, "        if body.is_empty() {")?);
    text = format!(
        "{}        let mut headers = vec![\n            (\"referer\".to_owned(), \"https://www.tiktok.com/\".to_owned()),\n            (\"origin\".to_owned(), \"https://www.tiktok.com\".to_owned()),\n        ];\n        if !self.session.is_empty() {{\n            headers.push((\"cookie\".to_owned(), self.session.clone()));\n        }}\n        let response = self.fetch(url, headers, 16 * 1024 * 1024).await?;\n        let status = response.status;\n        if !(200..300).contains(&status) {{\n            return Err(DiscoveryError::Status {{ status }});\n        }}\n        let body = bounded_text(response, 16 * 1024 * 1024)?;\n{}",
        &text[..a],
        &text[b..]
    );
    write(&lib, &text)?;

    let identity = src.join("identity.rs");
    let mut text = read_file(&identity)?;
    let (a, b) = (find(&text, "pub async fn bootstrap_guest_identity(")?, find(&text, "    if !(200..300).contains(&status) {")?);
    text = format!(
        "{}pub async fn bootstrap_guest_identity(\n    http: &dyn HttpGet,\n    user_agent: &str,\n    timeout: Duration,\n) -> Result<GuestIdentity, IdentityError> {{\n    let response = http\n        .get(HttpRequest {{\n            url: DEFAULT_GUEST_URL.to_owned(),\n            headers: vec![(\"accept-language\".to_owned(), \"en-US,en;q=0.9\".to_owned())],\n            user_agent: user_agent.to_owned(),\n            timeout,\n            // The body is discarded: reading up to 2 MiB is enough.\n            max_body: 2 * 1024 * 1024,\n            truncate: true,\n        }})\n        .await\n        .map_err(|error| match error {{\n            HttpError::Transport(message) => IdentityError::Transport(message),\n            HttpError::TooLarge => IdentityError::Transport(\"response too large\".to_owned()),\n        }})?;\n    let status = response.status;\n    let mut cookies = CookieJar::new();\n    for line in response.header_all(\"set-cookie\") {{\n        let pair = line.split(';').next().unwrap_or_default();\n        let Some((name, cookie_value)) = pair.split_once('=') else {{\n            continue;\n        }};\n        if !name.trim().is_empty() {{\n            cookies.set(name.trim(), cookie_value.trim());\n        }}\n    }}\n\n{}",
        &text[..a],
        &text[b..]
    );
    text = text.replacen("use ttl_sign_core::CookieJar;\n", "use ttl_sign_core::CookieJar;\n\nuse crate::{HttpError, HttpGet, HttpRequest};\n", 1);
    write(&identity, &text)
}

pub fn run(args: &[String]) -> Result<ExitCode> {
    let [checkout, generated] = args else {
        return Err("usage: cargo xtask vendor-signer <signer-checkout> <proto-out-dir>".to_owned());
    };
    let signer = PathBuf::from(checkout).join("crates");
    let generated = PathBuf::from(generated);
    let dest = root().join("mod/src/vendor");
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("cannot clear {}: {e}", dest.display()))?;
    }
    for name in CRATES {
        copy_sources(&signer, &dest, name)?;
    }

    // ttl-live-proto: the generated code instead of build.rs.
    let proto = dest.join("ttl-live-proto/src");
    for path in files_below(&generated, ".rs")?.into_iter().filter(|p| p.parent() == Some(generated.as_path())) {
        let name = path.file_name().ok_or("generated file without a name")?;
        write(&proto.join("generated").join(name), &strip_out_dir(&read_file(&path)?))?;
    }
    patch(&proto.join("lib.rs"), "include!(concat!(env!(\"OUT_DIR\"), \"/v3.rs\"));", "include!(\"generated/v3.rs\");")?;
    patch(&proto.join("registry.rs"), "include!(concat!(env!(\"OUT_DIR\"), \"/schema_registry.rs\"));", "include!(\"generated/schema_registry.rs\");")?;
    let registry = proto.join("registry.rs");
    write(&registry, &drop_descriptor(&read_file(&registry)?))?;

    // ttl-sign-embedded: bootstrap.js as a Rust constant.
    let embedded = dest.join("ttl-sign-embedded/src");
    let bootstrap = read_file(&signer.join("ttl-sign-embedded/bootstrap.js"))?;
    write(
        &embedded.join("bootstrap_js.rs"),
        &format!("//! Generated by `cargo xtask vendor-signer` from bootstrap.js. Do not edit.\npub const BOOTSTRAP: &str = {};\n", raw_string(&bootstrap)),
    )?;
    patch(
        &embedded.join("lib.rs"),
        "pub(crate) const BOOTSTRAP: &str = include_str!(\"../bootstrap.js\");",
        "mod bootstrap_js;\npub(crate) use bootstrap_js::BOOTSTRAP;",
    )?;

    patch_discovery(&dest.join("ttl-live-discovery"))?;

    // The signer's test modules use fixtures outside the package and depend on reqwest.
    for name in CRATES {
        let files = if name == "ttl-live-proto" {
            vec![dest.join(name).join("src/lib.rs"), dest.join(name).join("src/registry.rs")]
        } else {
            files_below(&dest.join(name).join("src"), ".rs")?
        };
        for file in files {
            strip_tests(&file)?;
        }
    }
    println!("vendored into {}", dest.display());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_dir_includes_become_plain_paths() {
        let text = "x(concat!(env!(\"OUT_DIR\"), \"/v3.rs\")) y";
        assert_eq!(strip_out_dir(text), "x(\"v3.rs\") y");
        assert_eq!(strip_out_dir("nothing here"), "nothing here");
    }

    #[test]
    fn test_modules_are_detected() {
        assert!(is_mod_open("mod tests {"));
        assert!(!is_mod_open("mod tests;"));
        assert!(!is_mod_open("pub mod tests {"));
    }

    #[test]
    fn the_descriptor_constant_is_dropped() {
        let text = "a\n/// SHA-256 of the generated descriptor set x\npub const DESCRIPTOR_BYTES: &[u8] = &[];\nb\n";
        assert_eq!(drop_descriptor(text), "a\n// DESCRIPTOR_BYTES is not vendored: only ttl-live-native uses it.\nb\n");
    }
}
