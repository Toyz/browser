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

mod forms;

use crate::capture::{self, CaptureJob};
use crate::history::{self, History};
use crate::loader::{self, LoadedPage, same_document};

/// Concurrent image downloads per page.
const IMAGE_CONCURRENCY: usize = 8;
/// Don't fetch more than this many images for one page (lazy loading comes later).
const MAX_IMAGES: usize = 300;
/// How long the pointer rests on a link before we start loading it.
const PRERENDER_DWELL: Duration = Duration::from_millis(60);

type PageResult = Result<LoadedPage, Arc<anyhow::Error>>;

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
    history_ix: usize,
    /// Pages we navigated away from, kept so back/forward is instant
    /// (history index → page and where it was scrolled to).
    page_cache: HashMap<usize, (LoadedPage, ListOffset)>,
    /// History index of the page on screen (it stays up while the next loads).
    page_ix: usize,
    page: Option<LoadedPage>,
    error: Option<String>,
    list_state: ListState,
    hovered_link: Option<Url>,
    loading: Option<Task<()>>,
    prerender: Option<Prerender>,
    _hover_timer: Option<Task<()>>,
    images: ImageStore,
    images_pending: usize,
    /// Bumped per finished image (invalidates cached inline layouts).
    images_generation: u64,
    _image_task: Option<Task<()>>,
    favicon: Option<Arc<Image>>,
    _favicon_task: Option<Task<()>>,
    _restyle: Option<Task<()>>,
    /// Elements whose blocks report the pointer inside them.
    hover_blocks: HashSet<NodeId>,
    /// The inline element under the pointer, per paragraph.
    hover_inline: Option<(usize, NodeId)>,
    mouse_down: bool,
    /// Hover restyles wait until scrolling stops (like Chrome): otherwise
    /// every element passing under the pointer restyles the page.
    scrolling_until: Option<std::time::Instant>,
    _scroll_end: Option<Task<()>>,
    /// Bumped per page shown, so hover reports from the previous page's last
    /// frame (with that document's node ids) are ignored.
    page_gen: u64,
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
            history_ix: 0,
            page_cache: HashMap::new(),
            page_ix: 0,
            page: None,
            error: None,
            // Measure well beyond the viewport so fast scrolling stays smooth.
            list_state: ListState::new(0, ListAlignment::Top, px(2000.)),
            hovered_link: None,
            loading: None,
            prerender: None,
            _hover_timer: None,
            images: ImageStore::default(),
            images_pending: 0,
            images_generation: 0,
            _image_task: None,
            favicon: None,
            _favicon_task: None,
            _restyle: None,
            hover_blocks: HashSet::new(),
            hover_inline: None,
            mouse_down: false,
            scrolling_until: None,
            _scroll_end: None,
            page_gen: 0,
            hover_styler: None,
            forms: Default::default(),
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
            self.page_cache.retain(|&ix, _| ix <= self.history_ix);
        }
        self.history.push(url.clone());
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
        self.list_state.reset(page.layout.rows.len());
        self.list_state.scroll_to(scroll);
        self.install(page, cx);
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
                        this.list_state.reset(page.layout.rows.len());
                        let fragment = page.url.fragment().map(str::to_owned);
                        // Keep the page we're leaving for back/forward (not on reload).
                        if this.page_ix != this.history_ix {
                            this.stash_page();
                        }
                        this.page_ix = this.history_ix;
                        this.install(page, cx);
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
        let page = loader::deep(move || loader::load(&url, env, post.as_deref()).map_err(Arc::new));
        cx.background_spawn(page).shared()
    }

    /// Put a loaded page on screen.
    fn install(&mut self, page: LoadedPage, cx: &mut Context<Self>) {
        let favicon = page.favicon.clone();
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
        self.hover_styler = None;
        self.forms = Default::default();
        self.hover_blocks.clear();
        self.hover_inline = None;
        self.hovered_link = None;
        self.element_state = Arc::default();
        self.load_images(cx);
        self.load_favicon(favicon, cx);
    }

    fn load_favicon(&mut self, url: Option<Url>, cx: &mut Context<Self>) {
        let Some(url) = url else {
            self.favicon = None;
            return;
        };
        self._favicon_task = Some(cx.spawn(async move |this, cx| {
            let icon = cx
                .background_spawn(async move { net::fetch_bytes(&url).and_then(render::images::prepare) })
                .await
                .ok()
                .map(|(image, _, _)| image);
            this.update(cx, |this, cx| {
                this.favicon = icon;
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
        let current = self.page.as_ref().is_some_and(|p| same_document(&p.url, &url));
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
                        Ok((image, width, height)) => ImageState::Ready { image, width, height },
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
                // Rules the patch can't see (`:has(:hover)`, `a:hover + b`)
                // get caught by a full restyle once the pointer settles.
                self.restyle(Duration::from_millis(600), window, cx);
            }
            None => self.restyle(Duration::from_millis(16), window, cx),
        }
    }

    /// Try to apply an interaction state change as a paint-only patch.
    /// `Some(None)`: nothing visible changed; `None`: needs a full restyle.
    fn patch_paint(&mut self, state: &Arc<ElementState>, window: &Window) -> Option<Option<render::LayoutTree>> {
        let page = self.page.as_ref()?;
        let (doc, styles) = (page.doc.as_ref()?, page.styles.as_ref()?);
        let env = media_env(window);
        let signature = styles.signature(&env);
        if page.signature != signature {
            return None;
        }
        if self.hover_styler.as_ref().is_none_or(|(sig, _)| *sig != signature) {
            self.hover_styler = Some((signature, styles.styler_with_state(env, self.element_state.clone())));
        }
        let (_, styler) = self.hover_styler.as_mut()?;
        match render::hover::patch(&page.layout, doc, styler, &self.element_state, state) {
            render::hover::Patch::Unchanged => Some(None),
            render::hover::Patch::Painted(layout) => Some(Some(layout)),
            render::hover::Patch::NeedsRestyle => None,
        }
    }

    /// Re-run style + layout in the background with the current viewport and
    /// interaction state, then swap it in, keeping the scroll position.
    fn restyle(&mut self, delay: Duration, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = &self.page else { return };
        let (Some(doc), Some(styles)) = (page.doc.clone(), page.styles.clone()) else { return };
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
            this.update(cx, |this, cx| {
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
                    this.list_state.reset(rows);
                    this.list_state.scroll_to(ListOffset {
                        item_ix: top.item_ix.min(rows.saturating_sub(1)),
                        offset_in_item: top.offset_in_item,
                    });
                }
                this.load_images(cx);
                cx.notify();
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
            if let Some((x, y)) = job.mouse {
                // Draw so hitboxes exist, then hover the point and let restyling run.
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
                })
                .ok();
                cx.background_executor().timer(Duration::from_millis(500)).await;
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
                // Whatever it did (restyle, navigation) gets time to finish.
                cx.background_executor().timer(Duration::from_millis(2500)).await;
                wait_for_images(cx).await;
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
            .and_then(|p| p.styles.as_ref())
            .is_some_and(|s| s.has_state_rules());
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
                    click.update(cx, |this, cx| this.activate(node, window, cx)).ok();
                    return;
                }
                let url = url.clone();
                let new_tab = window.modifiers().secondary(cx);
                click
                    .update(cx, |this, cx| {
                        if new_tab {
                            cx.emit(TabEvent::OpenInNewTab(url));
                        } else {
                            this.navigate(url, window, cx);
                        }
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

        let layout = page.layout.clone();
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

    /// No page yet: the Feather start page, with your most visited sites.
    fn render_new_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let sites = cx.global::<History>().top_sites(8);
        let tiles = sites.into_iter().enumerate().map(|(ix, entry)| {
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
                        .child(initial),
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
                    .child(if self.loading.is_some() { "Loading…" } else { "So light it doesn't even have JavaScript." }),
            )
            .child(div().flex().flex_wrap().justify_center().max_w(px(480.)).children(tiles))
            .into_any_element()
    }
}

impl Render for Tab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    this.focus_handle.focus(window, cx);
                    this.mouse_down = true;
                    this.interaction_changed(window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.mouse_down = false;
                    this.interaction_changed(window, cx);
                }),
            )
            .on_scroll(cx.listener(|this, _, _, _| {
                this.scrolling_until = Some(std::time::Instant::now() + Duration::from_millis(150));
            }))
            .child(self.render_content(cx))
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
    }
}
