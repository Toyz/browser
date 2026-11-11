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
    /// What each image element last showed (see `RenderCx::shown_images`).
    shown_images: Rc<std::cell::RefCell<HashMap<NodeId, Arc<url::Url>>>>,
    /// Images waiting for the loader, most wanted first, and how many it
    /// has in flight (see `load_images`).
    image_queue: Rc<std::cell::RefCell<std::collections::VecDeque<Url>>>,
    images_in_flight: Rc<std::cell::Cell<usize>>,
    image_loader_running: Rc<std::cell::Cell<bool>>,
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
    /// Mouse presses so far, and the press a link last acted on: nested
    /// flows each hear the release over a button, one press acts once.
    presses: u64,
    link_press: u64,
    /// The last mouse move was over something scripts listen to.
    script_hovering: bool,
    _refresh: Option<Task<()>>,
    _restyle: Option<Task<()>>,
    /// Painted boxes of the elements scripts measure ([x, y, w, h] in the
    /// page's viewport) and the viewport's size, gathered while painting,
    /// and what the script thread was last told.
    measured_bounds: Rc<std::cell::RefCell<(HashMap<NodeId, [f32; 4]>, (f32, f32))>>,
    /// Where each sticky box was last drawn in its place (x, y in the
    /// page, width, height): its pinned copy goes over it.
    sticky_bounds: Rc<std::cell::RefCell<HashMap<NodeId, [f32; 4]>>>,
    /// The last restyle's element styles, for the next to reuse.
    style_cache: Arc<std::sync::Mutex<style::StyleCache>>,
    /// This tab shows an `<iframe>`'s document: the page's tab and the
    /// element (see `frames`).
    pub(crate) frame: Option<(WeakEntity<Tab>, NodeId)>,
    /// The page's `<iframe>`s: each one's address and the tab showing it.
    frames: HashMap<NodeId, (Url, Entity<Tab>)>,
    /// Their views, for the renderer.
    embeds: Rc<HashMap<NodeId, AnyView>>,
    /// A frame's size when last styled (its media queries follow it).
    frame_size: Size<Pixels>,
    /// Bumped each time the tab paints (by an element painted before the
    /// page): bounds reported twice in one frame (a container split across
    /// rows) are unioned, and what a frame didn't paint is off screen.
    bounds_frame: Rc<std::cell::Cell<u64>>,
    /// The painted frame `note_unpainted` last looked at.
    bounds_checked: u64,
    /// Documents scripts sent (numbered), the one the layout is of, and
    /// the painted frame it was installed at.
    doc_gen: u64,
    installed_gen: u64,
    installed_frame: u64,
    /// Scripts waiting for a document to be laid out and painted (a
    /// forced layout): the reply, and the document it waits for.
    after_layout: Vec<(std::sync::mpsc::Sender<()>, u64)>,
    bounds_seen: Rc<std::cell::RefCell<HashMap<NodeId, u64>>>,
    layout_sent: HashMap<NodeId, [f32; 4]>,
    /// Measured elements painted in an earlier frame but not the latest,
    /// for the next `send_layout`.
    layout_gone: Vec<NodeId>,
    /// (Debugging) the list's scroll position last frame.
    debug_scroll_top: std::cell::Cell<Option<(usize, f32, usize)>>,
    /// (FEATHER_PERF) frame statistics, printed each second.
    perf: Option<Perf>,
    /// What was drawn far off screen last frame (skipped this one).
    cull: Option<Rc<render::cull::Cull>>,
    /// Sound is playing in the page (the tab strip shows a speaker).
    pub(super) audible: bool,
    /// The user muted the tab (kept across its pages).
    pub(super) muted: bool,
    /// Measured elements painted in the last frame scripts were told about.
    layout_painted: HashSet<NodeId>,
    /// The elements scripts measure that the current layout was built with
    /// (each has a box, if it renders at all).
    layout_keep: HashSet<NodeId>,
    /// Ones of those reported unpainted and not painted since.
    unpainted_sent: HashSet<NodeId>,
    /// Which row each measured element is in, for the layout it was
    /// worked out for (see `rows_of_measured`).
    measured_rows: Option<(usize, usize, HashMap<NodeId, usize>)>,
    layout_viewport_sent: (f32, f32),
    /// The scroll position the page's scripts were last told.
    layout_scroll_sent: (f32, f32),
    /// The rows each sticky box and container spans, for the layout (by
    /// address) they were found in.
    sticky_rows: std::cell::RefCell<(usize, HashMap<NodeId, (usize, usize)>)>,
    /// Each canvas's current frame (replaced ones leave the GPU atlas).
    canvas_frames: HashMap<NodeId, Arc<RenderImage>>,
    /// Pictures to take out of the GPU atlas at the next render (which has
    /// the window to do it with).
    atlas_drops: Vec<Arc<RenderImage>>,
    /// A smooth scroll in progress: (from, to, started), px of the page.
    scroll_animation: Option<(f32, f32, std::time::Instant)>,
    layout_scheduled: Rc<std::cell::Cell<bool>>,
    /// A restyle is building; another was asked for meanwhile.
    restyle_busy: bool,
    restyle_queued: bool,
    /// Restyles in a row not shown because the pointer had moved on (see
    /// the install in `restyle`).
    stale_skips: u8,
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
    /// What the page's media queries see: the window's page area, or for a
    /// frame, its own box once it has been drawn (the `<iframe>`'s size).
    fn env(&self, window: &Window) -> style::MediaEnv {
        let mut env = media_env(window);
        if self.frame.is_some() {
            let size = self.list_state.viewport_bounds().size;
            if size.width > px(0.) && size.height > px(0.) {
                env.width = f32::from(size.width);
                env.height = f32::from(size.height);
            }
        }
        env
    }

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
            shown_images: Rc::default(),
            image_queue: Rc::default(),
            images_in_flight: Rc::default(),
            image_loader_running: Rc::default(),
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
            presses: 0,
            link_press: 0,
            script_hovering: false,
            _refresh: None,
            _restyle: None,
            restyle_busy: false,
            restyle_queued: false,
            stale_skips: 0,
            measured_bounds: Rc::default(),
            sticky_bounds: Rc::default(),
            style_cache: Arc::default(),
            frame: None,
            frames: HashMap::new(),
            embeds: Rc::default(),
            frame_size: Size::default(),
            bounds_frame: Rc::default(),
            bounds_checked: 0,
            doc_gen: 0,
            installed_gen: 0,
            installed_frame: 0,
            after_layout: Vec::new(),
            bounds_seen: Rc::default(),
            layout_sent: HashMap::new(),
            layout_gone: Vec::new(),
            debug_scroll_top: Default::default(),
            perf: std::env::var_os("FEATHER_PERF").map(|_| Perf::default()),
            cull: std::env::var_os("FEATHER_NO_CULL").is_none().then(Rc::default),
            audible: false,
            muted: false,
            layout_painted: HashSet::new(),
            layout_keep: HashSet::new(),
            unpainted_sent: HashSet::new(),
            measured_rows: None,
            layout_viewport_sent: (0.0, 0.0),
            layout_scroll_sent: (0.0, 0.0),
            scroll_animation: None,
            canvas_frames: HashMap::new(),
            atlas_drops: Vec::new(),
            sticky_rows: Default::default(),
            layout_scheduled: Rc::default(),
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

    /// Whether the page plays sound now.
    pub fn is_audible(&self) -> bool {
        self.audible
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Mute or unmute everything the tab plays.
    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        if let Some(host) = self.page.as_ref().and_then(|p| p.scripts.as_ref()) {
            host.send(js::ToJs::MuteTab(self.muted));
        }
        cx.notify();
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
        loader::activate_origin(&url);
        self.script_navigate(url, window, cx);
    }

    /// A page's scripts went somewhere: not the user's doing.
    fn script_navigate(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
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
                        this.sync_frames(window, cx);
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
        let env = self.env(window);
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
        let frame = self.frame.is_some();
        let page = loader::deep(move || loader::load(&url, env, post.as_deref(), referer.as_ref(), session, frame).map_err(Arc::new));
        cx.background_spawn(page).shared()
    }

    /// Put a loaded page on screen.
    fn install(&mut self, page: LoadedPage, window: &mut Window, cx: &mut Context<Self>) {
        render::view::scrolled::forget();
        // The last page's frames go with it.
        self.frames.clear();
        self.embeds = Rc::default();
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
            // The last page's images still waiting aren't wanted any more
            // (ones in flight finish into the cache).
            self.image_queue.borrow_mut().clear();
        }
        self.shown_images.borrow_mut().clear();
        self.atlas_drops.extend(self.canvas_frames.drain().map(|(_, f)| f));
        self.page = Some(page);
        self.page_gen += 1;
        // Work for the previous page must not land on this one.
        self._restyle = None;
        self.restyle_busy = false;
        self.restyle_queued = false;
        self.measured_bounds.borrow_mut().0.clear();
        self.sticky_bounds.borrow_mut().clear();
        self.layout_sent.clear();
        self.layout_gone.clear();
        self.layout_painted.clear();
        self.layout_keep.clear();
        self.unpainted_sent.clear();
        self.layout_viewport_sent = (0.0, 0.0);
        self.layout_scroll_sent = (0.0, 0.0);
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

    /// A script waits for the document it just sent to be laid out (a
    /// forced layout): answered by `answer_after_layout`.
    pub(super) fn wait_for_layout(&mut self, reply: std::sync::mpsc::Sender<()>, cx: &mut Context<Self>) {
        if self.page.as_ref().and_then(|p| p.scripts.as_ref()).is_none() {
            reply.send(()).ok();
            return;
        }
        if std::env::var_os("BROWSER_DEBUG").is_some() {
            eprintln!("forced: wait for doc {} (installed {}, busy {}) at {:?}", self.doc_gen, self.installed_gen, self.restyle_busy, crate::started().elapsed());
        }
        self.after_layout.push((reply, self.doc_gen));
        cx.notify();
    }

    /// Scripts waiting for a layout whose document is now laid out, and
    /// painted since (`note_unpainted` has the painted boxes): go on.
    fn answer_after_layout(&mut self, window: &mut Window) {
        if self.after_layout.is_empty() {
            return;
        }
        let painted = self.bounds_checked > self.installed_frame;
        let installed = self.installed_gen;
        self.after_layout.retain(|(reply, wanted)| {
            if painted && installed >= *wanted {
                if std::env::var_os("BROWSER_DEBUG").is_some() {
                    eprintln!("forced: answered doc {wanted} at {:?}", crate::started().elapsed());
                }
                reply.send(()).ok();
                return false;
            }
            true
        });
        // Nothing else may ask for the frames it's waiting on.
        if !self.after_layout.is_empty() {
            window.request_animation_frame();
        }
    }

    /// The frame that just finished: measured elements it didn't paint
    /// (hidden, or in rows scrolled out of the list) that scripts still
    /// think are on screen. An IntersectionObserver must hear they're gone:
    /// a "load more" sentinel kept intersecting from its last rect, and
    /// YouTube loaded result after result.
    fn note_unpainted(&mut self, cx: &mut Context<Self>) {
        if self.page.as_ref().and_then(|p| p.scripts.as_ref()).is_none() {
            return;
        }
        // Only after a paint: a render with none since (GPUI can render more
        // than once a frame) saw nothing painted, and would report every
        // element gone, then back the next frame (observers flapped, and
        // YouTube's lazy images could miss the moment they were in view).
        let frame = self.bounds_frame.get();
        if frame == self.bounds_checked {
            return;
        }
        self.bounds_checked = frame;
        // The first paint: scripts hear the viewport even when nothing they
        // measure is on it (until a layout comes they get no answers about
        // where things are: `clientHeight`, `scrollTop` were 0).
        if self.layout_viewport_sent == (0.0, 0.0) && !self.layout_scheduled.get() {
            let size = self.list_state.viewport_bounds().size;
            let viewport = (f32::from(size.width), f32::from(size.height));
            if viewport.0 > 0.0 && viewport.1 > 0.0 {
                let mut store = self.measured_bounds.borrow_mut();
                if store.1 == (0.0, 0.0) {
                    store.1 = viewport;
                }
                drop(store);
                self.layout_scheduled.set(true);
                let tab = cx.entity().downgrade();
                cx.defer(move |cx| {
                    tab.update(cx, |this, _| this.send_layout()).ok();
                });
            }
        }
        let seen = self.bounds_seen.borrow();
        let now: HashSet<NodeId> = seen.iter().filter(|(_, f)| **f == frame).map(|(n, _)| *n).collect();
        drop(seen);
        // Scrolled: scripts hear it (`window.scrollY`, `scroll` events) even
        // when nothing they measure moved.
        let scroll = f32::from(-self.list_state.scroll_px_offset_for_scrollbar().y).max(0.0).round();
        if scroll != self.layout_scroll_sent.1 && !self.layout_scheduled.replace(true) {
            let tab = cx.entity().downgrade();
            cx.defer(move |cx| {
                tab.update(cx, |this, _| this.send_layout()).ok();
            });
        }
        if now.is_empty() && self.layout_painted.is_empty() && self.layout_keep.is_empty() {
            return;
        }
        let mut gone: Vec<NodeId> = self.layout_painted.difference(&now).copied().collect();
        // Measured elements in rows scrolled out of the list: about where
        // their row is (a jump link scrolling to a section asks where it is
        // before it was ever on screen). One in a row that's showing but
        // wasn't painted is hidden: gone.
        let approx = self.offscreen_rects(&now);
        if !approx.is_empty() {
            let mut store = self.measured_bounds.borrow_mut();
            for (n, r) in &approx {
                store.0.insert(*n, *r);
            }
            drop(store);
            gone.retain(|n| !approx.contains_key(n));
        }
        // Measured elements the layout has (or, rendering nothing, doesn't
        // have) a box for that this paint didn't paint: off screen. Said
        // once, so an IntersectionObserver's first entry comes from where
        // they are (a timer guessing "not painted yet, so not visible"
        // raced YouTube's lazy images, which then never loaded).
        for n in &now {
            self.unpainted_sent.remove(n);
        }
        let never: Vec<NodeId> = self
            .layout_keep
            .iter()
            .filter(|n| !now.contains(n) && !self.layout_painted.contains(n) && !self.unpainted_sent.contains(n) && !approx.contains_key(n))
            .copied()
            .collect();
        self.unpainted_sent.extend(never.iter().copied());
        gone.extend(never);
        // Painted again: the next layout message lists it among the rects
        // only if it moved, so make sure it does, and that there is one (in
        // the same place as before, nothing else sends one: YouTube's icons
        // and Shorts, drawn when an IntersectionObserver says they're on
        // screen, stayed blank until a scroll).
        let mut appeared = false;
        for n in now.difference(&self.layout_painted) {
            self.layout_sent.remove(n);
            appeared = true;
        }
        self.layout_painted = now;
        if gone.is_empty() && !appeared && approx.is_empty() {
            return;
        }
        self.layout_gone.extend(gone);
        if !self.layout_scheduled.replace(true) {
            let tab = cx.entity().downgrade();
            cx.defer(move |cx| {
                tab.update(cx, |this, _| this.send_layout()).ok();
            });
        }
    }

    /// Where `node`'s rows are in the page (x, y, width, height), from the
    /// rows' extents and indents: the first row it's in to the last.
    fn row_place(&self, node: NodeId) -> Option<[f32; 4]> {
        let layout = &self.page.as_ref()?.layout;
        let key = Arc::as_ptr(layout) as usize;
        let mut cache = self.sticky_rows.borrow_mut();
        if cache.0 != key {
            *cache = (key, HashMap::new());
        }
        let (first, last) = *match cache.1.entry(node) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => e.insert(row_range(layout, node)?),
        };
        drop(cache);
        let (top, _) = self.list_state.item_extent(first)?;
        let (last_top, last_h) = self.list_state.item_extent(last)?;
        let row = &layout.rows[first];
        // Each container's margin, border and padding (and offset) on the
        // way in, then the row's own.
        let side = |w: &render::boxes::Wrapper, left: bool| {
            let st = &w.style;
            let (m, p, b, i) = if left {
                (st.margin.left, st.padding.left, st.border_width.left, w.indent_left)
            } else {
                (st.margin.right, st.padding.right, st.border_width.right, w.indent_right)
            };
            m.px_or_zero() + p.px_or_zero() + b + i
        };
        let left: f32 = row.wrappers.iter().map(|w| side(w, true)).sum::<f32>() + row.indent_left;
        let right: f32 = row.wrappers.iter().map(|w| side(w, false)).sum::<f32>() + row.indent_right;
        let view = self.list_state.viewport_bounds();
        let x = f32::from(view.origin.x) + left;
        Some([x, f32::from(top), (f32::from(view.size.width) - left - right).max(0.0), f32::from(last_top + last_h - top)])
    }

    /// The page's sticky boxes.
    fn sticky_nodes(&self) -> Rc<HashSet<NodeId>> {
        let set = self.page.as_ref().map(|p| {
            p.layout
                .pinned
                .iter()
                .filter_map(|p| match p.pin {
                    render::boxes::Pin::Sticky { node, .. } => Some(node),
                    _ => None,
                })
                .collect()
        });
        Rc::new(set.unwrap_or_default())
    }

    /// The sticky boxes scrolled past their place, pinned: `top` px from
    /// the top of the view, pushed up by the end of their container.
    fn pinned_sticky(&self, rcx: &RenderCx) -> Vec<AnyElement> {
        let Some(page) = self.page.as_ref() else { return Vec::new() };
        let scroll = self.scroll_y();
        let list_top = f32::from(self.list_state.viewport_bounds().origin.y) - crate::CHROME_HEIGHT;
        let debug = std::env::var_os("FEATHER_DEBUG_STICKY").is_some();
        page.layout
            .pinned
            .iter()
            .filter_map(|p| {
                let render::boxes::Pin::Sticky { node, container, top } = p.pin else { return None };
                // Where its rows are (a sticky box isn't split, so it's
                // a row, or in one), and its container's.
                let [mut x, mut y, mut w, h] = self.row_place(node)?;
                // Where it was drawn in its place, if it has been: its own
                // column (a sticky sidebar in a grid isn't the row's width).
                if let Some(&[dx, page_y, dw, _]) = self.sticky_bounds.borrow().get(&node) {
                    (x, w, y) = (dx, dw, page_y);
                }
                let bottom = self.row_place(container).map_or(f32::INFINITY, |c| c[1] + c[3]);
                if debug {
                    eprintln!("sticky {node:?} in {container:?} top {top} at {y} to {bottom}, scroll {scroll}");
                }
                if scroll + top <= y {
                    return None;
                }
                let at = top.min(bottom - scroll - h);
                (at + h > 0.0).then(|| {
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(list_top + at))
                        .w(px(w))
                        .child(render::view::render_pinned(&p.content, rcx))
                        .into_any_element()
                })
            })
            .collect()
    }


    /// Canvases' new pixels: each shown as its picture from the next
    /// frame, the last one dropped from the GPU atlas.
    pub(super) fn canvas_frames(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let started = std::time::Instant::now();
        let redraw = self.canvas_frames_now(window);
        if let Some(p) = &mut self.perf {
            p.canvas += 1;
            p.canvas_time += started.elapsed();
        }
        if redraw {
            cx.notify();
        }
    }

    /// Whether the page needs rendering again (a frame of a new size, or a
    /// canvas new to the page); frames like the last are swapped into the
    /// GPU in place, and only presented.
    fn canvas_frames_now(&mut self, window: &mut Window) -> bool {
        let Some(host) = self.page.as_ref().and_then(|p| p.scripts.clone()) else { return false };
        let frames = std::mem::take(&mut *host.canvas_frames.lock().unwrap_or_else(|e| e.into_inner()));
        let mut redraw = false;
        for (node, mut c) in frames {
            if std::env::var_os("FEATHER_DEBUG_FRAMES").is_some() {
                let px = &c.pixels;
                let step = (px.len() / 4 / 64).max(1) * 4;
                let (sum, n) = px.chunks(4).step_by(step / 4).fold((0u64, 0u64), |(s, n), p| (s + p[0] as u64 + p[1] as u64 + p[2] as u64, n + 1));
                eprintln!("frame {node:?} {}x{} mean {}", c.width, c.height, sum / (3 * n.max(1)));
            }
            if !c.bgra {
                render::images::to_bgra(&mut c.pixels);
                c.bgra = true;
            }
            let same_size = |old: &Arc<gpui::RenderImage>| {
                old.size(0) == gpui::size(gpui::DevicePixels(c.width as i32), gpui::DevicePixels(c.height as i32))
            };
            if let Some(old) = self.canvas_frames.get(&node)
                && std::env::var_os("FEATHER_NO_INPLACE").is_none()
                && same_size(old)
                && window.update_image(old, 0, &c.pixels).unwrap_or(false)
            {
                continue;
            }
            redraw = true;
            let Ok(url) = Url::parse(&js::canvas_url(node)) else { continue };
            let Some((state, frame)) = render::images::from_pixels(c.width, c.height, c.pixels, true) else { continue };
            render::images::set_live(&frame, true);
            self.images.borrow_mut().insert(url, state);
            if let Some(old) = self.canvas_frames.insert(node, frame) {
                render::images::set_live(&old, false);
                window.drop_image(old).ok();
            }
        }
        // Canvases no longer in the page (replaced by new ones) go.
        if let Some(doc) = self.page.as_ref().and_then(|p| p.doc.as_ref()) {
            let gone: Vec<NodeId> = self.canvas_frames.keys().filter(|n| doc.get(**n).is_none()).copied().collect();
            for n in gone {
                if let Some(old) = self.canvas_frames.remove(&n) {
                    render::images::set_live(&old, false);
                    window.drop_image(old).ok();
                }
                if let Ok(url) = Url::parse(&js::canvas_url(n)) {
                    self.images.borrow_mut().remove(&url);
                }
            }
        }
        redraw
    }

    /// Lay out a few more rows the list hasn't measured yet, between
    /// frames, until all are: where things below the view are (for scripts
    /// asking, `scrollTo` targets, the scrollbar) is then known, not
    /// guessed from an average row, as in a browser.
    fn measure_ahead(&mut self, window: &mut Window) {
        if self.page.is_none() || self.scroll_animation.is_some() || self.scrolling_until.is_some_and(|t| t > std::time::Instant::now()) {
            return;
        }
        if let Some(first) = self.list_state.first_unmeasured() {
            self.list_state.measure_through(first + 12);
            window.request_animation_frame();
        }
    }

    /// The row `node` is in, or its first laid-out descendant's (an element
    /// without a box of its own).
    fn row_of(&self, node: NodeId) -> Option<usize> {
        let page = self.page.as_ref()?;
        let mut wanted: HashSet<NodeId> = HashSet::from([node]);
        if let Some(doc) = page.doc.as_ref() {
            wanted.extend(doc.descendants(node).take(256));
        }
        rows_of(&page.layout, &wanted).into_values().min()
    }

    /// Where `node` is for a script asking (viewport rect): where it was
    /// painted, else where its row is.
    pub(super) fn locate(&mut self, node: NodeId) -> Option<[f32; 4]> {
        if std::env::var_os("FEATHER_DEBUG_LOCATE").is_some() {
            eprintln!(
                "locate {node:?}: rect {:?} painted {} kept {} installed doc {}",
                self.measured_bounds.borrow().0.get(&node),
                self.layout_painted.contains(&node),
                self.layout_keep.contains(&node),
                self.installed_gen
            );
        }
        if self.layout_painted.contains(&node)
            && let Some(r) = self.measured_bounds.borrow().0.get(&node)
        {
            return Some(*r);
        }
        let ix = self.row_of(node)?;
        let (top, h) = self.list_state.item_extent(ix)?;
        let view = self.list_state.viewport_bounds();
        let scroll = -self.list_state.scroll_px_offset_for_scrollbar().y;
        let y = f32::from(view.origin.y) - crate::CHROME_HEIGHT + f32::from(top - scroll);
        // Its row as a stand-in says where an element scrolled out of view
        // is (a jump link's target). In a row that's showing, it's just not
        // painted yet (or hidden): no answer, rather than the row's width
        // (GitHub's breadcrumbs, each "as wide as the page", collapsed).
        let shown = y < f32::from(view.size.height) && y + f32::from(h) > 0.0;
        if shown {
            return None;
        }
        Some([f32::from(view.origin.x), y, f32::from(view.size.width), f32::from(h)])
    }

    /// The page's scroll position, px from the top.
    fn scroll_y(&self) -> f32 {
        f32::from(-self.list_state.scroll_px_offset_for_scrollbar().y).max(0.0)
    }

    /// Scroll the page to `y` px (by row: the list's own pixel offsets stop
    /// at the rows it has measured).
    fn scroll_page_to(&self, y: f32) {
        let offset = self.list_state.offset_for_y(px(y.max(0.0)));
        self.list_state.scroll_to(offset);
    }

    /// Scripts scrolled the page (`window.scrollTo`): there at once, or
    /// animated for `behavior: "smooth"`.
    pub(super) fn script_scroll(&mut self, y: f32, smooth: bool, cx: &mut Context<Self>) {
        if smooth {
            // (Its frames are asked for from render.)
            self.scroll_animation = Some((self.scroll_y(), y, std::time::Instant::now()));
        } else {
            self.scroll_animation = None;
            self.scroll_page_to(y);
        }
        cx.notify();
    }

    /// A frame of a smooth scroll (ease-in-out, 350ms).
    fn step_scroll_animation(&mut self, window: &mut Window) {
        let Some((from, to, started)) = self.scroll_animation else { return };
        let t = (started.elapsed().as_secs_f32() / 0.35).min(1.0);
        let eased = if t < 0.5 { 2.0 * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(2) / 2.0 };
        self.scroll_page_to(from + (to - from) * eased);
        if t < 1.0 {
            window.request_animation_frame();
        } else {
            self.scroll_animation = None;
        }
    }

    /// Approximate rects (window coordinates, as painted ones are) for the
    /// measured elements not painted this frame whose row is out of view:
    /// the row's box, from the heights the list has measured.
    fn offscreen_rects(&mut self, painted: &HashSet<NodeId>) -> HashMap<NodeId, [f32; 4]> {
        let Some(page) = self.page.as_ref() else { return HashMap::new() };
        let layout = page.layout.clone();
        let key = (Arc::as_ptr(&layout) as usize, self.layout_keep.len());
        if self.measured_rows.as_ref().is_none_or(|(p, n, _)| (*p, *n) != key) {
            self.measured_rows = Some((key.0, key.1, rows_of(&layout, &self.layout_keep)));
        }
        let rows = &self.measured_rows.as_ref().expect("just set").2;
        let view = self.list_state.viewport_bounds();
        let scroll = -self.list_state.scroll_px_offset_for_scrollbar().y;
        let (left, width, height) = (f32::from(view.origin.x), f32::from(view.size.width), f32::from(view.size.height));
        let list_top = f32::from(view.origin.y) - crate::CHROME_HEIGHT;
        let mut out = HashMap::new();
        for (&node, &ix) in rows {
            if painted.contains(&node) {
                continue;
            }
            let Some((top, h)) = self.list_state.item_extent(ix) else { continue };
            let y = f32::from(top - scroll);
            let h = f32::from(h);
            // A row in view was rendered: an element in it not painted is hidden.
            if y + h > 0.0 && y < height {
                continue;
            }
            out.insert(node, [left, list_top + y, width, h]);
        }
        out
    }

    /// Tell the page's scripts where the elements they measure were
    /// painted (what changed since last time).
    fn send_layout(&mut self) {
        self.layout_scheduled.set(false);
        let Some(host) = self.page.as_ref().and_then(|p| p.scripts.clone()) else { return };
        let (rects, viewport) = {
            let store = self.measured_bounds.borrow();
            let changed: Vec<(NodeId, [f32; 4])> =
                store.0.iter().filter(|(n, r)| self.layout_sent.get(n) != Some(r)).map(|(n, r)| (*n, *r)).collect();
            (changed, store.1)
        };
        let scroll = (0.0, f32::from(-self.list_state.scroll_px_offset_for_scrollbar().y).max(0.0).round());
        if rects.is_empty() && self.layout_gone.is_empty() && viewport == self.layout_viewport_sent && scroll == self.layout_scroll_sent {
            return;
        }
        for (n, r) in &rects {
            self.layout_sent.insert(*n, *r);
        }
        self.layout_viewport_sent = viewport;
        self.layout_scroll_sent = scroll;
        let gone = std::mem::take(&mut self.layout_gone);
        if std::env::var_os("FEATHER_DEBUG_IO").is_some()
            && let Some(doc) = self.page.as_ref().and_then(|p| p.doc.as_ref())
        {
            let img = |n: &NodeId| doc.get(*n).is_some() && doc.element(*n).is_some_and(|e| &**e.local_name() == "img");
            let measured: Vec<NodeId> = host.measured.read().map(|m| m.iter().copied().filter(img).collect()).unwrap_or_default();
            let store = self.measured_bounds.borrow();
            let with_rect = measured.iter().filter(|n| store.0.contains_key(n)).count();
            eprintln!(
                "layout: {} rects ({} img), {} gone; measured imgs {} with rect {}",
                rects.len(),
                rects.iter().filter(|(n, _)| img(n)).count(),
                gone.len(),
                measured.len(),
                with_rect
            );
        }
        host.send(js::ToJs::Layout { rects, gone, viewport, scroll });
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
                        let fallback = families.first().map(String::as_str);
                        let decoded = bytes.as_ref().map(|b| crate::fonts::decode_all(b.clone(), weights, fallback)).unwrap_or_default();
                        if std::env::var_os("BROWSER_DEBUG").is_some() {
                            let names: Vec<&str> = decoded.iter().map(|(_, f)| f.as_str()).collect();
                            eprintln!("web font {src}: {:?} bytes -> {} fonts {names:?}", bytes.as_ref().map(|b| b.len()).map_err(|e| e.to_string()), decoded.len());
                        }
                        Some((src, families, decoded))
                    })
                })
                .collect();
            let results = futures::future::join_all(tasks).await;
            this.update(cx, |this, cx| {
                let debug = std::env::var_os("BROWSER_DEBUG").is_some();
                let mut any = false;
                for (src, families, decoded) in results.into_iter().flatten() {
                    let family = decoded.first().map(|(_, f)| f.clone());
                    // One file at a time, and a panic in the platform's font
                    // code (CoreText finding no family name in a file we could
                    // read) only loses that file. Such files get a second go
                    // with names written afresh.
                    let add = |faces: Vec<Vec<u8>>| -> bool {
                        let faces: Vec<_> = faces.into_iter().map(std::borrow::Cow::Owned).collect();
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cx.text_system().add_fonts(faces))) {
                            Ok(Ok(())) => true,
                            Ok(Err(err)) => {
                                if debug {
                                    eprintln!("web font {src} rejected: {err:#}");
                                }
                                false
                            }
                            Err(_) => false,
                        }
                    };
                    let data: Vec<Vec<u8>> = decoded.iter().map(|(d, _)| d.clone()).collect();
                    let added = !data.is_empty()
                        && (add(data)
                            || add(decoded.iter().filter_map(|(d, f)| crate::fonts::renamed(d, f)).collect()));
                    if !added && !decoded.is_empty() {
                        eprintln!("web font {src} couldn't be loaded");
                    }
                    if added && let Some(family) = &family {
                        for css in families {
                            this.palette.fonts.add(&css, family);
                        }
                        any = true;
                    }
                    cx.global_mut::<crate::fonts::WebFonts>().loaded.insert(src, family.filter(|_| added));
                }
                if any {
                    if debug {
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

    /// Images queued or being fetched.
    fn images_pending(&self) -> usize {
        self.image_queue.borrow().len() + self.images_in_flight.get()
    }

    /// Queue the page's images that aren't loaded or loading yet, the ones
    /// on screen (and fixed ones) first, and keep the loader running; each
    /// image re-measures the rows showing it when it arrives.
    ///
    /// One loader runs for the tab: a layout update (pages change their DOM
    /// all through loading) only adds to its queue. Restarting it per update
    /// dropped the fetches in flight and started over from the top of the
    /// document, so images further down (YouTube's Shorts shelf) waited
    /// until the page went quiet.
    fn load_images(&mut self, cx: &mut Context<Self>) {
        let Some(page) = &self.page else { return };
        let top = self.list_state.logical_scroll_top().item_ix;
        let near = top.saturating_sub(4)..top + 40;
        {
            let mut store = self.images.borrow_mut();
            let mut queue = self.image_queue.borrow_mut();
            let mut soon: Vec<Url> = Vec::new();
            let mut later: Vec<Url> = Vec::new();
            let wanted = page
                .layout
                .fixed_images
                .iter()
                .map(|u| (u, true))
                .chain(page.layout.images.iter().map(|(u, rows)| (u, rows.iter().any(|r| near.contains(r)))))
                .take(MAX_IMAGES)
                // Canvases' pixels come from their scripts, not the network.
                .filter(|(url, _)| url.scheme() != "feather-canvas")
                .filter(|(url, _)| loader::may_load(&page.url, url));
            for (url, visible) in wanted {
                match store.get(&**url) {
                    // A small `data:` image (an inline SVG icon, drawn from
                    // its markup): decoded now, so the layout that has it
                    // shows it from its first frame (a framework re-rendering
                    // its icons made them blink out for a frame).
                    None if url.scheme() == "data" && url.as_str().len() <= 16 * 1024 => {
                        let state = net::fetch_bytes(url)
                            .and_then(render::images::prepare)
                            .map_or(ImageState::Failed, |p| p.into_state());
                        store.insert((**url).clone(), state);
                    }
                    None => {
                        store.insert((**url).clone(), ImageState::Loading);
                        if visible { soon.push((**url).clone()) } else { later.push((**url).clone()) }
                    }
                    // Queued already: move up if it's on screen now.
                    Some(ImageState::Loading) if visible => {
                        if let Some(at) = queue.iter().position(|q| q == &**url) {
                            if let Some(q) = queue.remove(at) {
                                soon.push(q);
                            }
                        }
                    }
                    _ => {}
                }
            }
            for url in soon.into_iter().rev() {
                queue.push_front(url);
            }
            queue.extend(later);
        }
        if self.image_loader_running.get() || self.image_queue.borrow().is_empty() {
            return;
        }
        self.image_loader_running.set(true);
        let (queue, in_flight_count, running) = (self.image_queue.clone(), self.images_in_flight.clone(), self.image_loader_running.clone());
        self._image_task = Some(cx.spawn(async move |this, cx| {
            let start = |cx: &mut AsyncApp, url: Url| {
                let task = cx.background_executor().spawn({
                    let url = url.clone();
                    async move { net::fetch_bytes(&url).and_then(render::images::prepare) }
                });
                async move { (url, task.await) }
            };
            let mut in_flight = FuturesUnordered::new();
            loop {
                while in_flight.len() < IMAGE_CONCURRENCY {
                    let Some(url) = queue.borrow_mut().pop_front() else { break };
                    in_flight_count.set(in_flight_count.get() + 1);
                    in_flight.push(start(cx, url));
                }
                let Some((url, result)) = in_flight.next().await else { break };
                in_flight_count.set(in_flight_count.get().saturating_sub(1));
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
                    if debug {
                        eprintln!("image ready: {} rows={rows:?}", url.as_str().chars().take(70).collect::<String>());
                    }
                    this.images.borrow_mut().insert(url, state);
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
            running.set(false);
        }));
    }

    /// The window was resized: if a media query now evaluates differently,
    /// restyle and re-lay-out the page (debounced, in the background).
    pub fn viewport_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env(window);
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
        let env = self.env(window);
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
        let signature = page.styles.as_ref()?.signature(&self.env(window));
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
        // One building at a time: replacing it would starve a big page whose
        // scripts send DOM updates faster than it restyles. The next one
        // starts (with the newest DOM) when this lands.
        if self.restyle_busy {
            self.restyle_queued = true;
            return;
        }
        let Some(page) = &self.page else { return };
        if page.doc.is_none() || page.styles.is_none() {
            return;
        }
        let page_gen = self.page_gen;
        self._restyle = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(delay).await;
            // The newest DOM and styles, as of now.
            let Ok(Some((doc, styles, doc_gen))) = this.update(cx, |this, _| {
                this.restyle_busy = true;
                let page = this.page.as_ref()?;
                Some((page.doc.clone()?, page.styles.clone()?, this.doc_gen))
            }) else {
                this.update(cx, |this, _| this.restyle_busy = false).ok();
                return;
            };
            let Ok(env) = this.update_in(cx, |this, window, _| this.env(window)) else { return };
            let Ok(state) = this.update(cx, |this, _| Arc::new(this.current_state())) else { return };
            let signature = styles.signature(&env);
            let Ok(old) = this.update(cx, |this, _| this.page.as_ref().map(|p| p.layout.clone())) else { return };
            // Elements scripts measure keep a box of their own.
            let Ok(keep) = this.update(cx, |this, _| {
                this.page.as_ref().and_then(|p| p.scripts.as_ref()).map(|h| h.measured.read().map(|m| m.clone()).unwrap_or_default()).unwrap_or_default()
            }) else { return };
            let kept = keep.clone();
            if let Ok(Some(())) = this.update(cx, |this, _| this.perf.as_mut().map(|p| p.restyles += 1)) {}
            let Ok(cache) = this.update(cx, |this, _| this.style_cache.clone()) else { return };
            let (layout, changed, ends) = loader::deep({
                let state = state.clone();
                move || {
                    let t = std::time::Instant::now();
                    let session = cache.lock().unwrap_or_else(|e| e.into_inner()).begin(&styles, &env, doc.clone(), state.clone());
                    let (mut layout, session) = render::build_cached(&doc, &styles.styler_with_state(env, state), &keep, Some(session));
                    let built = t.elapsed();
                    let (reused, computed, why) = session.as_ref().map_or((0, 0, ""), |s| (s.reused, s.computed, s.why));
                    if let Some(session) = session {
                        cache.lock().unwrap_or_else(|e| e.into_inner()).finish(session);
                    }
                    // Same rows: which changed. Rows added or removed: how
                    // many at either end didn't.
                    let (changed, ends) = match old {
                        Some(old) if old.rows.len() == layout.rows.len() => (layout.reuse_unchanged(&old), None),
                        Some(old) => (Vec::new(), Some(layout.reuse_ends(&old))),
                        None => (Vec::new(), None),
                    };
                    if std::env::var_os("BROWSER_DEBUG").is_some() {
                        eprintln!("forced: built doc {doc_gen} at {:?}", crate::started().elapsed());
                        eprintln!("perf: restyle+build {} rows in {:?} (style {:?}, {reused} reused, {computed} computed {why}; +{:?} diffing, ends {ends:?})", layout.rows.len(), built, Duration::from_nanos(render::boxes::STYLE_NS.with(|c| c.get())), t.elapsed() - built);
                    }
                    (layout, changed, ends)
                }
            })
            .await;
            this.update_in(cx, |this, window, cx| {
                this.restyle_busy = false;
                if this.page_gen != page_gen {
                    return;
                }
                // Built for a hover state the pointer has left already, with
                // no newer DOM in it: shown, the old hover would flash for a
                // frame before the next restyle (YouTube's controls blinked
                // as the pointer crossed them). Build again for now instead,
                // a few times at most (the page still updates while the
                // pointer keeps moving).
                if this.installed_gen == doc_gen && this.current_state() != *state && this.stale_skips < 4 {
                    this.stale_skips += 1;
                    this.restyle_queued = false;
                    this.restyle(Duration::ZERO, window, cx);
                    return;
                }
                this.stale_skips = 0;
                let again = std::mem::take(&mut this.restyle_queued);
                let Some(page) = &mut this.page else { return };
                let same_shape = page.layout.rows.len() == layout.rows.len();
                // Where the view is, by what's there (row numbers mean other
                // things once rows come and go).
                let top = this.list_state.logical_scroll_top();
                let anchor = page.layout.rows.get(top.item_ix).map(|r| r.anchors()).unwrap_or_default();
                let anchored = (!same_shape)
                    .then(|| {
                        anchor.iter().enumerate().find_map(|(depth, node)| {
                            let ix = layout.rows.iter().position(|r| r.anchors().first() == Some(node))?;
                            // The same row: same offset into it. A container it
                            // was in: its start.
                            Some(ListOffset { item_ix: ix, offset_in_item: if depth == 0 { top.offset_in_item } else { px(0.) } })
                        })
                    })
                    .flatten();
                let old_rows = page.layout.rows.len();
                page.layout = Arc::new(layout);
                page.signature = signature;
                this.installed_gen = doc_gen;
                this.installed_frame = this.bounds_frame.get();
                this.layout_keep = kept;
                this.element_state = state;
                if same_shape {
                    // Usually a hover changed a color in a row or two: re-measure
                    // just those (everything else kept its cached layout).
                    for ix in changed {
                        this.list_state.remeasure_items(ix..ix + 1);
                    }
                } else if let Some((prefix, suffix)) = ends
                    && (prefix > 0 || suffix > 0)
                {
                    // Rows came or went between unchanged ones: just those
                    // are laid out anew; the rest keep their heights (no
                    // jump, no flash of re-estimated rows).
                    let rows = page.layout.rows.len();
                    this.list_state.splice(prefix..old_rows - suffix, rows - prefix - suffix);
                    // The view's top was in what changed: find its place
                    // again (else the list kept it).
                    if (prefix..old_rows - suffix).contains(&top.item_ix)
                        && let Some(to) = anchored
                    {
                        this.list_state.scroll_to(to);
                    }
                } else {
                    // Keep the same place in the page: the row with the same
                    // element, else the same row number from its start (an
                    // offset into some other row could land past its end,
                    // a blank screen until the next layout).
                    let rows = page.layout.rows.len();
                    this.reset_rows(rows);
                    let to = anchored.unwrap_or(ListOffset {
                        item_ix: top.item_ix.min(rows.saturating_sub(1)),
                        offset_in_item: if top.item_ix == 0 { top.offset_in_item } else { px(0.) },
                    });
                    this.list_state.scroll_to(to);
                }
                this.load_images(cx);
                this.sync_keyframes(window);
                cx.notify();
                // The pointer moved on while this was building: catch up.
                this.interaction_changed(window, cx);
                // The DOM changed again meanwhile.
                if again {
                    this.restyle(Duration::ZERO, window, cx);
                }
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
                    let pending = this.update(cx, |this, _| this.images_pending()).unwrap_or(0);
                    if pending == 0 {
                        break;
                    }
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                }
            };
            // `--frames`: what the page does over time, from when it shows
            // (or, with `--click`, from the click and keys on).
            let record_frames = async |cx: &mut AsyncWindowContext| {
                for frame in 0..job.frames {
                    let path = job.frame_path(frame);
                    // With --mouse: the pointer moves along the points over
                    // the frames (hover effects, as a window gets them).
                    if job.mouse.len() >= 2 {
                        let t = frame as f32 / (job.frames.max(2) - 1) as f32 * (job.mouse.len() - 1) as f32;
                        let i = (t.floor() as usize).min(job.mouse.len() - 2);
                        let f = t - i as f32;
                        let (a, b) = (job.mouse[i], job.mouse[i + 1]);
                        let position = point(px(a.0 + (b.0 - a.0) * f), px(a.1 + (b.1 - a.1) * f));
                        cx.update(|window, cx| {
                            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }), cx);
                        })
                        .ok();
                    }
                    if job.frame_scroll != 0.0 {
                        this.update(cx, |this, cx| {
                            this.list_state.scroll_by(px(job.frame_scroll));
                            cx.notify();
                        })
                        .ok();
                    }
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
            };
            if job.frames > 0 && job.click.is_none() && job.mouse.len() < 2 {
                record_frames(cx).await;
                return;
            }
            if job.wait > 0 {
                // Keep drawing meanwhile, as a window on screen would: scripts
                // measuring the page hear about layout only from drawn frames.
                let until = std::time::Instant::now() + Duration::from_millis(job.wait);
                // (FEATHER_FRAME_MS=16 draws at a display's rate: pages that
                // react to layout can differ from the default's 100ms.)
                let every = std::env::var("FEATHER_FRAME_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
                let perf = std::env::var_os("FEATHER_PERF").is_some();
                let (mut n, mut total, mut worst, mut since) = (0u32, Duration::ZERO, Duration::ZERO, std::time::Instant::now());
                while std::time::Instant::now() < until {
                    let t = std::time::Instant::now();
                    cx.update(|window, cx| window.draw(cx).clear(cx)).ok();
                    if perf {
                        let d = t.elapsed();
                        (n, total, worst) = (n + 1, total + d, worst.max(d));
                        if since.elapsed() >= Duration::from_secs(1) {
                            eprintln!("perf/s: {n} draws, {:?} avg, {worst:?} worst, {} boxes a draw", total / n.max(1), render::view::take_rendered() / n.max(1) as usize);
                            (n, total, worst, since) = (0, Duration::ZERO, Duration::ZERO, std::time::Instant::now());
                        }
                    }
                    cx.background_executor().timer(Duration::from_millis(every)).await;
                }
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
            if job.frames > 0 && job.mouse.len() >= 2 && job.click.is_none() {
                record_frames(cx).await;
                return;
            }
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
                if job.frames > 0 {
                    record_frames(cx).await;
                    return;
                }
                // Whatever it did (restyle, navigation) gets time to finish,
                // drawing meanwhile as a window would (a video it started
                // plays).
                let every = std::env::var("FEATHER_FRAME_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
                let until = std::time::Instant::now() + Duration::from_millis(2500);
                while std::time::Instant::now() < until {
                    cx.update(|window, cx| window.draw(cx).clear(cx)).ok();
                    cx.background_executor().timer(Duration::from_millis(every)).await;
                }
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
                // refreshes), several times: GPUI decodes images on first use, and
                // nested inline-blocks settle their sizes a level per frame.
                cx.update(|window, cx| window.draw(cx).clear(cx)).ok();
                cx.background_executor().timer(Duration::from_millis(400)).await;
                for _ in 0..6 {
                    cx.update(|window, cx| window.draw(cx).clear(cx)).ok();
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                }
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

    /// Give each `<iframe>` in the document a tab showing its `src`, drop
    /// the ones gone (or pointed elsewhere).
    pub(super) fn sync_frames(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::env::var_os("FEATHER_NO_FRAMES").is_some() {
            return;
        }
        let Some(page) = &self.page else { return };
        let Some(doc) = page.doc.clone() else { return };
        let base = page.url.clone();
        let mut wanted: HashMap<NodeId, Url> = HashMap::new();
        for n in doc.descendants(doc.root()) {
            let Some(e) = doc.element(n) else { continue };
            if &*e.name.local != "iframe" {
                continue;
            }
            if std::env::var_os("BROWSER_DEBUG").is_some() {
                eprintln!("frame {n:?}: src {:?}", e.attr("src").map(|s| s.chars().take(100).collect::<String>()));
            }
            let Some(url) = e.attr("src").map(str::trim).filter(|s| !s.is_empty()).and_then(|s| base.join(s).ok()) else { continue };
            // (A local page's local frames too.)
            if matches!(url.scheme(), "http" | "https") || (url.scheme() == "file" && base.scheme() == "file") {
                wanted.insert(n, url);
            }
        }
        let before = self.frames.len();
        self.frames.retain(|n, (url, _)| wanted.get(n) == Some(url));
        let mut changed = self.frames.len() != before;
        for (node, url) in wanted {
            if self.frames.contains_key(&node) {
                continue;
            }
            let parent = cx.entity().downgrade();
            let palette = self.palette.clone();
            let child = cx.new(|cx| {
                let mut tab = Tab::new(palette, cx);
                tab.frame = Some((parent, node));
                tab
            });
            child.update(cx, |tab, cx| tab.script_navigate(url.clone(), window, cx));
            self.frames.insert(node, (url, child));
            changed = true;
        }
        if changed {
            self.embeds = Rc::new(self.frames.iter().map(|(n, (_, t))| (*n, AnyView::from(t.clone()))).collect());
            cx.notify();
        }
    }

    /// A frame's `postMessage`: to one of this page's `<iframe>`s, or to
    /// the page this one is framed in.
    pub(super) fn post_message(&mut self, to_frame: Option<NodeId>, data: String, origin: String, target_origin: String, cx: &mut Context<Self>) {
        if std::env::var_os("BROWSER_DEBUG").is_some() {
            eprintln!("postMessage to {} from {origin} (for {target_origin}): {}", if to_frame.is_some() { "frame" } else { "parent" }, data.chars().take(160).collect::<String>());
        }
        let deliver = move |tab: &mut Tab, from_frame: Option<NodeId>| {
            let Some(page) = &tab.page else { return };
            let theirs = page.url.origin().ascii_serialization();
            if target_origin != "*" && target_origin.trim_end_matches('/') != theirs {
                return;
            }
            if let Some(host) = &page.scripts {
                host.send(js::ToJs::Message { data, origin, from_frame });
            }
        };
        match to_frame {
            Some(node) => {
                if let Some((_, child)) = self.frames.get(&node) {
                    child.update(cx, |tab, _| deliver(tab, None));
                }
            }
            None => {
                if let Some((parent, node)) = self.frame.clone() {
                    parent.update(cx, |tab, _| deliver(tab, Some(node))).ok();
                }
            }
        }
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
            cull: self.cull.clone(),
            embeds: self.embeds.clone(),
            controls: Rc::new(move |control, style, window, cx| {
                controls
                    .update(cx, |this, cx| this.control_element(control, style, window, cx))
                    .unwrap_or_else(|_| div().into_any_element())
            }),
            palette: self.palette.clone(),
            images: self.images.clone(),
            shown_images: self.shown_images.clone(),
            images_generation: self.images_generation,
            round_clip: None,
            stacking: (0, 0),
            fonts_generation: self.fonts_generation,
            track_hover,
            wants_bounds: {
                let measured = self.page.as_ref().and_then(|p| p.scripts.as_ref()).map(|h| h.measured.clone());
                let sticky = self.sticky_nodes();
                Rc::new(move |node| sticky.contains(&node) || measured.as_ref().is_some_and(|m| m.read().is_ok_and(|m| m.contains(&node))))
            },
            on_bounds: {
                let (store, scheduled, tab) = (self.measured_bounds.clone(), self.layout_scheduled.clone(), cx.entity().downgrade());
                let (frame, seen) = (self.bounds_frame.clone(), self.bounds_seen.clone());
                let measured = self.page.as_ref().and_then(|p| p.scripts.as_ref()).map(|h| h.measured.clone());
                // (Where the page is scrolled, read now: the list can't be
                // asked while it's laying out, when bounds are reported.)
                let scroll = self.scroll_y();
                let list_top = f32::from(self.list_state.viewport_bounds().origin.y) - crate::CHROME_HEIGHT;
                let (sticky, sticky_store) = (self.sticky_nodes(), self.sticky_bounds.clone());
                Rc::new(move |node, bounds, window, cx| {
                    let mut rect = [
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y) - crate::CHROME_HEIGHT,
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    ];
                    // A sticky box in its place: where its pinned copy goes.
                    if sticky.contains(&node) {
                        // (In the page: its row may not be drawn later.)
                        sticky_store.borrow_mut().insert(node, [rect[0], rect[1] - list_top + scroll, rect[2], rect[3]]);
                        if !measured.as_ref().is_some_and(|m| m.read().is_ok_and(|m| m.contains(&node))) {
                            return;
                        }
                    }
                    // Another slice of the same box this frame: the union.
                    let now = frame.get();
                    if seen.borrow_mut().insert(node, now) == Some(now)
                        && let Some(&[x, y, w, h]) = store.borrow().0.get(&node)
                    {
                        let (x0, y0) = (x.min(rect[0]), y.min(rect[1]));
                        let (x1, y1) = ((x + w).max(rect[0] + rect[2]), (y + h).max(rect[1] + rect[3]));
                        rect = [x0, y0, x1 - x0, y1 - y0];
                    }
                    let viewport = window.viewport_size();
                    let viewport = (f32::from(viewport.width), f32::from(viewport.height) - crate::CHROME_HEIGHT);
                    let mut store = store.borrow_mut();
                    store.1 = viewport;
                    if store.0.get(&node) != Some(&rect) {
                        store.0.insert(node, rect);
                        if !scheduled.replace(true) {
                            let tab = tab.clone();
                            cx.defer(move |cx| {
                                tab.update(cx, |this, _| this.send_layout()).ok();
                            });
                        }
                    }
                })
            },
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
        // (Pinned boxes animate too: their frames are in it.)
        let shown = layout.clone();
        if let Some(cull) = &self.cull {
            // (Window coordinates: down to the list's bottom.)
            let viewport = self.list_state.viewport_bounds();
            let viewport = gpui_kit::size(viewport.size.width, viewport.origin.y + viewport.size.height);
            cull.begin(render::cull::CullKey {
                // (Not the in-between frames of animations: the same
                // boxes, mostly moved by transforms.)
                layout: Arc::as_ptr(&page.layout) as usize,
                images: self.images_generation,
                fonts: self.fonts_generation,
                viewport: (f32::from(viewport.width), f32::from(viewport.height)),
            });
        }
        let rcx = self.render_cx(cx);
        let rcx_fixed = rcx.clone();
        // (The pinned copies don't tell where the sticky boxes are: their
        // places in the page do.)
        let mut rcx_sticky = rcx.clone();
        let (sticky, wants) = (self.sticky_nodes(), rcx.wants_bounds.clone());
        rcx_sticky.wants_bounds = Rc::new(move |node| !sticky.contains(&node) && wants(node));
        let last = layout.rows.len().saturating_sub(1);
        let _ = palette;
        div()
            .size_full()
            // A content mask for the page: pinned and fixed boxes draw in
            // the deferred layer, clipped to it (a sticky header pushed up
            // by its container drew over the tab strip).
            .overflow_hidden()
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
            // Absolute boxes anchored to the page: in the initial containing
            // block (the viewport's size, at the top of the page), so they
            // scroll with it.
            .when(shown.pinned.iter().any(|p| p.is_page()), |d| {
                let list_top = f32::from(self.list_state.viewport_bounds().origin.y) - crate::CHROME_HEIGHT;
                d.child(
                    div()
                        .absolute()
                        .top(px(list_top - self.scroll_y()))
                        .left_0()
                        .w_full()
                        .h(self.list_state.viewport_bounds().size.height)
                        .children(shown.pinned.iter().filter(|p| p.is_page()).map(|p| render::render_fixed(&p.content, &rcx_fixed))),
                )
            })
            // `position: fixed` boxes, pinned over the viewport.
            .children(self.pinned_sticky(&rcx_sticky))
            .when(shown.pinned.iter().any(|p| p.is_fixed()), |d| {
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .children(shown.pinned.iter().filter(|p| p.is_fixed()).map(|p| render::render_fixed(&p.content, &rcx_fixed))),
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
        rcx.cull = None;
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

/// Frame statistics (FEATHER_PERF): how often the tab renders and what it
/// spends, per second.
#[derive(Default)]
pub(super) struct Perf {
    since: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
    renders: u32,
    max_gap: Duration,
    render_time: Duration,
    canvas: u32,
    canvas_time: Duration,
    restyles: u32,
}

impl Perf {
    fn frame(&mut self, render_time: Duration) {
        let now = std::time::Instant::now();
        if let Some(last) = self.last.replace(now) {
            self.max_gap = self.max_gap.max(now - last);
        }
        self.renders += 1;
        self.render_time += render_time;
        let since = *self.since.get_or_insert(now);
        if now - since >= Duration::from_secs(1) {
            eprintln!(
                "perf/s: {} renders (max gap {:?}, render {:?}), {} canvas uploads ({:?}), {} restyles",
                self.renders, self.max_gap, self.render_time, self.canvas, self.canvas_time, self.restyles
            );
            *self = Perf { last: self.last, since: Some(now), ..Perf::default() };
        }
    }
}

impl Render for Tab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = std::time::Instant::now();
        let out = self.render_tab(window, cx);
        let spent = started.elapsed();
        if let Some(p) = &mut self.perf {
            p.frame(spent);
        }
        out
    }
}

impl Tab {
    fn render_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        render::view::begin_stacking();
        if std::env::var_os("BROWSER_DEBUG").is_some() {
            let top = self.list_state.logical_scroll_top();
            let now = (top.item_ix, f32::from(top.offset_in_item), self.list_state.item_count());
            if self.debug_scroll_top.replace(Some(now)) != Some(now) {
                eprintln!("scroll: row {} +{:.0}px of {} rows", now.0, now.1, now.2);
            }
        }
        self.note_unpainted(cx);
        self.answer_after_layout(window);
        // A frame drawn at a new size: its media queries see it.
        if self.frame.is_some() {
            let size = self.list_state.viewport_bounds().size;
            if size != self.frame_size && size.width > px(0.) {
                self.frame_size = size;
                cx.defer_in(window, |this, window, cx| this.viewport_changed(window, cx));
            }
        }
        self.atlas_drops.extend(render::images::take_evicted());
        for old in self.atlas_drops.drain(..) {
            window.drop_image(old).ok();
        }
        self.measure_ahead(window);
        self.step_scroll_animation(window);
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
            // A frame's document shows the page around it through itself
            // unless it has a background (a consent dialog over BBC).
            .unwrap_or(if self.frame.is_some() { gpui::transparent_black() } else { palette.canvas });
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

        let frame = self.bounds_frame.clone();
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .bg(canvas)
            // Painted before the page: a new frame for the bounds it reports.
            .child(gpui::canvas(move |_, _, _| frame.set(frame.get() + 1), |_, _, _, _| {}).absolute().size_0())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    // A click in a text field focuses the field (it handled the
                    // event first). Anywhere else, focus leaves the field for
                    // the page, as in browsers: scripts hear `blur` (YouTube
                    // closes its suggestions on it).
                    // (`mousedown` reaches scripts first, as in browsers.)
                    this.mouse_down = true;
                    this.presses += 1;
                    if let Some(url) = this.page.as_ref().map(|p| &p.url) {
                        loader::activate_origin(url);
                    }
                    this.script_mouse("mousedown", event.position, 0, (0.0, 0.0));
                    if this.forms.clicked_field.take().is_none() {
                        if this.forms.focused.is_some() {
                            this.script_focus_lost(window, cx);
                        }
                        this.focus_handle.focus(window, cx);
                    }
                    this.interaction_changed(window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    this.mouse_down = false;
                    this.script_mouse("mouseup", event.position, 0, (0.0, 0.0));
                    if event.click_count == 2 {
                        this.script_mouse("dblclick", event.position, 0, (0.0, 0.0));
                    }
                    this.script_mouse_up(window, cx);
                    this.interaction_changed(window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, _| {
                this.script_mouse("mousemove", event.position, 0, (0.0, 0.0));
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.script_key("keydown", &event.keystroke, window, cx)))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| this.script_key("keyup", &event.keystroke, window, cx)))
            .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                // The reader scrolling takes over from a script's smooth scroll.
                this.scroll_animation = None;
                this.scrolling_until = Some(std::time::Instant::now() + Duration::from_millis(150));
                // Wheel listeners (zooming maps and graphs): deltaY > 0 scrolls down.
                let delta = event.delta.pixel_delta(px(16.));
                this.script_mouse("wheel", event.position, 0, (-f32::from(delta.x), -f32::from(delta.y)));
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
            // Painted last, when every element has said where it is: scripts
            // waiting on a forced layout go on now, not a frame later.
            .child({
                let tab = cx.entity().downgrade();
                gpui::canvas(
                    |_, _, _| {},
                    move |_, _, window, cx| {
                        tab.update(cx, |this, cx| {
                            if !this.after_layout.is_empty() {
                                this.note_unpainted(cx);
                                this.answer_after_layout(window);
                            }
                        })
                        .ok();
                    },
                )
                .absolute()
                .size_0()
            })
    }
}

/// The row each of `wanted` is in (the first, for a container split
/// across rows).
fn rows_of(layout: &render::LayoutTree, wanted: &HashSet<NodeId>) -> HashMap<NodeId, usize> {
    let mut out = HashMap::new();
    if wanted.is_empty() {
        return out;
    }
    for (ix, row) in layout.rows.iter().enumerate() {
        nodes_in_row(row, wanted, &mut |n| {
            out.entry(n).or_insert(ix);
        });
    }
    out
}

/// The first and last rows `node` is in (its own, or the rows of a
/// container split across several).
fn row_range(layout: &render::LayoutTree, node: NodeId) -> Option<(usize, usize)> {
    let wanted = HashSet::from([node]);
    let mut range: Option<(usize, usize)> = None;
    for (ix, row) in layout.rows.iter().enumerate() {
        let mut here = false;
        nodes_in_row(row, &wanted, &mut |_| here = true);
        if here {
            range = Some(range.map_or((ix, ix), |(a, _)| (a, ix)));
        }
    }
    range
}

/// Calls `found` for each of `wanted` that has a box in `row` (or is one
/// of the containers it's split from).
fn nodes_in_row(row: &render::boxes::Row, wanted: &HashSet<NodeId>, found: &mut dyn FnMut(NodeId)) {
    use render::boxes::{Block, InlineObject, LayoutBox};
    fn note(n: Option<NodeId>, wanted: &HashSet<NodeId>, found: &mut dyn FnMut(NodeId)) {
        if let Some(n) = n.filter(|n| wanted.contains(n)) {
            found(n);
        }
    }
    fn block(b: &Block, wanted: &HashSet<NodeId>, found: &mut dyn FnMut(NodeId)) {
        note(b.node, wanted, found);
        for c in &b.children {
            walk(c, wanted, found);
        }
    }
    fn walk(b: &LayoutBox, wanted: &HashSet<NodeId>, found: &mut dyn FnMut(NodeId)) {
        match b {
            LayoutBox::Block(b) => block(b, wanted, found),
            LayoutBox::Image(i) => note(i.node, wanted, found),
            LayoutBox::Paragraph(p) => {
                for span in p.spans.iter() {
                    note(span.style.node, wanted, found);
                }
                for o in &p.objects {
                    match o {
                        InlineObject::Image(i) => note(i.node, wanted, found),
                        InlineObject::Block(a) => note(a.node, wanted, found),
                        InlineObject::BlockBox(b) => block(b, wanted, found),
                    }
                }
            }
        }
    }
    for w in row.wrappers.iter() {
        note(w.node, wanted, found);
    }
    walk(&row.content, wanted, found);
    for a in &row.absolutes {
        walk(a, wanted, found);
    }
}
