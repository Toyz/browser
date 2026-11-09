//! One tab: its history, the page on screen, and everything that keeps the
//! page interactive (images, hover restyles, prerendering links).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt as _;
use futures::StreamExt as _;
use futures::future::Shared;
use futures::stream::FuturesUnordered;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::scroll::ScrollableElement as _;
use page::NodeId;
use render::{ImageState, ImageStore, Palette, RenderCx};
use style::ElementState;
use url::Url;

mod animations;
mod forms;
mod scripts;

use crate::capture::{self, CaptureJob};
use crate::history::{self, History};
use crate::loader::{self, LoadedPage, same_document};

/// Concurrent image downloads per page.
const IMAGE_CONCURRENCY: usize = 8;
/// Don't fetch more than this many images for one page (lazy loading comes later).
const MAX_IMAGES: usize = 300;
/// How far a swipe must go (fraction of the width) to navigate, unless
/// it's flicked; and how long the page takes to slide the rest of the way.
const SWIPE_GO: f32 = 0.3;
const SWIPE_FLICK: f32 = 18.0;
const SWIPE_SETTLE: Duration = Duration::from_millis(220);
/// How long the pointer rests on a link before we start loading it.
const PRERENDER_DWELL: Duration = Duration::from_millis(60);

type PageResult = Result<LoadedPage, Arc<anyhow::Error>>;

/// The sign-in sheet for HTTP Basic authentication.
struct AuthPrompt {
    realm: String,
    user: Entity<gpui_kit::component::input::InputState>,
    password: Entity<gpui_kit::component::input::InputState>,
    _subscriptions: Vec<Subscription>,
}

/// A link loaded in the background because the pointer is resting on it:
/// by the time it's clicked it's usually fetched, parsed, styled and laid out.
struct Prerender {
    url: Url,
    page: Shared<Task<PageResult>>,
}

pub enum TabEvent {
    /// Cmd-click: open this in a new background tab.
    OpenInNewTab(Url),
    /// Screenshot mode: type this into the omnibox.
    TypeInOmnibox(String),
}

pub struct Tab {
    focus_handle: FocusHandle,
    palette: Palette,
    history: Vec<Url>,
    /// Which document made each history entry: `pushState` entries share
    /// their page's, and moving between those doesn't load anything.
    history_doc: Vec<u64>,
    next_doc: u64,
    history_ix: usize,
    /// Pages we navigated away from, kept so back/forward is instant
    /// (history index → page and where it was scrolled to).
    page_cache: HashMap<usize, (LoadedPage, ListOffset)>,
    /// History index of the page on screen (it stays up while the next loads).
    page_ix: usize,
    page: Option<LoadedPage>,
    error: Option<String>,
    list_state: ListState,
    /// Rows nobody has measured yet need a height guess, or GPUI counts them
    /// as 0px and the scrollbar shrinks and jumps as they scroll into view.
    /// Set by `reset_rows`, applied after the next frame measures some.
    row_hint_pending: bool,
    hovered_link: Option<Url>,
    loading: Option<Task<()>>,
    prerender: Option<Prerender>,
    _hover_timer: Option<Task<()>>,
    images: ImageStore,
    images_pending: usize,
    /// Bumped per finished image (invalidates cached inline layouts).
    images_generation: u64,
    /// Bumped when web fonts arrive.
    fonts_generation: u64,
    /// Web font fetches for this page, and the font files they asked for
    /// (scripts adding styles can ask for more later).
    _fonts_tasks: Vec<Task<()>>,
    fonts_requested: HashSet<String>,
    _image_task: Option<Task<()>>,
    favicon: Option<Arc<Image>>,
    _favicon_task: Option<Task<()>>,
    /// Following the page's script thread (DOM updates, navigations).
    _scripts_task: Option<Task<()>>,
    /// Re-reading stylesheets scripts changed.
    _script_styles: Option<Task<()>>,
    /// A link click already went to the scripts (mouse up shouldn't again).
    script_clicked: bool,
    _refresh: Option<Task<()>>,
    _restyle: Option<Task<()>>,
    /// Elements whose blocks report the pointer inside them.
    hover_blocks: HashSet<NodeId>,
    /// The inline element under the pointer, per paragraph.
    hover_inline: Option<(usize, NodeId)>,
    mouse_down: bool,
    /// A two-finger horizontal swipe in progress (px the page has moved;
    /// positive: back).
    swipe: f32,
    /// Fingers on the trackpad. macOS keeps sending (momentum) scroll events
    /// after they lift, with no end; only a live gesture moves the page.
    swipe_active: bool,
    /// The last event's horizontal movement, for flicks.
    swipe_velocity: f32,
    /// Let go: the page slides from, to, starting when.
    swipe_release: Option<(f32, f32, std::time::Instant)>,
    /// The page a swipe uncovers: its history index and its own list,
    /// at the scroll position it was left at.
    swipe_peek: Option<(usize, ListState)>,
    _swipe_task: Option<Task<()>>,
    /// Ends a swipe whose end never arrives (macOS reports a cancelled
    /// gesture as just another move).
    _swipe_watchdog: Option<Task<()>>,
    /// Hover restyles wait until scrolling stops (like Chrome): otherwise
    /// every element passing under the pointer restyles the page.
    scrolling_until: Option<std::time::Instant>,
    _scroll_end: Option<Task<()>>,
    /// Bumped per page shown, so hover reports from the previous page's last
    /// frame (with that document's node ids) are ignored.
    page_gen: u64,
    /// The code editor showing a source page (CSS/JS/JSON, `view-source:`),
    /// and whether it shows the pretty-printed version.
    source_editor: Option<Entity<gpui_kit::component::input::EditorState>>,
    source_pretty: bool,
    _source_redraw: Option<Subscription>,
    /// A site asking for HTTP Basic credentials.
    auth_prompt: Option<AuthPrompt>,
    /// Running CSS transitions and animations.
    animations: animations::Animations,
    /// Form controls' widgets and state.
    forms: forms::Forms,
    /// A form `POST` waiting for its navigation to start loading.
    pending_post: Option<(Url, String)>,
    /// A styler kept for patching hover changes (keyed by media signature).
    hover_styler: Option<(Vec<bool>, style::Styler)>,
    /// The interaction state the current layout was styled with.
    element_state: Arc<ElementState>,
    /// Screenshot job from the command line, run once the first page loads.
    pub capture: Option<CaptureJob>,
    _capture_task: Option<Task<()>>,
}

impl EventEmitter<TabEvent> for Tab {}

/// The media environment for the page area of `window`.
pub fn media_env(window: &Window) -> style::MediaEnv {
    let viewport = window.viewport_size();
    style::MediaEnv {
        width: f32::from(viewport.width),
        height: f32::from(viewport.height) - crate::CHROME_HEIGHT,
        dark: crate::is_dark(window),
        ..Default::default()
    }
}

impl Tab {
    pub fn new(palette: Palette, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            palette,
            history: Vec::new(),
            history_doc: Vec::new(),
            next_doc: 0,
            history_ix: 0,
            page_cache: HashMap::new(),
            page_ix: 0,
            page: None,
            error: None,
            // Measure well beyond the viewport so fast scrolling stays smooth.
            list_state: ListState::new(0, ListAlignment::Top, px(2000.)),
            row_hint_pending: false,
            hovered_link: None,
            loading: None,
            prerender: None,
            _hover_timer: None,
            images: ImageStore::default(),
            images_pending: 0,
            images_generation: 0,
            fonts_generation: 0,
            _fonts_tasks: Vec::new(),
            fonts_requested: HashSet::new(),
            _image_task: None,
            favicon: None,
            _favicon_task: None,
            _scripts_task: None,
            _script_styles: None,
            script_clicked: false,
            _refresh: None,
            _restyle: None,
            hover_blocks: HashSet::new(),
            hover_inline: None,
            mouse_down: false,
            scrolling_until: None,
            swipe: 0.0,
            swipe_active: false,
            swipe_velocity: 0.0,
            swipe_release: None,
            swipe_peek: None,
            _swipe_task: None,
            _swipe_watchdog: None,
            _scroll_end: None,
            page_gen: 0,
            hover_styler: None,
            forms: Default::default(),
            animations: Default::default(),
            auth_prompt: None,
            source_editor: None,
            source_pretty: false,
            _source_redraw: None,
            pending_post: None,
            element_state: Arc::default(),
            capture: None,
            _capture_task: None,
        }
    }

    // What the window chrome shows.

    pub fn title(&self) -> SharedString {
        match &self.page {
            Some(p) => p.title.clone().unwrap_or_else(|| history::display_url(&p.url)).into(),
            None if self.loading.is_some() => "Loading…".into(),
            None => "New Tab".into(),
        }
    }

    /// `view-source:` for the page on screen.
    pub fn source_url(&self) -> Option<Url> {
        let url = self.page.as_ref()?.url.clone();
        if url.scheme() == "view-source" {
            return None;
        }
        Url::parse(&format!("view-source:{url}")).ok()
    }

    pub fn url(&self) -> Option<Url> {
        self.history.get(self.history_ix).cloned()
    }

    pub fn is_loading(&self) -> bool {
        self.loading.is_some()
    }

    pub fn can_go_back(&self) -> bool {
        self.history_ix > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.history_ix + 1 < self.history.len()
    }

    pub fn favicon(&self) -> Option<Arc<Image>> {
        self.favicon.clone()
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus_handle.focus(window, cx);
    }

    // Navigation.

    /// Go somewhere new: push onto history (dropping any forward entries).
    pub fn navigate(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
        if !self.history.is_empty() {
            self.history.truncate(self.history_ix + 1);
            self.history_doc.truncate(self.history_ix + 1);
            self.page_cache.retain(|&ix, _| ix <= self.history_ix);
        }
        self.history.push(url.clone());
        self.next_doc += 1;
        self.history_doc.push(self.next_doc);
        self.history_ix = self.history.len() - 1;
        self.show(url, false, window, cx);
    }

    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_go_back() {
            self.go_to_history(self.history_ix - 1, window, cx);
        }
    }

    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_go_forward() {
            self.go_to_history(self.history_ix + 1, window, cx);
        }
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self.url() {
            self.show(url, true, window, cx);
        }
    }

    /// Back/forward: restore the page from memory if we still have it.
    fn go_to_history(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.history[ix].clone();
        // An entry the page made with pushState: its scripts move there.
        let same_doc = self.history_doc.get(ix).is_some_and(|d| Some(d) == self.history_doc.get(self.page_ix));
        if same_doc && let Some(host) = self.page.as_ref().and_then(|p| p.scripts.clone()) {
            let delta = ix as i32 - self.history_ix as i32;
            self.history_ix = ix;
            self.page_ix = ix;
            if let Some(page) = &mut self.page {
                page.url = url;
            }
            host.send(js::ToJs::Traverse { delta });
            cx.notify();
            return;
        }
        let same_page = self.page.as_ref().is_some_and(|p| same_document(&p.url, &url));
        self.history_ix = ix;
        if same_page {
            self.show(url, false, window, cx);
            return;
        }
        let Some((page, scroll)) = self.page_cache.remove(&ix) else {
            self.show(url, false, window, cx);
            return;
        };
        self.stash_page();
        self.page_ix = ix;
        self.loading = None;
        self.error = None;
        self.reset_rows(page.layout.rows.len());
        self.list_state.scroll_to(scroll);
        self.install(page, window, cx);
        // The window may have been resized since.
        self.viewport_changed(window, cx);
        cx.notify();
    }

    /// Keep the current page (and scroll position) for back/forward, and
    /// forget pages too far away in history.
    fn stash_page(&mut self) {
        const KEEP: usize = 8;
        if let Some(page) = self.page.take() {
            let scroll = self.list_state.logical_scroll_top();
            self.page_cache.insert(self.page_ix, (page, scroll));
        }
        let here = self.history_ix;
        self.page_cache.retain(|&ix, _| ix.abs_diff(here) <= KEEP);
    }

    /// Display `url`: scroll within the current page if only the fragment differs,
    /// otherwise load it (or take it from a finished prerender). Doesn't touch history.
    fn show(&mut self, url: Url, force_reload: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.focus_handle.focus(window, cx);

        if !force_reload
            && let Some(page) = &mut self.page
            && same_document(&page.url, &url)
        {
            self.loading = None;
            page.url = url.clone();
            self.page_ix = self.history_ix;
            self.scroll_to_fragment(url.fragment());
            // `:target` moved.
            self.interaction_changed(window, cx);
            cx.notify();
            return;
        }

        let prerendered = match self.prerender.take() {
            Some(p) if !force_reload && same_document(&p.url, &url) => Some(p.page),
            _ => None,
        };
        let pending = match prerendered {
            Some(page) => {
                if std::env::var_os("BROWSER_DEBUG").is_some() {
                    eprintln!("prerender hit: {url}");
                }
                page
            }
            None => self.spawn_load(url.clone(), window, cx),
        };
        cx.notify();
        self.loading = Some(cx.spawn_in(window, async move |this, cx| {
            let result = pending.await;
            this.update_in(cx, |this, window, cx| {
                this.loading = None;
                match result {
                    Ok(mut page) => {
                        // Keep the fragment across redirects so #anchors still work.
                        if url.fragment().is_some() {
                            page.url.set_fragment(url.fragment());
                        }
                        // Redirects may have moved us; record where we actually are.
                        this.history[this.history_ix] = page.url.clone();
                        History::record(&page.url, page.title.as_deref(), cx);
                        this.reset_rows(page.layout.rows.len());
                        let fragment = page.url.fragment().map(str::to_owned);
                        // Keep the page we're leaving for back/forward (not on reload).
                        if this.page_ix != this.history_ix {
                            this.stash_page();
                        }
                        this.page_ix = this.history_ix;
                        this.install(page, window, cx);
                        this.scroll_to_fragment(fragment.as_deref());
                        // A prerender was styled for the window and pointer as they were.
                        this.viewport_changed(window, cx);
                        this.interaction_changed(window, cx);
                    }
                    Err(err) => this.error = Some(format!("{err:#}")),
                }
                if let Some(job) = this.capture.take() {
                    this.start_capture(job, window, cx);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn spawn_load(&mut self, url: Url, window: &Window, cx: &mut Context<Self>) -> Shared<Task<PageResult>> {
        // Media queries see the page area's size.
        let env = media_env(window);
        let post = match self.pending_post.take() {
            Some((target, body)) if target == url => Some(body),
            other => {
                self.pending_post = other;
                None
            }
        };
        // The page we're leaving is the referrer.
        let referer = self.page.as_ref().map(|p| p.url.clone());
        // sessionStorage belongs to the tab.
        let session = cx.entity_id().as_u64();
        let page = loader::deep(move || loader::load(&url, env, post.as_deref(), referer.as_ref(), session).map_err(Arc::new));
        cx.background_spawn(page).shared()
    }

    /// Put a loaded page on screen.
    fn install(&mut self, page: LoadedPage, window: &mut Window, cx: &mut Context<Self>) {
        if std::env::var_os("BROWSER_DEBUG").is_some() {
            eprintln!("install {} ({} rows, title {:?})", page.url, page.layout.rows.len(), page.title);
        }
        let favicon = page.favicon.clone();
        let refresh = page.refresh.clone();
        self.auth_prompt = page.auth_realm.clone().map(|realm| self.new_auth_prompt(realm, window, cx));
        // Source code goes in a (read-only) code editor, highlighted.
        // Minified code opens un-minified.
        self.source_pretty = page.source.as_ref().is_some_and(|s| s.pretty.is_some());
        self.source_editor = page.source.clone().map(|source| {
            let text = source.pretty.clone().unwrap_or(source.raw.clone());
            let text = SharedString::from(text.to_string());
            cx.new(|cx| gpui_kit::component::input::EditorState::new(window, cx).language(source.language).default_value(text))
        });
        // Drawn inside this view: redraw when it changes (highlighting
        // finishing in the background, the caret).
        self._source_redraw = self.source_editor.as_ref().map(|e| cx.observe(e, |_, _, cx| cx.notify()));
        // Images the new page shares with the last one (logos, icons, sprites)
        // stay loaded: they appear in the first frame instead of popping in.
        {
            let wanted: HashSet<&Url> = page
                .layout
                .images
                .iter()
                .map(|(u, _)| &**u)
                .chain(page.layout.fixed_images.iter().map(|u| &**u))
                .collect();
            self.images
                .borrow_mut()
                .retain(|url, state| wanted.contains(url) && matches!(state, ImageState::Ready { .. }));
        }
        self.page = Some(page);
        self.page_gen += 1;
        // Work for the previous page must not land on this one.
        self._restyle = None;
        self._scroll_end = None;
        self.hover_styler = None;
        self.forms = Default::default();
        self.animations = Default::default();
        self.hover_blocks.clear();
        self.hover_inline = None;
        self.hovered_link = None;
        self.element_state = Arc::default();
        self.load_images(cx);
        self._fonts_tasks.clear();
        self.fonts_requested.clear();
        self.load_fonts(cx);
        let site = self.page.as_ref().map(|p| p.url.clone());
        self.load_favicon(site, favicon, cx);
        self.follow_scripts(window, cx);
        self.schedule_refresh(refresh, window, cx);
        self.sync_keyframes(window);
    }

    /// Follow a `<meta http-equiv="refresh">` redirect, replacing this
    /// history entry (so Back doesn't bounce you forward again).
    fn schedule_refresh(&mut self, refresh: Option<(f32, Url)>, window: &mut Window, cx: &mut Context<Self>) {
        self._refresh = None;
        let Some((secs, url)) = refresh else { return };
        if secs > 30.0 {
            return;
        }
        let page_gen = self.page_gen;
        self._refresh = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f32(secs.max(0.0))).await;
            this.update_in(cx, |this, window, cx| {
                if this.page_gen == page_gen {
                    this.redirect(url, window, cx);
                }
            })
            .ok();
        }));
    }

    /// Load `url` in place of the current history entry.
    fn redirect(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.history.get_mut(self.history_ix) {
            *entry = url.clone();
        }
        // The page being replaced isn't kept for Back.
        self.page_ix = self.history_ix;
        self.show(url, true, window, cx);
    }

    /// The page's scripts changed its address (`history.pushState`, or
    /// `replaceState` when `replace`): a history entry of the same document.
    pub(super) fn script_url_changed(&mut self, url: Url, replace: bool, cx: &mut Context<Self>) {
        if self.history.is_empty() {
            return;
        }
        let doc = self.history_doc.get(self.history_ix).copied().unwrap_or(0);
        if replace {
            self.history[self.history_ix] = url.clone();
        } else {
            self.history.truncate(self.history_ix + 1);
            self.history_doc.truncate(self.history_ix + 1);
            self.page_cache.retain(|&ix, _| ix <= self.history_ix);
            self.history.push(url.clone());
            self.history_doc.push(doc);
            self.history_ix += 1;
            self.page_ix = self.history_ix;
        }
        if let Some(page) = &mut self.page {
            page.url = url;
        }
        cx.notify();
    }

    /// Fetch the page's web fonts, register them, then re-lay out its text.
    pub(super) fn load_fonts(&mut self, cx: &mut Context<Self>) {
        if std::env::var_os("FEATHER_NO_WEBFONTS").is_some() {
            return;
        }
        let Some(page) = &self.page else { return };
        let Some(styles) = &page.styles else { return };
        let page_url = page.url.clone();
        let palette = self.palette.clone();
        let mut fetch: Vec<(String, Vec<String>, (u16, u16))> = Vec::new(); // url → CSS families, weights
        let mut known = false;
        // Only families the page actually asks for (a font CSS may declare dozens).
        let used = styles.used_font_families();
        let face_count = styles.font_faces().len();
        {
            let registry = cx.global::<crate::fonts::WebFonts>();
            for face in styles.font_faces() {
                let Some(src) = crate::fonts::pick_source(face) else { continue };
                if let Some(loaded) = registry.loaded.get(src) {
                    // Registered by an earlier page: just the alias.
                    if let Some(family) = loaded {
                        palette.fonts.add(&face.family, family);
                        known = true;
                    }
                    continue;
                }
                if !used.contains(&face.family.to_lowercase()) || self.fonts_requested.contains(src) {
                    continue;
                }
                if let Some(i) = fetch.iter().position(|(u, _, _)| u == src) {
                    fetch[i].1.push(face.family.to_string());
                } else if fetch.len() < crate::fonts::MAX_FONTS_PER_PAGE {
                    fetch.push((src.to_owned(), vec![face.family.to_string()], face.weight));
                }
            }
        }
        if known {
            self.fonts_changed(cx);
        }
        if std::env::var_os("BROWSER_DEBUG").is_some() {
            eprintln!("web fonts: used {}, faces {face_count}, fetching {:?}", used.len(), fetch.iter().map(|f| (&f.0, &f.1)).collect::<Vec<_>>());
        }
        if fetch.is_empty() {
            return;
        }
        self.fonts_requested.extend(fetch.iter().map(|(src, _, _)| src.clone()));
        self._fonts_tasks.push(cx.spawn(async move |this, cx| {
            let tasks: Vec<_> = fetch
                .into_iter()
                .map(|(src, families, weights)| {
                    let page_url = page_url.clone();
                    cx.background_spawn(async move {
                        let url = url::Url::parse(&src).ok()?;
                        if !loader::may_load(&page_url, &url) {
                            return Some((src, families, Vec::new()));
                        }
                        let bytes = net::fetch_bytes(&url);
                        let decoded = bytes.as_ref().map(|b| crate::fonts::decode_all(b.clone(), weights)).unwrap_or_default();
                        if std::env::var_os("BROWSER_DEBUG").is_some() {
                            eprintln!("web font {src}: {:?} bytes -> {} fonts", bytes.as_ref().map(|b| b.len()).map_err(|e| e.to_string()), decoded.len());
                        }
                        Some((src, families, decoded))
                    })
                })
                .collect();
            let results = futures::future::join_all(tasks).await;
            this.update(cx, |this, cx| {
                let mut faces = Vec::new();
                let mut aliases = Vec::new();
                for (src, families, decoded) in results.into_iter().flatten() {
                    let family = decoded.first().map(|(_, f)| f.clone());
                    for (data, _) in decoded {
                        faces.push(std::borrow::Cow::Owned(data));
                    }
                    if let Some(family) = &family {
                        aliases.extend(families.into_iter().map(|css| (css, family.clone())));
                    }
                    cx.global_mut::<crate::fonts::WebFonts>().loaded.insert(src, family);
                }
                if faces.is_empty() {
                    return;
                }
                let added = cx.text_system().add_fonts(faces);
                if let Err(err) = &added
                    && std::env::var_os("BROWSER_DEBUG").is_some()
                {
                    eprintln!("web fonts rejected: {err:#}");
                }
                if added.is_ok() {
                    for (css, family) in aliases {
                        this.palette.fonts.add(&css, &family);
                    }
                    if std::env::var_os("BROWSER_DEBUG").is_some() {
                        eprintln!("web fonts loaded for {}", this.page.as_ref().map(|p| p.url.to_string()).unwrap_or_default());
                    }
                    this.fonts_changed(cx);
                }
            })
            .ok();
        }));
    }

    /// Text metrics changed: every row's layout is stale.
    fn fonts_changed(&mut self, cx: &mut Context<Self>) {
        self.fonts_generation += 1;
        if let Some(page) = &self.page {
            let rows = page.layout.rows.len();
            self.list_state.remeasure_items(0..rows);
        }
        cx.notify();
    }

    fn reset_rows(&mut self, rows: usize) {
        self.list_state.reset(rows);
        self.row_hint_pending = true;
    }

    /// Give unmeasured rows the average height of the ones laid out so far,
    /// so the scrollbar starts near the page's real length.
    fn apply_row_hint(&mut self, window: &mut Window) {
        if !self.row_hint_pending {
            return;
        }
        let rows = self.list_state.item_count();
        let (mut sum, mut count) = (px(0.), 0usize);
        for ix in self.list_state.logical_scroll_top().item_ix..rows {
            match self.list_state.bounds_for_item(ix) {
                Some(b) => {
                    sum += b.size.height;
                    count += 1;
                }
                None if count > 0 => break,
                None => {}
            }
        }
        if count == 0 {
            if rows > 0 {
                window.request_animation_frame();
            }
            return;
        }
        self.row_hint_pending = false;
        self.list_state.clone().with_uniform_item_height(sum / count as f32);
    }

    fn load_favicon(&mut self, site: Option<Url>, url: Option<Url>, cx: &mut Context<Self>) {
        // The site's icon from last time, until (or unless) this one loads.
        self.favicon = site.as_ref().and_then(|s| crate::favicons::Favicons::get(s, cx));
        let Some(url) = url else {
            self._favicon_task = None;
            return;
        };
        self._favicon_task = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    let bytes = net::fetch_bytes(&url).ok()?;
                    let image = render::images::prepare(bytes.clone()).ok()?.image;
                    Some((bytes, image))
                })
                .await;
            this.update(cx, |this, cx| {
                if let Some((bytes, image)) = loaded {
                    if let Some(site) = site {
                        crate::favicons::Favicons::store(&site, bytes, image.clone(), cx);
                    }
                    this.favicon = Some(image);
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The pointer moved onto (or off) a link: show it in the status bubble
    /// and, if it stays a moment, start loading it.
    fn link_hovered(&mut self, url: Option<Url>, window: &mut Window, cx: &mut Context<Self>) {
        if self.hovered_link == url {
            return;
        }
        self.hovered_link = url.clone();
        cx.notify();
        let Some(url) = url else {
            self._hover_timer = None;
            return;
        };
        let current = self.page.as_ref().is_some_and(|p| same_document(&p.url, &url))
            || self.url().is_some_and(|u| same_document(&u, &url));
        let already = self.prerender.as_ref().is_some_and(|p| same_document(&p.url, &url));
        if current || already || !loader::prefetchable(&url) || self.capture.is_some() {
            return;
        }
        self._hover_timer = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(PRERENDER_DWELL).await;
            this.update_in(cx, |this, window, cx| {
                if this.hovered_link.as_ref() == Some(&url) {
                    if std::env::var_os("BROWSER_DEBUG").is_some() {
                        eprintln!("prerendering {url}");
                    }
                    let page = this.spawn_load(url.clone(), window, cx);
                    this.prerender = Some(Prerender { url, page });
                }
            })
            .ok();
        }));
    }

    /// Fetch the page's images in the background; each one re-measures the rows
    /// showing it when it arrives.
    fn load_images(&mut self, cx: &mut Context<Self>) {
        let Some(page) = &self.page else { return };
        // Everything not already loaded or failed (this replaces any previous
        // loader task, so in-flight images are queued again too).
        let jobs: Vec<Url> = {
            let mut store = self.images.borrow_mut();
            let jobs: Vec<Url> = page
                .layout
                .images
                .iter()
                .map(|(url, _)| url)
                .chain(page.layout.fixed_images.iter())
                .take(MAX_IMAGES)
                .filter(|url| loader::may_load(&page.url, url))
                .map(|url| (**url).clone())
                .filter(|url| matches!(store.get(url), None | Some(ImageState::Loading)))
                .collect();
            for url in &jobs {
                store.insert(url.clone(), ImageState::Loading);
            }
            jobs
        };
        self.images_pending = jobs.len();
        if jobs.is_empty() {
            self._image_task = None;
            return;
        }
        self._image_task = Some(cx.spawn(async move |this, cx| {
            let start = |cx: &mut AsyncApp, url: Url| {
                let task = cx.background_executor().spawn({
                    let url = url.clone();
                    async move { net::fetch_bytes(&url).and_then(render::images::prepare) }
                });
                async move { (url, task.await) }
            };
            let mut queue = jobs.into_iter();
            let mut in_flight = FuturesUnordered::new();
            for job in queue.by_ref().take(IMAGE_CONCURRENCY) {
                in_flight.push(start(cx, job));
            }
            while let Some((url, result)) = in_flight.next().await {
                if let Some(job) = queue.next() {
                    in_flight.push(start(cx, job));
                }
                let updated = this.update(cx, |this, cx| {
                    let debug = std::env::var_os("BROWSER_DEBUG").is_some();
                    let state = match result {
                        Ok(prepared) => prepared.into_state(),
                        Err(err) => {
                            if debug {
                                eprintln!("image failed: {url}: {err:#}");
                            }
                            ImageState::Failed
                        }
                    };
                    // Re-measure the rows (of the *current* layout) showing it.
                    let rows = this
                        .page
                        .as_ref()
                        .and_then(|p| p.layout.images.iter().find(|(u, _)| **u == url))
                        .map(|(_, rows)| rows.clone())
                        .unwrap_or_default();
                    this.images.borrow_mut().insert(url, state);
                    this.images_pending = this.images_pending.saturating_sub(1);
                    this.images_generation += 1;
                    for row in rows {
                        this.list_state.remeasure_items(row..row + 1);
                    }
                    cx.notify();
                });
                if updated.is_err() {
                    break;
                }
            }
        }));
    }

    /// The window was resized: if a media query now evaluates differently,
    /// restyle and re-lay-out the page (debounced, in the background).
    pub fn viewport_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = media_env(window);
        let Some(page) = &self.page else { return };
        let Some(styles) = &page.styles else { return };
        if styles.signature(&env) != page.signature {
            self.restyle(Duration::from_millis(150), window, cx);
        }
    }

    /// The interaction state implied by what's under the pointer etc.
    fn current_state(&self) -> ElementState {
        let mut state = ElementState::default();
        let Some(page) = &self.page else { return state };
        let Some(doc) = &page.doc else { return state };
        // :hover covers the element and all its ancestors, including ones whose
        // boxes were flattened away (they have no GPUI element to report).
        let deepest = self.hover_blocks.iter().copied().chain(self.hover_inline.map(|(_, n)| n));
        for node in deepest {
            let mut n = Some(node);
            while let Some(id) = n {
                if !state.hovered.insert(id) {
                    break;
                }
                n = doc.parent(id);
            }
        }
        if self.mouse_down {
            state.active = state.hovered.clone();
        }
        state.focused = self.forms.focused;
        state.checked = self.forms.checked.clone();
        state.target = page.target();
        state
    }

    /// Hover/active state changed: restyle if the page has rules that care.
    fn interaction_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_rules = self
            .page
            .as_ref()
            .and_then(|p| p.styles.as_ref())
            .is_some_and(|s| s.has_state_rules());
        if !has_rules {
            return;
        }
        if let Some(until) = self.scrolling_until {
            let now = std::time::Instant::now();
            if now < until {
                // Check again once the scrolling stops.
                self._scroll_end = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(until - now).await;
                    this.update_in(cx, |this, window, cx| {
                        this.scrolling_until = None;
                        this.interaction_changed(window, cx);
                    })
                    .ok();
                }));
                return;
            }
        }
        let state = Arc::new(self.current_state());
        if *state == *self.element_state {
            return;
        }
        let started = std::time::Instant::now();
        match self.patch_paint(&state, window) {
            Some(patched) => {
                if let Some(layout) = patched
                    && let Some(page) = &mut self.page
                {
                    // Same geometry: nothing to re-measure.
                    page.layout = Arc::new(layout);
                }
                self.element_state = state;
                if std::env::var_os("BROWSER_DEBUG").is_some() {
                    eprintln!("hover patched in {:?}", started.elapsed());
                }
                cx.notify();
                // Rules the patch may not have followed (`:has(:hover)`,
                // `a:hover + b`, big hovered containers) get caught by a full
                // restyle once the pointer settles.
                let reaches = self.page.as_ref().and_then(|p| p.styles.as_ref()).is_some_and(|s| s.state_reaches_others());
                if reaches {
                    self.restyle(Duration::from_millis(600), window, cx);
                }
            }
            None => {
                if std::env::var_os("BROWSER_DEBUG").is_some() {
                    eprintln!("hover needs a full restyle");
                }
                self.restyle(Duration::from_millis(16), window, cx)
            }
        }
    }

    /// A styler for the page as it's styled now (kept between hovers).
    fn styler(&mut self, window: &Window) -> Option<&style::Styler> {
        let styles = self.page.as_ref()?.styles.as_ref()?;
        let env = media_env(window);
        let signature = styles.signature(&env);
        if self.hover_styler.as_ref().is_none_or(|(sig, _)| *sig != signature) {
            self.hover_styler = Some((signature, styles.styler_with_state(env, self.element_state.clone())));
        }
        self.hover_styler.as_ref().map(|(_, s)| s)
    }

    /// Try to apply an interaction state change as a paint-only patch.
    /// `Some(None)`: nothing visible changed; `None`: needs a full restyle.
    fn patch_paint(&mut self, state: &Arc<ElementState>, window: &Window) -> Option<Option<render::LayoutTree>> {
        let page = self.page.as_ref()?;
        let signature = page.styles.as_ref()?.signature(&media_env(window));
        if page.signature != signature {
            return None;
        }
        self.styler(window)?;
        let page = self.page.as_ref()?;
        let (doc, styles) = (page.doc.as_ref()?, page.styles.as_ref()?);
        let (_, styler) = self.hover_styler.as_mut()?;
        match render::hover::patch(&page.layout, doc, styler, &self.element_state, state, styles.state_reaches_others()) {
            render::hover::Patch::Unchanged => Some(None),
            render::hover::Patch::Painted(layout, changed) => {
                self.start_transitions(changed);
                Some(Some(layout))
            }
            render::hover::Patch::NeedsRestyle(why) => {
                if std::env::var_os("BROWSER_DEBUG").is_some() {
                    eprintln!("hover can't patch: {why}");
                }
                None
            }
        }
    }

    /// Re-run style + layout in the background with the current viewport and
    /// interaction state, then swap it in, keeping the scroll position.
    fn restyle(&mut self, delay: Duration, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = &self.page else { return };
        let (Some(doc), Some(styles)) = (page.doc.clone(), page.styles.clone()) else { return };
        let page_gen = self.page_gen;
        self._restyle = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Ok(env) = cx.update(|window, _| media_env(window)) else { return };
            let Ok(state) = this.update(cx, |this, _| Arc::new(this.current_state())) else { return };
            let signature = styles.signature(&env);
            let Ok(old) = this.update(cx, |this, _| this.page.as_ref().map(|p| p.layout.clone())) else { return };
            let (layout, changed) = loader::deep({
                let state = state.clone();
                move || {
                    let mut layout = render::build(&doc, &styles.styler_with_state(env, state));
                    let changed = old.map(|old| layout.reuse_unchanged(&old)).unwrap_or_default();
                    (layout, changed)
                }
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                if this.page_gen != page_gen {
                    return;
                }
                let Some(page) = &mut this.page else { return };
                let same_shape = page.layout.rows.len() == layout.rows.len();
                page.layout = Arc::new(layout);
                page.signature = signature;
                this.element_state = state;
                if same_shape {
                    // Usually a hover changed a color in a row or two: re-measure
                    // just those (everything else kept its cached layout).
                    for ix in changed {
                        this.list_state.remeasure_items(ix..ix + 1);
                    }
                } else {
                    // Keep roughly the same place in the page.
                    let top = this.list_state.logical_scroll_top();
                    let rows = page.layout.rows.len();
                    this.reset_rows(rows);
                    this.list_state.scroll_to(ListOffset {
                        item_ix: top.item_ix.min(rows.saturating_sub(1)),
                        offset_in_item: top.offset_in_item,
                    });
                }
                this.load_images(cx);
                this.sync_keyframes(window);
                cx.notify();
                // The pointer moved on while this was building: catch up.
                this.interaction_changed(window, cx);
            })
            .ok();
        }));
    }

    /// Take the screenshots requested on the command line, then quit.
    pub fn start_capture(&mut self, job: CaptureJob, window: &mut Window, cx: &mut Context<Self>) {
        // Leave a little overlap between pages so nothing falls in a seam.
        let page_step = f32::from(window.viewport_size().height) - crate::CHROME_HEIGHT - 40.0;
        let settle = Duration::from_millis(600);
        self._capture_task = Some(cx.spawn_in(window, async move |this, cx| {
            let wait_for_images = async |cx: &mut AsyncWindowContext| {
                for _ in 0..50 {
                    let pending = this.update(cx, |this, _| this.images_pending).unwrap_or(0);
                    if pending == 0 {
                        break;
                    }
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                }
            };
            if job.frames > 0 {
                for frame in 0..job.frames {
                    let path = job.frame_path(frame);
                    cx.update(|window, cx| {
                        window.draw(cx).clear(cx);
                        if let Err(err) = capture::save_frame(window, &path) {
                            eprintln!("screenshot failed: {err:#}");
                        }
                    })
                    .ok();
                    cx.background_executor().timer(Duration::from_millis(job.interval)).await;
                }
                println!("saved {} frames", job.frames);
                cx.update(|_, cx| cx.quit()).ok();
                return;
            }
            if job.wait > 0 {
                cx.background_executor().timer(Duration::from_millis(job.wait)).await;
            }
            // Wait (a while) for images, then let the list lay out and measure.
            wait_for_images(cx).await;
            if let Some((w, h)) = job.resize {
                cx.update(|window, _| window.resize(size(px(w), px(h)))).ok();
                // Debounce + background restyle + image loads for the new layout.
                cx.background_executor().timer(Duration::from_millis(1200)).await;
                wait_for_images(cx).await;
            }
            if let Some(row) = job.row {
                this.update(cx, |this, cx| {
                    this.list_state.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.) });
                    cx.notify();
                })
                .ok();
            }
            cx.background_executor().timer(settle).await;
            for &(x, y) in &job.mouse {
                // Draw so hitboxes exist, then hover the point; time until
                // the frame that shows it is drawn.
                let t = std::time::Instant::now();
                cx.update(|window, cx| {
                    window.draw(cx).clear(cx);
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent {
                            position: point(px(x), px(y)),
                            pressed_button: None,
                            modifiers: Modifiers::default(),
                        }),
                        cx,
                    );
                    window.draw(cx).clear(cx);
                })
                .ok();
                if std::env::var_os("BROWSER_DEBUG").is_some() {
                    eprintln!("hover ({x},{y}): event + frame in {:?}", t.elapsed());
                }
                cx.background_executor().timer(Duration::from_millis(300)).await;
            }
            if let Some((x, y)) = job.click {
                let position = point(px(x), px(y));
                cx.update(|window, cx| {
                    window.draw(cx).clear(cx);
                    let down = MouseDownEvent {
                        button: MouseButton::Left,
                        position,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    };
                    window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }), cx);
                    window.dispatch_event(PlatformInput::MouseDown(down), cx);
                    window.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position, modifiers: Modifiers::default(), click_count: 1 }),
                        cx,
                    );
                })
                .ok();
                if let Some(keys) = job.keys.clone() {
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                    cx.update(|window, cx| {
                        let (text, enter) = match keys.strip_suffix("\\n") {
                            Some(t) => (t.to_owned(), true),
                            None => (keys.clone(), false),
                        };
                        for c in text.chars() {
                            // (A dev tool: leaking a few short strings is fine.)
                            let key: &'static str = if c == ' ' { "space" } else { Box::leak(c.to_string().into_boxed_str()) };
                            if let Ok(k) = Keystroke::parse(key) {
                                let k = Keystroke { key_char: Some(c.to_string()), ..k };
                                window.dispatch_keystroke(k, cx);
                            }
                        }
                        if enter && let Ok(k) = Keystroke::parse("enter") {
                            window.dispatch_keystroke(k, cx);
                        }
                    })
                    .ok();
                }
                // Whatever it did (restyle, navigation) gets time to finish.
                cx.background_executor().timer(Duration::from_millis(2500)).await;
                wait_for_images(cx).await;
            }
            if let Some((dx, end)) = job.swipe {
                cx.update(|window, cx| {
                    window.draw(cx).clear(cx);
                    let position = point(px(500.), window.viewport_size().height * 0.6);
                    let event = |delta: f32, touch_phase| {
                        PlatformInput::Scroll(ScrollEvent {
                            position,
                            delta: ScrollDelta::Pixels(point(px(delta), px(0.))),
                            modifiers: Modifiers::default(),
                            touch_phase,
                            source: Default::default(),
                        })
                    };
                    // Scrolls go to what's under the pointer.
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }),
                        cx,
                    );
                    // A frame between events, like real input: what's on screen
                    // decides where the next one goes.
                    let send = |window: &mut Window, cx: &mut App, e| {
                        let t = std::time::Instant::now();
                        window.dispatch_event(e, cx);
                        window.draw(cx).clear(cx);
                        if std::env::var_os("BROWSER_DEBUG").is_some() {
                            eprintln!("swipe frame {:.2}ms", t.elapsed().as_secs_f64() * 1000.0);
                        }
                    };
                    send(window, cx, event(0.0, TouchPhase::Started));
                    for _ in 0..10 {
                        send(window, cx, event(dx / 10.0, TouchPhase::Moved));
                    }
                    if end {
                        send(window, cx, event(0.0, TouchPhase::Ended));
                    }
                })
                .ok();
                cx.background_executor().timer(Duration::from_millis(1500)).await;
            }
            if let Some(dy) = job.wheel {
                // Like a trackpad: fingers move, lift, then momentum (macOS
                // sends it with no phase, which arrives as Moved).
                let mut times = Vec::new();
                cx.update(|window, cx| {
                    window.draw(cx).clear(cx);
                    let position = point(px(500.), window.viewport_size().height * 0.6);
                    let event = |delta: f32, touch_phase| {
                        PlatformInput::Scroll(ScrollEvent {
                            position,
                            delta: ScrollDelta::Pixels(point(px(0.), px(-delta))),
                            modifiers: Modifiers::default(),
                            touch_phase,
                            source: Default::default(),
                        })
                    };
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }),
                        cx,
                    );
                    let mut frame = |window: &mut Window, cx: &mut App, e| {
                        let t = std::time::Instant::now();
                        window.dispatch_event(e, cx);
                        window.draw(cx).clear(cx);
                        times.push(t.elapsed().as_secs_f64() * 1000.0);
                    };
                    frame(window, cx, event(0.0, TouchPhase::Started));
                    for _ in 0..20 {
                        frame(window, cx, event(dy / 30.0, TouchPhase::Moved));
                    }
                    frame(window, cx, event(0.0, TouchPhase::Ended));
                    for i in 0..30 {
                        frame(window, cx, event(dy / 30.0 * (1.0 - i as f32 / 30.0), TouchPhase::Moved));
                    }
                })
                .ok();
                times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                println!(
                    "wheel: {} frames, median {:.2}ms, max {:.2}ms",
                    times.len(),
                    times[times.len() / 2],
                    times.last().copied().unwrap_or_default()
                );
            }
            if let Some(text) = job.typing.clone() {
                this.update(cx, |_, cx| cx.emit(TabEvent::TypeInOmnibox(text))).ok();
                cx.background_executor().timer(Duration::from_millis(300)).await;
            }
            if let Some(frames) = job.bench {
                // Scroll in small steps, timing each full frame (layout + paint).
                let mut times = Vec::with_capacity(frames);
                let rows = this.update(cx, |this, _| this.page.as_ref().map_or(0, |p| p.layout.rows.len())).unwrap_or(0);
                for f in 0..frames {
                    // One row per frame: about what a fast trackpad scroll reveals.
                    this.update(cx, |this, cx| {
                        let item_ix = f.min(rows.saturating_sub(1));
                        this.list_state.scroll_to(ListOffset { item_ix, offset_in_item: px(0.) });
                        cx.notify();
                    })
                    .ok();
                    let t = std::time::Instant::now();
                    cx.update(|window, cx| window.draw(cx).clear(cx)).ok();
                    times.push(t.elapsed().as_secs_f64() * 1000.0);
                    if std::env::var_os("SCROLLBAR_TRACE").is_some() {
                        this.update(cx, |this, _| {
                            let max = this.list_state.max_offset_for_scrollbar().y;
                            let at = -this.list_state.scroll_px_offset_for_scrollbar().y;
                            eprintln!("frame {f}: at {at:.0} of {max:.0} = {:.3}", at / max.max(px(1.)));
                        })
                        .ok();
                    }
                }
                times.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let avg = times.iter().sum::<f64>() / times.len() as f64;
                let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
                println!(
                    "bench: {frames} frames, avg {avg:.2}ms, p95 {p95:.2}ms, max {:.2}ms",
                    times.last().unwrap()
                );
            }
            for page in 0..job.pages {
                let step = if page == 0 { job.scroll } else { page_step };
                // Scroll in small steps, drawing in between: the list measures rows
                // lazily, so one big jump would skip past unmeasured (zero-height) rows.
                let mut remaining = step;
                while remaining > 0.0 {
                    let chunk = remaining.min(300.0);
                    remaining -= chunk;
                    this.update(cx, |this, cx| {
                        this.list_state.scroll_by(px(chunk));
                        cx.notify();
                    })
                    .ok();
                    cx.update(|window, cx| window.draw(cx).clear(cx)).ok();
                }
                cx.background_executor().timer(settle).await;
                let path = job.path_for(page);
                // Draw explicitly (a background window may not be getting display
                // refreshes), twice: GPUI decodes images on first use, so the first
                // frame kicks off decoding and a later one shows the result.
                cx.update(|window, cx| window.draw(cx).clear(cx)).ok();
                cx.background_executor().timer(Duration::from_millis(400)).await;
                cx.update(|window, cx| {
                    window.draw(cx).clear(cx);
                    match capture::save_frame(window, &path) {
                        Ok(()) => println!("saved {}", path.display()),
                        Err(err) => eprintln!("screenshot failed: {err:#}"),
                    }
                })
                .ok();
            }
            cx.update(|_, cx| cx.quit()).ok();
        }));
    }

    fn scroll_to_fragment(&self, fragment: Option<&str>) {
        let row = fragment
            .and_then(|f| self.page.as_ref()?.layout.row_for_anchor(f))
            .unwrap_or(0);
        self.list_state.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.) });
    }

    fn render_cx(&self, cx: &Context<Self>) -> RenderCx {
        let this = cx.entity().downgrade();
        let (click, hover, block_hover, inline_hover) = (this.clone(), this.clone(), this.clone(), this);
        let track_hover = self
            .page
            .as_ref()
            .is_some_and(|p| p.scripts.is_some() || p.styles.as_ref().is_some_and(|s| s.has_state_rules()));
        let page_gen = self.page_gen;
        let controls = cx.entity().downgrade();
        RenderCx {
            controls: Rc::new(move |control, style, window, cx| {
                controls
                    .update(cx, |this, cx| this.control_element(control, style, window, cx))
                    .unwrap_or_else(|_| div().into_any_element())
            }),
            palette: self.palette.clone(),
            images: self.images.clone(),
            images_generation: self.images_generation,
            fonts_generation: self.fonts_generation,
            track_hover,
            on_block_hover: Rc::new(move |node, hovered, window, cx| {
                block_hover
                    .update(cx, |this, cx| {
                        if this.page_gen != page_gen {
                            return;
                        }
                        let changed = if hovered {
                            this.hover_blocks.insert(node)
                        } else {
                            this.hover_blocks.remove(&node)
                        };
                        if changed {
                            this.interaction_changed(window, cx);
                        }
                    })
                    .ok();
            }),
            on_inline_hover: Rc::new(move |para, node, window, cx| {
                inline_hover
                    .update(cx, |this, cx| {
                        if this.page_gen != page_gen {
                            return;
                        }
                        let next = match node {
                            Some(node) => Some((para, node)),
                            // Leaving this paragraph only clears hover if it was this one's.
                            None if this.hover_inline.is_some_and(|(p, _)| p == para) => None,
                            None => return,
                        };
                        if this.hover_inline != next {
                            this.hover_inline = next;
                            this.interaction_changed(window, cx);
                        }
                    })
                    .ok();
            }),
            on_link_click: Rc::new(move |url, window, cx| {
                if std::env::var_os("BROWSER_DEBUG").is_some() {
                    eprintln!("link click {url}");
                }
                // Buttons and labels: activate the element.
                if let Some(node) = render::activation_target(url) {
                    click
                        .update(cx, |this, cx| this.link_clicked(move |this, window, cx| this.activate(node, window, cx), window, cx))
                        .ok();
                    return;
                }
                let url = url.clone();
                let new_tab = window.modifiers().secondary(cx);
                click
                    .update(cx, |this, cx| {
                        this.link_clicked(
                            move |this, window, cx| {
                                if new_tab {
                                    cx.emit(TabEvent::OpenInNewTab(url));
                                } else {
                                    this.navigate(url, window, cx);
                                }
                            },
                            window,
                            cx,
                        )
                    })
                    .ok();
            }),
            on_link_hover: Rc::new(move |url, window, cx| {
                let url = url.filter(|u| render::activation_target(u).is_none()).cloned();
                hover.update(cx, |this, cx| this.link_hovered(url, window, cx)).ok();
            }),
        }
    }

    fn render_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        if let Some(err) = &self.error {
            return div().p_4().text_color(cx.theme().danger).child(err.clone()).into_any_element();
        }
        let Some(page) = &self.page else {
            return self.render_new_tab(cx);
        };
        if let Some(editor) = &self.source_editor {
            let theme = cx.theme();
            let pretty = self.source_pretty;
            let toggle = div()
                .id("pretty-print")
                .absolute()
                .bottom_3()
                .right_5()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(if pretty { theme.primary } else { theme.background })
                .text_color(if pretty { theme.primary_foreground } else { theme.foreground })
                .text_xs()
                .cursor_pointer()
                .shadow_md()
                .child(Icon::new(Lucide::Braces).size(px(14.)))
                .child(if pretty { "Pretty" } else { "Raw" })
                .on_click(cx.listener(|this, _, window, cx| this.toggle_pretty(window, cx)));
            return div()
                .size_full()
                .relative()
                .bg(theme.background)
                .child(gpui_kit::component::input::Editor::new(editor).readonly(true).appearance(false).size_full())
                .child(toggle)
                .into_any_element();
        }

        // Mid-animation, the frame's in-between styles.
        let layout = self.animations.display.clone().unwrap_or_else(|| page.layout.clone());
        let rcx = self.render_cx(cx);
        let rcx_fixed = rcx.clone();
        let last = layout.rows.len().saturating_sub(1);
        let _ = palette;
        div()
            .size_full()
            .child(
                list(self.list_state.clone(), move |ix, _window, _cx| {
                    let row = render::render_row(&layout.rows[ix], &rcx);
                    if ix == last {
                        div().pb(px(layout.space_after)).child(row).into_any_element()
                    } else {
                        row
                    }
                })
                .size_full(),
            )
            .vertical_scrollbar(&self.list_state)
            // `position: fixed` boxes, pinned over the viewport.
            .when(!page.layout.fixed.is_empty(), |d| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .children(page.layout.fixed.iter().map(|b| render::render_fixed(b, &rcx_fixed))),
                )
            })
            .into_any_element()
    }

    /// Two-finger swipes go back and forward (Safari-style): the page
    /// follows the fingers sideways, uncovering the page it would go to; let
    /// go past the line (or flick) and it slides the rest of the way.
    fn track_swipe(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(20.));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        let width = f32::from(window.viewport_size().width);
        // Page content scrolling sideways (a wide <pre>) keeps the gesture.
        let claimed = render::view::sideways::take_claim();
        match event.touch_phase {
            TouchPhase::Started => {
                self.swipe_active = self.swipe_release.is_none();
                self.swipe = 0.0;
                self.swipe_velocity = 0.0;
            }
            TouchPhase::Moved => {
                // Mostly sideways, and somewhere to go that way.
                if self.swipe_active && ((dx.abs() > dy.abs() * 1.5 && !claimed) || self.swipe != 0.0) {
                    let next = self.swipe + dx;
                    let open = (next > 0.0 && self.can_go_back()) || (next < 0.0 && self.can_go_forward());
                    // Nowhere to go: the page gives a little, reluctantly.
                    self.swipe = if open { next.clamp(-width, width) } else { (self.swipe + dx * 0.2).clamp(-48.0, 48.0) };
                    self.swipe_velocity = if open { dx } else { 0.0 };
                    if self.swipe != 0.0 {
                        self._swipe_watchdog = Some(cx.spawn_in(window, async move |this, cx| {
                            cx.background_executor().timer(Duration::from_millis(500)).await;
                            this.update_in(cx, |this, window, cx| this.finish_swipe(true, window, cx)).ok();
                        }));
                    }
                    cx.notify();
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.finish_swipe(event.touch_phase == TouchPhase::Ended, window, cx)
            }
        }
    }

    /// The fingers lifted (`ended`) or the gesture was cancelled: slide the
    /// page the rest of the way, or back.
    fn finish_swipe(&mut self, ended: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.swipe_active = false;
        self._swipe_watchdog = None;
        let from = std::mem::take(&mut self.swipe);
        if from == 0.0 {
            return;
        }
        let width = f32::from(window.viewport_size().width);
        let flicked = self.swipe_velocity * from.signum() > SWIPE_FLICK;
        let open = if from > 0.0 { self.can_go_back() } else { self.can_go_forward() };
        let go = open && ended && (from.abs() >= width * SWIPE_GO || flicked);
        let to = if go { width * from.signum() } else { 0.0 };
        self.swipe_release = Some((from, to, std::time::Instant::now()));
        self._swipe_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SWIPE_SETTLE).await;
            this.update_in(cx, |this, window, cx| {
                this.swipe_release = None;
                this.swipe_peek = None;
                if go && from > 0.0 {
                    this.go_back(window, cx);
                } else if go {
                    this.go_forward(window, cx);
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Where the page is mid-swipe (px; 0 when not swiping).
    fn swipe_offset(&self, window: &mut Window) -> f32 {
        let Some((from, to, start)) = self.swipe_release else { return self.swipe };
        let t = (start.elapsed().as_secs_f32() / SWIPE_SETTLE.as_secs_f32()).min(1.0);
        if t < 1.0 {
            window.request_animation_frame();
        }
        let eased = 1.0 - (1.0 - t).powi(3);
        from + (to - from) * eased
    }

    /// The page a swipe uncovers: the back/forward page as it was left,
    /// drawn without interaction (or a blank canvas if it has to load).
    fn render_peek(&mut self, back: bool, cx: &mut Context<Self>) -> AnyElement {
        let ix = if back { self.history_ix.checked_sub(1) } else { Some(self.history_ix + 1) };
        let Some((ix, (page, scroll))) = ix.and_then(|ix| Some((ix, self.page_cache.get(&ix)?))) else {
            return div().size_full().bg(self.palette.canvas).into_any_element();
        };
        // Its own list: only the rows on screen are laid out, and they keep
        // their measurements while the swipe goes on.
        if self.swipe_peek.as_ref().is_none_or(|(at, _)| *at != ix) {
            let list = ListState::new(page.layout.rows.len(), ListAlignment::Top, px(0.));
            list.scroll_to(*scroll);
            self.swipe_peek = Some((ix, list));
        }
        let list_state = self.swipe_peek.as_ref().map(|(_, l)| l.clone()).expect("just set");
        let mut rcx = self.render_cx(cx);
        rcx.track_hover = false;
        rcx.controls = Rc::new(|_: &render::Control, _: &style::ComputedStyle, _: &mut Window, _: &mut App| {
            div().into_any_element()
        });
        let layout = page.layout.clone();
        let canvas = layout.canvas.map(|c| self.palette.color(c)).unwrap_or(self.palette.canvas);
        // Its own id scope: paragraph ids restart on every page, and two
        // pages' rows sharing element state would undo each other's caches.
        div()
            .id("swipe-peek")
            .size_full()
            .bg(canvas)
            .child(list(list_state, move |ix, _, _| render::render_row(&layout.rows[ix], &rcx)).size_full())
            .into_any_element()
    }

    fn new_auth_prompt(&mut self, realm: String, window: &mut Window, cx: &mut Context<Self>) -> AuthPrompt {
        use gpui_kit::component::input::{InputEvent, InputState};
        let user = cx.new(|cx| InputState::new(window, cx).placeholder("Username"));
        let password = cx.new(|cx| InputState::new(window, cx).placeholder("Password").masked(true));
        let submit = |this: &mut Self, _: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>| {
            if let InputEvent::PressEnter { .. } = event {
                this.sign_in(window, cx);
            }
        };
        let subs = vec![cx.subscribe_in(&user, window, submit), cx.subscribe_in(&password, window, submit)];
        user.update(cx, |s, cx| s.focus(window, cx));
        AuthPrompt { realm, user, password, _subscriptions: subs }
    }

    fn sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.auth_prompt.take() else { return };
        let Some(url) = self.url() else { return };
        let user = prompt.user.read(cx).value().to_string();
        let password = prompt.password.read(cx).value().to_string();
        net::set_credentials(&url, &user, &password);
        self.reload(window, cx);
    }

    fn render_auth_prompt(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use gpui_kit::component::button::Button;
        use gpui_kit::component::input::Input;
        let prompt = self.auth_prompt.as_ref()?;
        let theme = cx.theme();
        let url = self.url();
        let origin = url.as_ref().map(|u| u.origin().ascii_serialization()).unwrap_or_default();
        let insecure = url.as_ref().is_some_and(|u| u.scheme() == "http");
        let blue: Hsla = rgb(0x0b57d0).into();
        let label = |text: &'static str| div().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.muted_foreground).child(text);
        Some(
            // A sheet hanging under the toolbar, like Chrome's.
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(8.))
                .child(
                    div()
                        .id("auth-prompt")
                        .occlude()
                        .w(px(380.))
                        .flex()
                        .flex_col()
                        .gap_4()
                        .p_6()
                        .rounded_xl()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .text_color(theme.popover_foreground)
                        .shadow_xl()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .size(px(36.))
                                        .flex_none()
                                        .rounded_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(blue.opacity(0.12))
                                        .child(Icon::new(Lucide::LockKeyhole).size(px(18.)).text_color(blue)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .min_w_0()
                                        .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child("Sign in"))
                                        .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(origin)),
                                ),
                        )
                        .when(!prompt.realm.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("The site says: “{}”", prompt.realm)),
                            )
                        })
                        .when(insecure, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(theme.danger)
                                    .child(Icon::new(Lucide::TriangleAlert).size(px(14.)))
                                    .child("Your password will be sent unencrypted."),
                            )
                        })
                        .child(div().flex().flex_col().gap_1p5().child(label("Username")).child(Input::new(&prompt.user)))
                        .child(div().flex().flex_col().gap_1p5().child(label("Password")).child(Input::new(&prompt.password)))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .pt_1()
                                .child(
                                    Button::new("auth-cancel")
                                        .outline()
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.auth_prompt = None;
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    div()
                                        .id("auth-sign-in")
                                        .px_4()
                                        .py_1p5()
                                        .rounded_md()
                                        .bg(blue)
                                        .hover(|d| d.bg(blue.opacity(0.9)))
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(gpui_kit::white())
                                        .cursor_pointer()
                                        .child("Sign in")
                                        .on_click(cx.listener(|this, _, window, cx| this.sign_in(window, cx))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Switch a source page between pretty-printed and raw.
    fn toggle_pretty(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(source) = self.page.as_ref().and_then(|p| p.source.clone()) else { return };
        let Some(editor) = self.source_editor.clone() else { return };
        self.source_pretty = !self.source_pretty;
        let text = if self.source_pretty {
            source.pretty.clone().unwrap_or_else(|| crate::pretty::pretty(source.language, &source.raw).into())
        } else {
            source.raw.clone()
        };
        let text = SharedString::from(text.to_string());
        editor.update(cx, |e, cx| e.set_value(text, window, cx));
        cx.notify();
    }

    /// No page yet: the Feather start page, with your most visited sites.
    fn render_new_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let sites = cx.global::<History>().top_sites(8);
        let icons: Vec<_> = sites.iter().map(|e| crate::favicons::Favicons::get(&e.url, cx)).collect();
        let theme = cx.theme();
        let tiles = sites.into_iter().zip(icons).enumerate().map(|(ix, (entry, icon))| {
            let host = entry.url.host_str().unwrap_or_default().trim_start_matches("www.").to_owned();
            let initial = host.chars().next().unwrap_or('?').to_ascii_uppercase().to_string();
            let url = entry.url.clone();
            div()
                .id(("top-site", ix))
                .w(px(112.))
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .p_3()
                .rounded_lg()
                .cursor_pointer()
                .hover(|d| d.bg(theme.secondary))
                .child(
                    div()
                        .size(px(48.))
                        .rounded_full()
                        .bg(theme.secondary)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_lg()
                        .text_color(theme.muted_foreground)
                        .map(|d| match icon {
                            Some(icon) => d.child(img(icon).size(px(24.))),
                            None => d.child(initial),
                        }),
                )
                .child(div().w_full().text_xs().text_center().truncate().child(host))
                .on_click(cx.listener(move |this, _, window, cx| this.navigate(url.clone(), window, cx)))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(relative(0.18))
            .gap_6()
            .bg(theme.background)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(Icon::new(Lucide::Feather).size(px(44.)).text_color(theme.primary))
                    .child(div().text_3xl().font_weight(FontWeight::SEMIBOLD).child("Feather")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if self.loading.is_some() { "Loading…" } else { "Now with JavaScript. Still no web engine." }),
            )
            .child(div().flex().flex_wrap().justify_center().max_w(px(480.)).children(tiles))
            .into_any_element()
    }
}

impl Render for Tab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.animation_frame(window);
        self.apply_row_hint(window);
        let theme = cx.theme();
        let accent = theme.primary;
        let palette = &self.palette;
        // The page's own background (from <html>/<body>), else the default canvas.
        let canvas = self
            .page
            .as_ref()
            .and_then(|p| p.layout.canvas)
            .map(|c| palette.color(c))
            .unwrap_or(palette.canvas);
        let status = self.hovered_link.as_ref().map(|url| {
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .max_w(relative(0.6))
                .px_2()
                .py_0p5()
                .text_xs()
                .bg(theme.background)
                .text_color(theme.muted_foreground)
                .border_t_1()
                .border_r_1()
                .rounded_tr_md()
                .border_color(theme.border)
                .truncate()
                .child(url.to_string())
        });

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .bg(canvas)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // A click in a form field focuses the field (it handled
                    // the event first); don't take focus back from it.
                    if !this.focus_handle.contains_focused(window, cx) {
                        this.focus_handle.focus(window, cx);
                    }
                    this.mouse_down = true;
                    this.interaction_changed(window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.mouse_down = false;
                    this.script_mouse_up();
                    this.interaction_changed(window, cx);
                }),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                this.scrolling_until = Some(std::time::Instant::now() + Duration::from_millis(150));
                this.track_swipe(event, window, cx);
            }))
            .map(|d| {
                let offset = self.swipe_offset(window);
                let width = f32::from(window.viewport_size().width);
                // The page itself, moved by the swipe: over the page it
                // uncovers going back, under the one sliding in going forward.
                let page = div()
                    .absolute()
                    .top_0()
                    .left(px(if offset < 0.0 { offset * 0.3 } else { offset }))
                    .size_full()
                    .bg(canvas)
                    .child(self.render_content(cx));
                if offset == 0.0 {
                    return d.child(page);
                }
                let back = offset > 0.0;
                let open = if back { self.can_go_back() } else { self.can_go_forward() };
                // How uncovered the lower page is (0..1).
                let progress = (offset.abs() / width).min(1.0);
                // A soft shadow cast by the top page's leading edge onto the
                // lower one, and the lower page dimmed until it's uncovered.
                let edge_shadow = |left: f32| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(left - 28.0))
                        .w(px(28.0))
                        .bg(linear_gradient(
                            90.,
                            linear_color_stop(gpui_kit::black().opacity(0.0), 0.0),
                            linear_color_stop(gpui_kit::black().opacity(0.16 * (1.0 - progress * 0.6)), 1.0),
                        ))
                };
                let shade = if back { 1.0 - progress } else { progress };
                let dim = div().absolute().top_0().left_0().size_full().bg(gpui_kit::black().opacity(0.12 * shade));
                let lower_left = if back { (offset - width) * 0.3 } else { offset * 0.3 };
                let upper_left = if back { offset } else { width + offset };
                let peek = div()
                    .absolute()
                    .top_0()
                    .left(px(if back { lower_left } else { upper_left }))
                    .size_full()
                    .map(|p| if open { p.child(self.render_peek(back, cx)) } else { p.bg(cx.theme().secondary) });
                if back {
                    d.child(peek.child(dim)).child(edge_shadow(upper_left)).child(page)
                } else {
                    d.child(page.child(dim)).child(edge_shadow(upper_left)).child(peek)
                }
            })
            .when(self.loading.is_some(), |d| {
                // Indeterminate progress: a bar sweeping across the top.
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .h(px(2.))
                        .w(relative(0.3))
                        .bg(accent)
                        .with_animation(
                            "loading-bar",
                            Animation::new(Duration::from_millis(900)).repeat(),
                            |bar, t| bar.left(relative(t * 1.3 - 0.3)),
                        ),
                )
            })
            .children(status)
            .children(self.render_auth_prompt(cx))
    }
}
