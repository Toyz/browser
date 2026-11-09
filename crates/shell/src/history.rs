//! Everywhere you've been: visit counts for the omnibox's suggestions and the
//! new tab page. Saved as a TSV file, because of course.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::{App, Global};
use url::Url;

#[derive(Clone, Debug)]
pub struct Entry {
    pub url: Url,
    pub title: String,
    pub visits: u32,
    /// Unix seconds.
    pub last: u64,
}

#[derive(Default)]
pub struct History {
    entries: Vec<Entry>,
    /// `None`: don't persist (screenshot / bench runs).
    path: Option<PathBuf>,
}

impl Global for History {}

/// What the omnibox can offer for some typed text.
#[derive(Clone, Debug)]
pub struct Match {
    pub entry: Entry,
    /// The typed text is a prefix of the URL as displayed (inline-completable).
    pub prefix: bool,
}

const MAX_ENTRIES: usize = 5000;

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// "https://www.example.com/a" → "example.com/a"; "https://x.org/" → "x.org".
pub fn display_url(url: &Url) -> String {
    let s = url.as_str();
    let s = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://")).unwrap_or(s);
    let s = s.strip_prefix("www.").unwrap_or(s);
    s.strip_suffix('/').unwrap_or(s).to_owned()
}

/// Feather's data folder (history, cookies).
pub fn data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Feather")
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(xdg).join("feather")
    } else {
        home.join(".local/share/feather")
    })
}

impl History {
    /// Load saved history (or start empty). `persist: false` keeps it in memory.
    pub fn init(persist: bool, cx: &mut App) {
        let path = persist.then(|| data_dir().map(|d| d.join("history.tsv"))).flatten();
        let mut history = History { entries: Vec::new(), path };
        if let Some(text) = history.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            for line in text.lines() {
                let mut f = line.splitn(4, '\t');
                let (Some(visits), Some(last), Some(url), title) = (f.next(), f.next(), f.next(), f.next()) else {
                    continue;
                };
                if let (Ok(visits), Ok(last), Ok(url)) = (visits.parse(), last.parse(), Url::parse(url)) {
                    history.entries.push(Entry { url, title: title.unwrap_or_default().to_owned(), visits, last });
                }
            }
        }
        cx.set_global(history);
    }

    pub fn record(url: &Url, title: Option<&str>, cx: &mut App) {
        if !matches!(url.scheme(), "http" | "https") {
            return;
        }
        let mut url = url.clone();
        url.set_fragment(None);
        let history = cx.global_mut::<History>();
        let title = title.unwrap_or_default().replace(['\t', '\n', '\r'], " ").trim().to_owned();
        match history.entries.iter_mut().find(|e| e.url == url) {
            Some(e) => {
                e.visits += 1;
                e.last = now();
                if !title.is_empty() {
                    e.title = title;
                }
            }
            None => history.entries.push(Entry { url, title, visits: 1, last: now() }),
        }
        if history.entries.len() > MAX_ENTRIES {
            history.entries.sort_by_key(|e| std::cmp::Reverse(e.last));
            history.entries.truncate(MAX_ENTRIES);
        }
        let Some(path) = history.path.clone() else { return };
        let text = history.serialize();
        cx.background_executor()
            .spawn(async move {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).ok();
                }
                std::fs::write(path, text).ok();
            })
            .detach();
    }

    fn serialize(&self) -> String {
        self
            .entries
            .iter()
            .map(|e| format!("{}\t{}\t{}\t{}\n", e.visits, e.last, e.url, e.title))
            .collect()
    }

    /// History entries matching `query`, best first: URL-prefix matches, then
    /// URLs/titles containing every word; more visits and recency win ties.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Match> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let words: Vec<&str> = q.split_whitespace().collect();
        let mut matches: Vec<Match> = self
            .entries
            .iter()
            .filter_map(|e| {
                let shown = display_url(&e.url).to_lowercase();
                let prefix = shown.starts_with(&q) || e.url.as_str().to_lowercase().starts_with(&q);
                let haystack = format!("{shown} {}", e.title.to_lowercase());
                (prefix || words.iter().all(|w| haystack.contains(w))).then(|| Match { entry: e.clone(), prefix })
            })
            .collect();
        matches.sort_by(|a, b| {
            b.prefix
                .cmp(&a.prefix)
                .then(b.entry.visits.cmp(&a.entry.visits))
                .then(b.entry.last.cmp(&a.entry.last))
        });
        matches.truncate(limit);
        matches
    }

    /// The most visited sites (one entry per host), for the new tab page.
    pub fn top_sites(&self, limit: usize) -> Vec<Entry> {
        let mut by_host: Vec<Entry> = Vec::new();
        let mut sorted = self.entries.clone();
        sorted.sort_by(|a, b| b.visits.cmp(&a.visits).then(b.last.cmp(&a.last)));
        for e in sorted {
            let host = e.url.host_str().unwrap_or_default();
            match by_host.iter_mut().find(|t| t.url.host_str() == Some(host)) {
                Some(t) => t.visits += e.visits,
                None => by_host.push(e),
            }
        }
        by_host.sort_by_key(|e| std::cmp::Reverse(e.visits));
        by_host.truncate(limit);
        by_host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(url: &str, title: &str, visits: u32) -> Entry {
        Entry { url: Url::parse(url).unwrap(), title: title.into(), visits, last: 0 }
    }

    #[test]
    fn displays_urls_like_chrome() {
        assert_eq!(display_url(&Url::parse("https://www.example.com/").unwrap()), "example.com");
        assert_eq!(display_url(&Url::parse("http://news.ycombinator.com/item?id=1").unwrap()), "news.ycombinator.com/item?id=1");
    }

    #[test]
    fn prefix_matches_first() {
        let h = History {
            entries: vec![
                entry("https://en.wikipedia.org/wiki/Rust", "Rust - Wikipedia", 1),
                entry("https://www.rust-lang.org/", "Rust Programming Language", 5),
                entry("https://news.ycombinator.com/", "Hacker News", 9),
            ],
            path: None,
        };
        let m = h.search("rust", 5);
        assert_eq!(m.len(), 2);
        assert!(m[0].prefix && m[0].entry.url.as_str() == "https://www.rust-lang.org/");
        assert!(!m[1].prefix);
        assert_eq!(h.search("hacker news", 5).len(), 1);
        assert_eq!(h.top_sites(2)[0].url.host_str(), Some("news.ycombinator.com"));
    }
}
