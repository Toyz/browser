//! Command-line options, and offscreen screenshots for development.
//!
//! `feather [URL...] [--dark|--light] [--screenshot out.png] [--pages N] [--scroll PX] [--row N] [--size WxH] [--resize WxH] [--mouse X,Y] [--bench FRAMES]`
//!
//! With `--screenshot`, the browser loads the page, renders frames to PNG
//! (scrolling a viewport between pages) and quits. Rendering goes through
//! GPUI's `render_to_image`, so nothing is read from the screen: it needs no
//! screen-recording permission. Requires `--features screenshot`.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Args {
    /// Pages to open, one tab each.
    pub urls: Vec<String>,
    pub width: f32,
    pub height: f32,
    pub capture: Option<CaptureJob>,
    /// `--dark` / `--light`: override the system appearance.
    pub dark: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct CaptureJob {
    pub path: PathBuf,
    /// How many viewport-sized screenshots to take, scrolling between them.
    pub pages: usize,
    /// Initial scroll offset, px.
    pub scroll: f32,
    /// Start at this top-level row (deterministic, unlike pixel scrolling
    /// across rows the list hasn't measured yet).
    pub row: Option<usize>,
    /// Resize the window to this size after loading (exercises live restyling).
    pub resize: Option<(f32, f32)>,
    /// Move the mouse through these points (window coordinates) before
    /// capturing (`:hover`); `X,Y;X,Y;...`.
    pub mouse: Vec<(f32, f32)>,
    /// Scroll-and-draw this many frames, timing each, and print stats.
    pub bench: Option<usize>,
    /// A two-finger sideways swipe after the click: (px, released?).
    pub swipe: Option<(f32, bool)>,
    /// Real trackpad scrolling: a gesture of this many px down (negative:
    /// up), then momentum events, drawing a frame after each.
    pub wheel: Option<f32>,
    /// Keystrokes to send after the click (`enter` ends with Enter).
    pub keys: Option<String>,
    /// Extra time to wait before capturing, ms (scripted pages).
    pub wait: u64,
    /// Click here (window coordinates) before capturing, then let it settle.
    pub click: Option<(f32, f32)>,
    /// Type this into the omnibox before capturing (suggestions, autocomplete).
    pub typing: Option<String>,
}

impl CaptureJob {
    /// `out.png` for one page; `out-1.png`, `out-2.png`... for several.
    pub fn path_for(&self, page: usize) -> PathBuf {
        if self.pages <= 1 {
            return self.path.clone();
        }
        let stem = self.path.file_stem().unwrap_or_default().to_string_lossy();
        let ext = self.path.extension().map(|e| e.to_string_lossy()).unwrap_or("png".into());
        self.path.with_file_name(format!("{stem}-{}.{ext}", page + 1))
    }
}

impl Args {
    pub fn parse() -> Self {
        let mut args = Args { urls: Vec::new(), width: 1100.0, height: 800.0, capture: None, dark: None };
        let mut screenshot = None;
        let mut pages = 1;
        let mut scroll = 0.0;
        let mut row = None;
        let mut resize = None;
        let mut mouse = Vec::new();
        let mut bench = None;
        let mut typing = None;
        let mut click = None;
        let mut keys = None;
        let mut swipe = None;
        let mut wheel = None;
        let mut wait = 0;
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--dark" => args.dark = Some(true),
                "--light" => args.dark = Some(false),
                "--screenshot" => screenshot = it.next().map(PathBuf::from),
                "--pages" => pages = it.next().and_then(|n| n.parse().ok()).unwrap_or(1).max(1),
                "--scroll" => scroll = it.next().and_then(|n| n.parse().ok()).unwrap_or(0.0),
                "--row" => row = it.next().and_then(|n| n.parse().ok()),
                "--type" => typing = it.next(),
                "--keys" => keys = it.next(),
                // `--swipe 160` (held) or `--swipe 160,end` (released).
                "--swipe" => {
                    swipe = it.next().and_then(|s| {
                        let (n, end) = s.split_once(',').map_or((s.as_str(), false), |(n, e)| (n, e == "end"));
                        Some((n.parse().ok()?, end))
                    })
                }
                "--wait" => wait = it.next().and_then(|n| n.parse().ok()).unwrap_or(0),
                "--wheel" => wheel = it.next().and_then(|n| n.parse().ok()),
                "--click" => {
                    click = it.next().as_deref().and_then(|s| s.split_once(',')).and_then(|(x, y)| {
                        Some((x.parse().ok()?, y.parse().ok()?))
                    })
                }
                "--bench" => bench = it.next().and_then(|n| n.parse().ok()).or(Some(120)),
                "--mouse" => {
                    mouse = it
                        .next()
                        .unwrap_or_default()
                        .split(';')
                        .filter_map(|p| p.split_once(','))
                        .filter_map(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))
                        .collect()
                }
                "--resize" => {
                    resize = it.next().as_deref().and_then(|s| s.split_once('x')).and_then(|(w, h)| {
                        Some((w.parse().ok()?, h.parse().ok()?))
                    })
                }
                "--size" => {
                    if let Some((w, h)) = it.next().as_deref().and_then(|s| s.split_once('x')) {
                        args.width = w.parse().unwrap_or(args.width);
                        args.height = h.parse().unwrap_or(args.height);
                    }
                }
                _ if !arg.starts_with("--") => args.urls.push(arg),
                other => eprintln!("ignoring unknown argument {other:?}"),
            }
        }
        args.capture = screenshot.map(|path| CaptureJob { path, pages, scroll, row, resize, mouse, bench, typing, click, keys, swipe, wheel, wait });
        if args.capture.is_some() && !cfg!(feature = "screenshot") {
            eprintln!("--screenshot needs a build with `--features screenshot`; ignoring");
            args.capture = None;
        }
        args
    }
}

/// Render the window's current frame to `path` (at 1x scale).
#[cfg(feature = "screenshot")]
pub fn save_frame(window: &gpui_kit::Window, path: &std::path::Path) -> anyhow::Result<()> {
    let image = window.render_to_image()?;
    let scale = window.scale_factor();
    let image = if scale > 1.0 {
        let (w, h) = (image.width() as f32 / scale, image.height() as f32 / scale);
        image::imageops::resize(&image, w as u32, h as u32, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    image.save(path)?;
    Ok(())
}

#[cfg(not(feature = "screenshot"))]
pub fn save_frame(_window: &gpui_kit::Window, _path: &std::path::Path) -> anyhow::Result<()> {
    anyhow::bail!("built without the `screenshot` feature")
}
