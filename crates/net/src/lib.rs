//! Networking: turning what the user typed into a URL, and fetching documents.
//!
//! Everything here is blocking; callers run it on a background executor.

use anyhow::{Context as _, Result, bail};
use ureq::ResponseExt as _;
use url::Url;

/// Largest document body we'll read, to keep a hostile page from eating memory.
const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

const USER_AGENT: &str = concat!("Feather/", env!("CARGO_PKG_VERSION"));

/// A fetched document.
#[derive(Debug, Clone)]
pub struct Response {
    /// The URL after following redirects; relative links resolve against this.
    pub url: Url,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
    /// A 401 asking for HTTP Basic credentials: the realm it named.
    pub auth_realm: Option<String>,
}

/// Where the browser keeps state between runs (the cookie jar). `None`
/// (the default, and screenshot runs): memory only.
static STATE_DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();

/// Set up persistence: load saved cookies from `dir`, save them there later.
pub fn init(dir: Option<std::path::PathBuf>) {
    if let Some(dir) = &dir
        && let Ok(file) = std::fs::File::open(dir.join("cookies.json"))
    {
        let _ = agent().cookie_jar_lock().load_json(std::io::BufReader::new(file));
    }
    STATE_DIR.set(dir).ok();
}

/// Write the cookie jar out (after responses that may have set cookies).
fn save_cookies() {
    let Some(Some(dir)) = STATE_DIR.get() else { return };
    let mut out = Vec::new();
    if agent().cookie_jar_lock().save_json(&mut out).is_ok() {
        std::fs::create_dir_all(dir).ok();
        // Write then rename, so a crash mid-write can't eat the jar.
        let tmp = dir.join("cookies.json.tmp");
        if std::fs::write(&tmp, &out).is_ok() {
            std::fs::rename(tmp, dir.join("cookies.json")).ok();
        }
    }
}

/// HTTP Basic credentials, by origin, once the user has given them (or the
/// URL carried them: `https://user:pass@host/`).
mod auth {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use url::Url;

    fn store() -> &'static Mutex<HashMap<String, (String, String)>> {
        static STORE: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
        STORE.get_or_init(Default::default)
    }

    pub fn set(url: &Url, user: &str, password: &str) {
        store().lock().unwrap().insert(url.origin().ascii_serialization(), (user.to_owned(), password.to_owned()));
    }

    /// The `Authorization` header for a request to `url`, if we have credentials.
    pub fn header(url: &Url) -> Option<String> {
        use base64::Engine as _;
        let from_url = (!url.username().is_empty())
            .then(|| (percent_decode(url.username()), percent_decode(url.password().unwrap_or_default())));
        let creds = from_url.or_else(|| store().lock().unwrap().get(&url.origin().ascii_serialization()).cloned())?;
        Some(format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", creds.0, creds.1))))
    }

    fn percent_decode(s: &str) -> String {
        percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
    }
}

/// Remember HTTP Basic credentials for `url`'s origin (after a sign-in prompt).
pub fn set_credentials(url: &Url, user: &str, password: &str) {
    auth::set(url, user, password);
}

/// The realm of a `WWW-Authenticate: Basic realm="..."` challenge.
fn basic_realm(header: &str) -> Option<String> {
    let rest = header.trim();
    if !rest.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("basic")) {
        return None;
    }
    let realm = rest.split("realm=").nth(1).map(|r| r.trim_matches(|c| c == '"' || c == ' ').split('"').next().unwrap_or("").to_owned());
    Some(realm.unwrap_or_default())
}

/// What a document request accepts (roughly a browser's).
const ACCEPT_DOCUMENT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";

/// Interpret URL-bar input: full URLs pass through, bare hosts get `https://`.
pub fn parse_input(input: &str) -> Result<Url> {
    let input = input.trim();
    if input.is_empty() {
        bail!("empty address");
    }
    // Local paths: "/Users/me/page.html", "~/notes.txt".
    if input.starts_with('/') || input.starts_with("~/") {
        let path = match input.strip_prefix("~/") {
            Some(rest) => std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
            None => input.into(),
        };
        return Url::from_file_path(&path).map_err(|()| anyhow::anyhow!("not a valid path: {input}"));
    }
    // `view-source:https://...`: the page's HTML, highlighted.
    if let Some(inner) = input.strip_prefix("view-source:") {
        let inner = parse_input(inner)?;
        return Url::parse(&format!("view-source:{inner}")).context("not a valid address");
    }
    match Url::parse(input) {
        Ok(url) if matches!(url.scheme(), "http" | "https" | "file") => Ok(url),
        // "localhost:8080" parses with scheme "localhost"; treat it as a host.
        Ok(_) if !input.contains("://") => {
            Url::parse(&format!("https://{input}")).context("not a valid address")
        }
        Ok(url) => bail!("unsupported scheme: {}", url.scheme()),
        // "example.com/path" parses as a relative URL error, not as a host.
        Err(_) => Url::parse(&format!("https://{input}")).context("not a valid address"),
    }
}

/// Largest subresource (image) we'll download.
const MAX_RESOURCE_BYTES: u64 = 16 * 1024 * 1024;

/// One shared agent so connections (and TLS sessions) are reused across a
/// page's many subresource requests.
fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .timeout_global(Some(std::time::Duration::from_secs(30)))
            .build()
            .into()
    })
}

/// Fetch a URL, following redirects. Non-2xx responses still return their body
/// (error pages are documents too).
pub fn fetch(url: &Url) -> Result<Response> {
    fetch_document(url, None, None)
}

/// Submit a form with `POST` (`application/x-www-form-urlencoded`).
pub fn fetch_post(url: &Url, body: &str) -> Result<Response> {
    fetch_document(url, None, Some(body))
}

/// Fetch a document (a navigation): GET, or POST a urlencoded form body.
/// Cookies are sent and stored; `referer` is the page the navigation
/// came from (not sent from https to http, like browsers).
pub fn fetch_document(url: &Url, referer: Option<&Url>, post: Option<&str>) -> Result<Response> {
    if url.scheme() == "file" && post.is_none() {
        return file::fetch(url);
    }
    if !matches!(url.scheme(), "http" | "https") {
        bail!("unsupported scheme: {}", url.scheme());
    }
    // Credentials in the URL go in a header, not on the wire's URL.
    let mut clean = url.clone();
    let _ = clean.set_username("");
    let _ = clean.set_password(None);
    let referer = referer
        .filter(|r| matches!(r.scheme(), "http" | "https"))
        .filter(|r| !(r.scheme() == "https" && url.scheme() == "http"))
        .map(|r| {
            let mut r = r.clone();
            r.set_fragment(None);
            let _ = r.set_username("");
            let _ = r.set_password(None);
            r.to_string()
        });
    let result = match post {
        Some(body) => {
            let mut req = agent()
                .post(clean.as_str())
                .header("accept", ACCEPT_DOCUMENT)
                .header("accept-language", ACCEPT_LANGUAGE)
                .header("content-type", "application/x-www-form-urlencoded");
            if let Some(r) = &referer {
                req = req.header("referer", r);
            }
            if let Some(a) = auth::header(url) {
                req = req.header("authorization", a);
            }
            req.send(body)
        }
        None => {
            let mut req = agent().get(clean.as_str()).header("accept", ACCEPT_DOCUMENT).header("accept-language", ACCEPT_LANGUAGE);
            if let Some(r) = &referer {
                req = req.header("referer", r);
            }
            if let Some(a) = auth::header(url) {
                req = req.header("authorization", a);
            }
            req.call()
        }
    };
    let response = result.with_context(|| format!("failed to fetch {url}"))?;
    let document = read_document(&clean, response);
    save_cookies();
    document
}

fn read_document(url: &Url, mut response: ureq::http::Response<ureq::Body>) -> Result<Response> {

    let final_url = Url::parse(&response.get_uri().to_string()).unwrap_or_else(|_| url.clone());
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let auth_realm = (status == 401)
        .then(|| response.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).and_then(basic_realm))
        .flatten();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .lossy_utf8(true)
        .read_to_string()
        .context("failed to read response body")?;

    Ok(Response {
        url: final_url,
        status,
        content_type,
        body,
        auth_realm,
    })
}

/// In-memory cache for subresources (stylesheets, images): navigating around
/// one site shouldn't re-download the same CSS and logos every time.
mod cache {
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex, OnceLock};

    const MAX_BYTES: usize = 96 * 1024 * 1024;

    #[derive(Default)]
    struct Cache {
        entries: HashMap<String, Arc<Vec<u8>>>,
        order: VecDeque<String>,
        bytes: usize,
    }

    fn cache() -> &'static Mutex<Cache> {
        static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
        CACHE.get_or_init(Default::default)
    }

    pub fn get(url: &str) -> Option<Arc<Vec<u8>>> {
        cache().lock().unwrap().entries.get(url).cloned()
    }

    pub fn put(url: &str, body: Arc<Vec<u8>>) {
        let mut c = cache().lock().unwrap();
        if body.len() > MAX_BYTES / 8 || c.entries.contains_key(url) {
            return;
        }
        c.bytes += body.len();
        c.entries.insert(url.to_owned(), body);
        c.order.push_back(url.to_owned());
        while c.bytes > MAX_BYTES {
            let Some(oldest) = c.order.pop_front() else { break };
            if let Some(evicted) = c.entries.remove(&oldest) {
                c.bytes -= evicted.len();
            }
        }
    }
}

/// Fetch a subresource as text (stylesheets), through the cache. `None` on failure.
pub fn fetch_text_cached(url: &Url) -> Option<String> {
    let bytes = fetch_bytes(url).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Fetch a subresource as raw bytes (images etc), through the cache.
/// Supports `data:` URLs. Fails on non-2xx responses.
pub fn fetch_bytes(url: &Url) -> Result<Vec<u8>> {
    if url.scheme() == "data" {
        return decode_data_url(url.as_str());
    }
    if url.scheme() == "file" {
        // Not cached: local files change under you while you edit them.
        return file::read(url);
    }
    if let Some(hit) = cache::get(url.as_str()) {
        return Ok((*hit).clone());
    }
    let body = fetch_bytes_uncached(url)?;
    cache::put(url.as_str(), std::sync::Arc::new(body.clone()));
    Ok(body)
}

fn fetch_bytes_uncached(url: &Url) -> Result<Vec<u8>> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("unsupported scheme: {}", url.scheme());
    }
    let mut req = agent().get(url.as_str()).header("accept-language", ACCEPT_LANGUAGE);
    if let Some(a) = auth::header(url) {
        req = req.header("authorization", a);
    }
    let mut response = req.call().with_context(|| format!("failed to fetch {url}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        bail!("HTTP {status} for {url}");
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_RESOURCE_BYTES)
        .read_to_vec()
        .context("failed to read response body")
}

/// `file://` URLs: local files, and directories as listings.
mod file {
    use super::*;
    use std::fmt::Write as _;

    fn path(url: &Url) -> Result<std::path::PathBuf> {
        url.to_file_path().map_err(|()| anyhow::anyhow!("not a local file: {url}"))
    }

    pub fn read(url: &Url) -> Result<Vec<u8>> {
        let path = path(url)?;
        let meta = std::fs::metadata(&path).with_context(|| format!("can't open {}", path.display()))?;
        if meta.len() > MAX_RESOURCE_BYTES {
            bail!("{} is too big", path.display());
        }
        std::fs::read(&path).with_context(|| format!("can't read {}", path.display()))
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    pub fn fetch(url: &Url) -> Result<Response> {
        let path = path(url)?;
        let html = |url: Url, body: String| Response { url, status: 200, content_type: Some("text/html".into()), body, auth_realm: None };
        if path.is_dir() {
            // Relative links in the listing need the trailing slash.
            let mut url = url.clone();
            if !url.path().ends_with('/') {
                url.set_path(&format!("{}/", url.path()));
            }
            return Ok(html(url, listing(&path)?));
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let image = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tif" | "tiff");
        if image {
            // An image on its own: a page with just the image on it, like everyone does.
            let name = escape(&path.file_name().unwrap_or_default().to_string_lossy());
            let body = format!(
                "<!doctype html><title>{name}</title><body style=\"margin:0;background:#0e0e0e;text-align:center\"><img src=\"{}\" style=\"max-width:100%\">",
                escape(url.as_str())
            );
            return Ok(html(url.clone(), body));
        }
        let bytes = read(url)?;
        let body = String::from_utf8_lossy(&bytes).into_owned();
        let content_type = match ext.as_str() {
            "html" | "htm" | "xhtml" => "text/html",
            _ => "text/plain",
        };
        Ok(Response { url: url.clone(), status: 200, content_type: Some(content_type.into()), body, auth_realm: None })
    }

    fn listing(dir: &std::path::Path) -> Result<String> {
        let mut entries: Vec<(bool, String)> = std::fs::read_dir(dir)
            .with_context(|| format!("can't list {}", dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| (e.path().is_dir(), e.file_name().to_string_lossy().into_owned()))
            .filter(|(_, name)| !name.starts_with('.'))
            .collect();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        let title = escape(&dir.display().to_string());
        let mut body = format!(
            "<!doctype html><title>{title}</title><style>body{{font-family:system-ui;margin:2em 3em}} \
             li{{list-style:none;padding:2px 0}} a{{text-decoration:none}} a:hover{{text-decoration:underline}} \
             .d a{{font-weight:600}}</style><h1>Index of {title}</h1><ul>"
        );
        if dir.parent().is_some() {
            body.push_str("<li class=d><a href=\"../\">⬆ Parent directory</a>");
        }
        for (is_dir, name) in entries {
            let href: String = url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>().replace('+', "%20");
            let (class, icon, slash) = if is_dir { ("d", "📁", "/") } else { ("f", "📄", "") };
            let _ = write!(body, "<li class={class}>{icon} <a href=\"{href}{slash}\">{}{slash}</a>", escape(&name));
        }
        body.push_str("</ul>");
        Ok(body)
    }
}

/// `data:[<mediatype>][;base64],<data>`
pub fn decode_data_url(url: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    let rest = url.strip_prefix("data:").context("not a data: URL")?;
    let (meta, data) = rest.split_once(',').context("malformed data: URL")?;
    let decoded: Vec<u8> = percent_encoding::percent_decode_str(data).collect();
    if meta.to_ascii_lowercase().ends_with(";base64") {
        let cleaned: Vec<u8> = decoded.into_iter().filter(|b| !b.is_ascii_whitespace()).collect();
        base64::engine::general_purpose::STANDARD
            .decode(&cleaned)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&cleaned))
            .context("bad base64 in data: URL")
    } else {
        Ok(decoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_gets_https() {
        assert_eq!(parse_input("example.com").unwrap().as_str(), "https://example.com/");
        assert_eq!(
            parse_input("  news.ycombinator.com/item?id=1 ").unwrap().as_str(),
            "https://news.ycombinator.com/item?id=1"
        );
    }

    #[test]
    fn data_urls() {
        assert_eq!(decode_data_url("data:text/plain,hi%20there").unwrap(), b"hi there");
        assert_eq!(decode_data_url("data:image/png;base64,aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_data_url("data:image/svg+xml,%3Csvg%3E").unwrap(), b"<svg>");
    }

    #[test]
    fn full_urls_pass_through() {
        assert_eq!(parse_input("http://example.com/a").unwrap().as_str(), "http://example.com/a");
    }

    #[test]
    fn rejects_other_schemes_and_empty() {
        assert!(parse_input("ftp://example.com").is_err());
        assert_eq!(parse_input("file:///etc/hosts").unwrap().as_str(), "file:///etc/hosts");
        assert_eq!(parse_input("/tmp/a b.html").unwrap().as_str(), "file:///tmp/a%20b.html");
        assert_eq!(parse_input("localhost:8080").unwrap().as_str(), "https://localhost:8080/");
        assert!(parse_input("   ").is_err());
    }
}
