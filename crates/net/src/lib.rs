//! Networking: turning what the user typed into a URL, and fetching documents.
//!
//! Everything here is blocking; callers run it on a background executor.

use anyhow::{Context as _, Result, bail};
use ureq::ResponseExt as _;
use url::Url;

/// Largest document body we'll read, to keep a hostile page from eating memory.
const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

const USER_AGENT: &str = concat!("browser/", env!("CARGO_PKG_VERSION"));

/// A fetched document.
#[derive(Debug, Clone)]
pub struct Response {
    /// The URL after following redirects; relative links resolve against this.
    pub url: Url,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
}

/// Interpret URL-bar input: full URLs pass through, bare hosts get `https://`.
pub fn parse_input(input: &str) -> Result<Url> {
    let input = input.trim();
    if input.is_empty() {
        bail!("empty address");
    }
    match Url::parse(input) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => Ok(url),
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
    let mut response = agent()
        .get(url.as_str())
        .call()
        .with_context(|| format!("failed to fetch {url}"))?;

    let final_url = Url::parse(&response.get_uri().to_string()).unwrap_or_else(|_| url.clone());
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
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
    let mut response = agent()
        .get(url.as_str())
        .call()
        .with_context(|| format!("failed to fetch {url}"))?;
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
        assert!(parse_input("file:///etc/passwd").is_err());
        assert_eq!(parse_input("localhost:8080").unwrap().as_str(), "https://localhost:8080/");
        assert!(parse_input("   ").is_err());
    }
}
