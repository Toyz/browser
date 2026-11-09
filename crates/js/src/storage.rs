//! `localStorage` (per origin, saved in Feather's data folder, shared by
//! every tab) and `sessionStorage` (per tab and origin, in memory, for as
//! long as the browser runs).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

type Area = BTreeMap<String, String>;

static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
static LOCAL: OnceLock<Mutex<HashMap<String, Area>>> = OnceLock::new();
static SESSION: OnceLock<Mutex<HashMap<(u64, String), Area>>> = OnceLock::new();

/// Where `localStorage` lives (`None`: memory only, e.g. screenshot runs).
pub fn init(dir: Option<PathBuf>) {
    DIR.set(dir.map(|d| d.join("storage"))).ok();
}

fn file(origin: &str) -> Option<PathBuf> {
    let dir = DIR.get()?.as_ref()?;
    let name: String = origin.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    Some(dir.join(format!("{name}.tsv")))
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

fn load(origin: &str) -> Area {
    let Some(text) = file(origin).and_then(|f| std::fs::read_to_string(f).ok()) else { return Area::new() };
    text.lines().filter_map(|l| l.split_once('\t')).map(|(k, v)| (unescape(k), unescape(v))).collect()
}

fn save(origin: &str, area: &Area) {
    let Some(path) = file(origin) else { return };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let text: String = area.iter().map(|(k, v)| format!("{}\t{}\n", escape(k), escape(v))).collect();
    std::fs::write(path, text).ok();
}

/// Which storage: `local`, or `session` for a tab.
#[derive(Clone, Copy)]
pub enum Kind {
    Local,
    Session(u64),
}

/// Read or change an origin's storage area.
pub fn with<R>(kind: Kind, origin: &str, f: impl FnOnce(&mut Area) -> (R, bool)) -> R {
    match kind {
        Kind::Local => {
            let mut all = LOCAL.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
            let area = all.entry(origin.to_owned()).or_insert_with(|| load(origin));
            let (out, changed) = f(area);
            if changed {
                save(origin, area);
            }
            out
        }
        Kind::Session(tab) => {
            let mut all = SESSION.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
            f(all.entry((tab, origin.to_owned())).or_default()).0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_round_trips() {
        let s = "a\tb\nc\\d";
        assert_eq!(unescape(&escape(s)), s);
    }
}
