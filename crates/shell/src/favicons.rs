//! Favicons remembered per site, for places that show sites you aren't on:
//! the start page's tiles and the omnibox's suggestions. Kept in memory and,
//! as the raw downloaded bytes, in the data folder.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{App, Global, Image};
use url::Url;

#[derive(Default)]
pub struct Favicons {
    /// `None`: in memory only.
    dir: Option<PathBuf>,
    /// host → icon (`None`: looked on disk, there isn't one).
    icons: HashMap<String, Option<Arc<Image>>>,
    reading: HashSet<String>,
}

impl Global for Favicons {}

fn key(url: &Url) -> Option<String> {
    matches!(url.scheme(), "http" | "https")
        .then(|| url.host_str())
        .flatten()
        .map(|h| h.trim_start_matches("www.").to_ascii_lowercase())
}

/// A file name for a host: hosts are already safe, but check.
fn file_name(host: &str) -> String {
    host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

impl Favicons {
    pub fn init(dir: Option<PathBuf>, cx: &mut App) {
        cx.set_global(Favicons { dir: dir.map(|d| d.join("favicons")), ..Default::default() });
    }

    /// A page on `page`'s site downloaded its icon.
    pub fn store(page: &Url, bytes: Vec<u8>, image: Arc<Image>, cx: &mut App) {
        let Some(host) = key(page) else { return };
        let this = cx.global_mut::<Favicons>();
        this.icons.insert(host.clone(), Some(image));
        let Some(dir) = this.dir.clone() else { return };
        cx.background_executor()
            .spawn(async move {
                std::fs::create_dir_all(&dir).ok();
                std::fs::write(dir.join(file_name(&host)), bytes).ok();
            })
            .detach();
    }

    /// `page`'s site's icon, if known. The first ask reads it from disk in
    /// the background and redraws when it's there.
    pub fn get(page: &Url, cx: &mut App) -> Option<Arc<Image>> {
        let host = key(page)?;
        let this = cx.global_mut::<Favicons>();
        if let Some(icon) = this.icons.get(&host) {
            return icon.clone();
        }
        let dir = this.dir.clone()?;
        if !this.reading.insert(host.clone()) {
            return None;
        }
        cx.spawn(async move |cx| {
            let path = dir.join(file_name(&host));
            let icon = cx
                .background_executor()
                .spawn(async move {
                    std::fs::read(path).ok().and_then(|b| render::images::prepare(b).ok()).map(|p| p.image)
                })
                .await;
            cx.update(|cx| {
                let this = cx.global_mut::<Favicons>();
                this.reading.remove(&host);
                this.icons.insert(host, icon);
                cx.refresh_windows();
            });
        })
        .detach();
        None
    }
}
