//! `fetch` / `XMLHttpRequest` from a page: the Fetch standard's CORS rules
//! around [`net::request`].

use net::{HttpRequest, HttpResponse};
use url::Url;

/// What a script asked for.
#[derive(Debug, Clone)]
pub struct ScriptRequest {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// `cors` (the default), `no-cors`, `same-origin`, `navigate`.
    pub mode: String,
    /// `same-origin` (the default), `include`, `omit`.
    pub credentials: String,
}

/// What the script gets.
#[derive(Debug, Clone)]
pub struct ScriptResponse {
    /// `basic`, `cors` or `opaque`.
    pub kind: &'static str,
    pub url: Url,
    pub status: u16,
    pub status_text: String,
    /// Only the ones the script may read.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub redirected: bool,
}

/// Headers a script may never set.
const FORBIDDEN: &[&str] = &[
    "accept-charset", "accept-encoding", "access-control-request-headers", "access-control-request-method", "connection",
    "content-length", "cookie", "cookie2", "date", "dnt", "expect", "host", "keep-alive", "origin", "referer", "te",
    "trailer", "transfer-encoding", "upgrade", "via",
];

/// Response headers every script can read.
const SAFELISTED_RESPONSE: &[&str] = &["cache-control", "content-language", "content-length", "content-type", "expires", "last-modified", "pragma"];

fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// A CORS-safelisted request header (needs no preflight).
fn safelisted(name: &str, value: &str) -> bool {
    match name {
        "accept" | "accept-language" | "content-language" => value.len() <= 128,
        // EventSource's reconnects.
        "last-event-id" => value.len() <= 128,
        "content-type" => {
            let essence = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            matches!(essence.as_str(), "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain")
        }
        _ => false,
    }
}

/// Run a script's request, whole. `Err` is a network error (`TypeError:
/// Failed to fetch`), with the reason for the console.
pub fn fetch(page: &Url, req: ScriptRequest) -> Result<ScriptResponse, String> {
    let mut head = None;
    let mut body = Vec::new();
    fetch_streaming(page, req, |h| head = Some(h), |chunk| {
        body.extend_from_slice(chunk);
        true
    })?;
    let mut response = head.ok_or("no response")?;
    response.body = body;
    Ok(response)
}

/// The same, streamed: `on_head` gets the response (empty body) once CORS
/// allows it, then `on_chunk` the body as it arrives.
pub fn fetch_streaming(
    page: &Url,
    req: ScriptRequest,
    mut on_head: impl FnMut(ScriptResponse),
    mut on_chunk: impl FnMut(&[u8]) -> bool,
) -> Result<(), String> {
    let headers: Vec<(String, String)> = req
        .headers
        .into_iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v))
        .filter(|(k, _)| !FORBIDDEN.contains(&k.as_str()) && !k.starts_with("proxy-") && !k.starts_with("sec-"))
        .collect();
    let method = req.method.to_ascii_uppercase();
    let origin = origin_of(page);
    // Opaque origins (file:, data:) are never same-origin.
    let same_origin = page.origin().is_tuple() && origin_of(&req.url) == origin;
    let data = req.url.scheme() == "data";
    if req.url.scheme() == "file" && page.scheme() != "file" {
        return Err(format!("{} can't load local files", page));
    }

    if same_origin || data || page.scheme() == "file" {
        let cookies = req.credentials != "omit";
        let requested = req.url.clone();
        net::request_streaming(
            &HttpRequest { method, url: req.url.clone(), headers, body: req.body, cookies },
            |head| {
                on_head(basic(head.clone(), &requested, "basic"));
                true
            },
            &mut on_chunk,
        )
        .map_err(|e| format!("{e:#}"))?;
        return Ok(());
    }
    match req.mode.as_str() {
        "same-origin" => return Err(format!("{} is cross-origin (mode: same-origin)", req.url)),
        "no-cors" => {
            // Sent, but the script learns nothing.
            if !matches!(method.as_str(), "GET" | "HEAD" | "POST") {
                return Err(format!("{method} isn't allowed in no-cors mode"));
            }
            let cookies = req.credentials == "include";
            net::request(&HttpRequest { method, url: req.url.clone(), headers, body: req.body, cookies }).map_err(|e| format!("{e:#}"))?;
            on_head(ScriptResponse {
                kind: "opaque",
                url: req.url,
                status: 0,
                status_text: String::new(),
                headers: Vec::new(),
                body: Vec::new(),
                redirected: false,
            });
            return Ok(());
        }
        _ => {}
    }

    // CORS.
    let credentials = req.credentials == "include";
    let unsafe_headers: Vec<String> = {
        let mut v: Vec<String> = headers.iter().filter(|(k, v)| !safelisted(k, v)).map(|(k, _)| k.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    let simple = matches!(method.as_str(), "GET" | "HEAD" | "POST") && unsafe_headers.is_empty();
    if !simple {
        let mut pre = vec![
            ("origin".to_owned(), origin.clone()),
            ("access-control-request-method".to_owned(), method.clone()),
        ];
        if !unsafe_headers.is_empty() {
            pre.push(("access-control-request-headers".to_owned(), unsafe_headers.join(",")));
        }
        let preflight = net::request(&HttpRequest { method: "OPTIONS".into(), url: req.url.clone(), headers: pre, body: None, cookies: false })
            .map_err(|e| format!("CORS preflight failed: {e:#}"))?;
        if !(200..300).contains(&preflight.status) {
            return Err(format!("CORS preflight for {} answered {}", req.url, preflight.status));
        }
        allowed_origin(&preflight, &origin, credentials)?;
        let list = |name: &str| -> Vec<String> {
            preflight.header(name).unwrap_or("").split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect()
        };
        let methods = list("access-control-allow-methods");
        let method_ok = matches!(method.as_str(), "GET" | "HEAD" | "POST")
            || methods.iter().any(|m| m.eq_ignore_ascii_case(&method))
            || (!credentials && methods.iter().any(|m| m == "*"));
        if !method_ok {
            return Err(format!("CORS: {} doesn't allow {method}", req.url));
        }
        let allowed = list("access-control-allow-headers");
        for h in &unsafe_headers {
            let ok = allowed.contains(h) || (!credentials && allowed.iter().any(|a| a == "*") && h != "authorization");
            if !ok {
                return Err(format!("CORS: {} doesn't allow the {h} header", req.url));
            }
        }
    }
    let mut actual = headers;
    actual.push(("origin".into(), origin.clone()));
    let mut refused = None;
    let requested = req.url.clone();
    net::request_streaming(
        &HttpRequest { method, url: req.url.clone(), headers: actual, body: req.body, cookies: credentials },
        |response| {
            // Nothing of a response the page may not read reaches it.
            if let Err(e) = allowed_origin(response, &origin, credentials) {
                refused = Some(e);
                return false;
            }
            let exposed: Vec<String> = response
                .header("access-control-expose-headers")
                .unwrap_or("")
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            let all = !credentials && exposed.iter().any(|e| e == "*");
            let mut out = basic(response.clone(), &requested, "cors");
            out.headers.retain(|(k, _)| SAFELISTED_RESPONSE.contains(&k.as_str()) || all || exposed.contains(k));
            on_head(out);
            true
        },
        &mut on_chunk,
    )
    .map_err(|e| format!("{e:#}"))?;
    match refused {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The response's `Access-Control-Allow-Origin` lets `origin` read it.
fn allowed_origin(response: &HttpResponse, origin: &str, credentials: bool) -> Result<(), String> {
    let allow = response.header("access-control-allow-origin").map(str::trim);
    let ok = match allow {
        Some("*") => !credentials,
        Some(o) => o == origin,
        None => false,
    };
    if !ok {
        return Err(format!(
            "CORS: {} isn't allowed to read {} (Access-Control-Allow-Origin: {})",
            origin,
            response.url,
            allow.unwrap_or("missing")
        ));
    }
    if credentials && response.header("access-control-allow-credentials").map(str::trim) != Some("true") {
        return Err(format!("CORS: {} doesn't allow credentials", response.url));
    }
    Ok(())
}

fn basic(r: HttpResponse, requested: &Url, kind: &'static str) -> ScriptResponse {
    let redirected = r.url != *requested;
    ScriptResponse {
        kind,
        url: r.url,
        status: r.status,
        status_text: r.status_text,
        // Never a script's business.
        headers: r.headers.into_iter().filter(|(k, _)| k != "set-cookie" && k != "set-cookie2").collect(),
        body: r.body,
        redirected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safelisting() {
        assert!(safelisted("content-type", "text/plain; charset=utf-8"));
        assert!(!safelisted("content-type", "application/json"));
        assert!(!safelisted("x-requested-with", "XMLHttpRequest"));
    }

    #[test]
    fn origin_checks() {
        let r = |allow: Option<&str>, creds: Option<&str>| HttpResponse {
            url: Url::parse("https://api.example.com/x").unwrap(),
            status: 200,
            status_text: "OK".into(),
            headers: allow
                .map(|a| ("access-control-allow-origin".to_owned(), a.to_owned()))
                .into_iter()
                .chain(creds.map(|c| ("access-control-allow-credentials".to_owned(), c.to_owned())))
                .collect(),
            body: Vec::new(),
        };
        let me = "https://app.example.com";
        assert!(allowed_origin(&r(Some("*"), None), me, false).is_ok());
        assert!(allowed_origin(&r(Some("*"), None), me, true).is_err(), "no wildcard with credentials");
        assert!(allowed_origin(&r(Some(me), Some("true")), me, true).is_ok());
        assert!(allowed_origin(&r(Some(me), None), me, true).is_err());
        assert!(allowed_origin(&r(Some("https://evil.com"), None), me, false).is_err());
        assert!(allowed_origin(&r(None, None), me, false).is_err());
    }
}
