//! The browser window: toolbar on top, the native page below.

extern crate gpui_kit as gpui;

mod capture;

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use capture::{Args, CaptureJob};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, IconName, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use page::NodeId;
use render::{FontBook, ImageState, ImageStore, LayoutTree, Palette, RenderCx};
use std::collections::{HashMap, HashSet};
use style::ElementState;
use url::Url;

actions!(browser, [Quit, Back, Forward, Reload, FocusAddressBar]);

/// Concurrent image downloads per page.
const IMAGE_CONCURRENCY: usize = 8;
/// Don't fetch more than this many images for one page (lazy loading comes later).
const MAX_IMAGES: usize = 300;

/// A fetched, parsed and laid-out page.
struct LoadedPage {
    url: Url,
    title: Option<String>,
    layout: Arc<LayoutTree>,
    /// Kept so the page can be restyled (window resizes, later: hover, JS).
    doc: Option<Arc<page::Document>>,
    styles: Option<style::StyleData>,
    /// Which media conditions held when `layout` was built.
    signature: Vec<bool>,
}

impl LoadedPage {
    /// The element `#fragment` points at, for `:target`.
    fn target(&self) -> Option<NodeId> {
        let fragment = self.url.fragment()?;
        let doc = self.doc.as_ref()?;
        doc.descendants(doc.root())
            .find(|&id| doc.element(id).and_then(|e| e.attr("id")) == Some(fragment))
    }
}

/// Fetch a subresource (stylesheet) as text, or `None` on any failure. Cached.
fn fetch_text(url: &Url) -> Option<String> {
    net::fetch_text_cached(url)
}

/// Fetch → parse → style → build boxes. Runs on a background thread.
fn load(url: &Url, env: style::MediaEnv) -> anyhow::Result<LoadedPage> {
    let response = net::fetch(url)?;
    let is_html = match &response.content_type {
        Some(ct) => ct.contains("html"),
        None => response.body.trim_start().starts_with('<'),
    };
    if !is_html {
        return Ok(LoadedPage {
            url: response.url,
            title: None,
            layout: Arc::new(render::build_plain_text(&response.body)),
            doc: None,
            styles: None,
            signature: Vec::new(),
        });
    }
    let doc = html::parse(&response.body, response.url.clone());
    let styles = style::StyleData::for_document(&doc, &fetch_text);
    // `:target` from the requested URL's fragment.
    let target = url.fragment().and_then(|f| {
        doc.descendants(doc.root())
            .find(|&id| doc.element(id).and_then(|e| e.attr("id")) == Some(f))
    });
    let state = Arc::new(ElementState { target, ..Default::default() });
    let layout = render::build(&doc, &styles.styler_with_state(env, state));
    Ok(LoadedPage {
        url: response.url,
        title: doc.title(),
        layout: Arc::new(layout),
        signature: styles.signature(&env),
        doc: Some(Arc::new(doc)),
        styles: Some(styles),
    })
}

/// The media environment for the page area of `window`.
fn media_env(window: &Window) -> style::MediaEnv {
    let viewport = window.viewport_size();
    style::MediaEnv {
        width: f32::from(viewport.width),
        // Minus the toolbar.
        height: f32::from(viewport.height) - 48.0,
        ..Default::default()
    }
}

/// Same document, ignoring the fragment?
fn same_document(a: &Url, b: &Url) -> bool {
    a.as_str().split('#').next() == b.as_str().split('#').next()
}

struct Browser {
    focus_handle: FocusHandle,
    palette: Palette,
    url_bar: Entity<InputState>,
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
    images: ImageStore,
    images_pending: usize,
    /// Bumped per finished image (invalidates cached inline layouts).
    images_generation: u64,
    _image_task: Option<Task<()>>,
    _restyle: Option<Task<()>>,
    /// Elements whose blocks report the pointer inside them.
    hover_blocks: HashSet<NodeId>,
    /// The inline element under the pointer, per paragraph.
    hover_inline: Option<(usize, NodeId)>,
    mouse_down: bool,
    /// The interaction state the current layout was styled with.
    element_state: Arc<ElementState>,
    /// Screenshot job from the command line, run once the first page loads.
    capture: Option<CaptureJob>,
    _capture_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Browser {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url_bar = cx.new(|cx| InputState::new(window, cx).placeholder("Enter an address"));
        let subscription = cx.subscribe_in(&url_bar, window, |this, input, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let text = input.read(cx).value();
                match net::parse_input(&text) {
                    Ok(url) => this.navigate(url, window, cx),
                    Err(err) => {
                        this.error = Some(format!("{err:#}"));
                        cx.notify();
                    }
                }
            }
        });
        url_bar.read(cx).focus_handle(cx).focus(window, cx);
        let resize = cx.observe_window_bounds(window, |this, window, cx| this.viewport_changed(window, cx));
        let fonts = Rc::new(FontBook::new(cx.text_system().all_font_names()));
        Self {
            focus_handle: cx.focus_handle(),
            palette: Palette::light(fonts),
            url_bar,
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
            images: ImageStore::default(),
            images_pending: 0,
            images_generation: 0,
            _image_task: None,
            _restyle: None,
            hover_blocks: HashSet::new(),
            hover_inline: None,
            mouse_down: false,
            element_state: Arc::default(),
            capture: None,
            _capture_task: None,
            _subscriptions: vec![subscription, resize],
        }
    }

    /// Go somewhere new: push onto history (dropping any forward entries).
    fn navigate(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
        if !self.history.is_empty() {
            self.history.truncate(self.history_ix + 1);
            self.page_cache.retain(|&ix, _| ix <= self.history_ix);
        }
        self.history.push(url.clone());
        self.history_ix = self.history.len() - 1;
        self.show(url, false, window, cx);
    }

    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_ix > 0 {
            self.go_to_history(self.history_ix - 1, window, cx);
        }
    }

    fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_ix + 1 < self.history.len() {
            self.go_to_history(self.history_ix + 1, window, cx);
        }
    }

    /// Back/forward: restore the page from memory if we still have it.
    fn go_to_history(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.history[ix].clone();
        let same_page = self.page.as_ref().is_some_and(|p| same_document(&p.url, &url));
        if same_page {
            self.history_ix = ix;
            self.show(url, false, window, cx);
            return;
        }
        self.history_ix = ix;
        let Some((page, scroll)) = self.page_cache.remove(&ix) else {
            self.show(url, false, window, cx);
            return;
        };
        self.stash_page();
        self.page_ix = ix;
        self.loading = None;
        self.error = None;
        self.set_url_bar(page.url.as_str(), window, cx);
        window.set_window_title(page.title.as_deref().unwrap_or(page.url.as_str()));
        self.list_state.reset(page.layout.rows.len());
        self.list_state.scroll_to(scroll);
        self.page = Some(page);
        self.hover_blocks.clear();
        self.hover_inline = None;
        self.element_state = Arc::default();
        self.images.borrow_mut().clear();
        self.load_images(cx);
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

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self.history.get(self.history_ix).cloned() {
            self.show(url, true, window, cx);
        }
    }

    /// Display `url`: scroll within the current page if only the fragment differs,
    /// otherwise fetch it. Doesn't touch history.
    fn show(&mut self, url: Url, force_reload: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.set_url_bar(url.as_str(), window, cx);
        self.error = None;
        self.focus_handle.focus(window, cx);

        if !force_reload
            && let Some(page) = &self.page
            && same_document(&page.url, &url)
        {
            self.loading = None;
            if let Some(page) = &mut self.page {
                page.url = url.clone();
            }
            self.page_ix = self.history_ix;
            self.scroll_to_fragment(url.fragment());
            // `:target` moved.
            self.interaction_changed(window, cx);
            cx.notify();
            return;
        }

        cx.notify();
        // Media queries see the page area's size.
        let env = media_env(window);
        self.loading = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn({
                    let url = url.clone();
                    async move { load(&url, env) }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.loading = None;
                match result {
                    Ok(mut page) => {
                        // Keep the fragment across redirects so #anchors still work.
                        if page.url.fragment().is_none() && url.fragment().is_some() {
                            page.url.set_fragment(url.fragment());
                        }
                        // Redirects may have moved us; record where we actually are.
                        this.history[this.history_ix] = page.url.clone();
                        this.set_url_bar(page.url.as_str(), window, cx);
                        window.set_window_title(page.title.as_deref().unwrap_or(page.url.as_str()));
                        this.list_state.reset(page.layout.rows.len());
                        let fragment = page.url.fragment().map(str::to_owned);
                        // Keep the page we're leaving for back/forward (not on reload).
                        if this.page_ix != this.history_ix {
                            this.stash_page();
                        }
                        this.page_ix = this.history_ix;
                        this.page = Some(page);
                        this.hover_blocks.clear();
                        this.hover_inline = None;
                        this.element_state = Arc::default();
                        this.images.borrow_mut().clear();
                        this.scroll_to_fragment(fragment.as_deref());
                        this.load_images(cx);
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
                        Ok((image, width, height)) => {
                            if debug {
                                eprintln!("image loaded: {url}");
                            }
                            ImageState::Ready { image, width, height }
                        }
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
    fn viewport_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if has_rules && self.current_state() != *self.element_state {
            self.restyle(Duration::from_millis(16), window, cx);
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
            let layout = cx
                .background_spawn({
                    let state = state.clone();
                    async move {
                        let mut layout = render::build(&doc, &styles.styler_with_state(env, state));
                        if let Some(old) = old {
                            layout.reuse_unchanged(&old);
                        }
                        layout
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
                    // Usually just colors/decorations changed. Re-measure only
                    // what's on screen: forgetting every row's height makes the
                    // next scroll jump past rows the list hasn't measured.
                    let top = this.list_state.logical_scroll_top().item_ix;
                    let rows = page.layout.rows.len();
                    this.list_state.remeasure_items(top..(top + 40).min(rows));
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
    fn start_capture(&mut self, job: CaptureJob, window: &mut Window, cx: &mut Context<Self>) {
        // Leave a little overlap between pages so nothing falls in a seam.
        let page_step = f32::from(window.viewport_size().height) - 48.0 - 40.0;
        let settle = Duration::from_millis(600);
        self._capture_task = Some(cx.spawn_in(window, async move |this, cx| {
            // Wait (a while) for images, then let the list lay out and measure.
            for _ in 0..50 {
                let pending = this.update(cx, |this, _| this.images_pending).unwrap_or(0);
                if pending == 0 {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(200)).await;
            }
            if let Some((w, h)) = job.resize {
                cx.update(|window, _| window.resize(size(px(w), px(h)))).ok();
                // Debounce + background restyle + image loads for the new layout.
                cx.background_executor().timer(Duration::from_millis(1200)).await;
                for _ in 0..50 {
                    let pending = this.update(cx, |this, _| this.images_pending).unwrap_or(0);
                    if pending == 0 {
                        break;
                    }
                    cx.background_executor().timer(Duration::from_millis(200)).await;
                }
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
            if let Some(frames) = job.bench {
                // Scroll in small steps, timing each full frame (layout + paint).
                let mut times = Vec::with_capacity(frames);
                let rows = this.update(cx, |this, _| this.page.as_ref().map_or(0, |p| p.layout.rows.len())).unwrap_or(0);
                for f in 0..frames {
                    // Step through the whole page (a row at a time, deterministic).
                    this.update(cx, |this, cx| {
                        // One row per frame: about what a fast trackpad scroll reveals.
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
        self.list_state.scroll_to(ListOffset {
            item_ix: row,
            offset_in_item: px(0.),
        });
    }

    fn set_url_bar(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = SharedString::from(text.to_owned());
        self.url_bar
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    fn render_cx(&self, cx: &Context<Self>) -> RenderCx {
        let click = cx.entity().downgrade();
        let hover = click.clone();
        let block_hover = click.clone();
        let inline_hover = click.clone();
        let track_hover = self
            .page
            .as_ref()
            .and_then(|p| p.styles.as_ref())
            .is_some_and(|s| s.has_state_rules());
        RenderCx {
            palette: self.palette.clone(),
            images: self.images.clone(),
            images_generation: self.images_generation,
            track_hover,
            on_block_hover: Rc::new(move |node, hovered, window, cx| {
                block_hover
                    .update(cx, |this, cx| {
                        let changed = if hovered {
                            this.hover_blocks.insert(node)
                        } else {
                            this.hover_blocks.remove(&node)
                        };
                        if changed && std::env::var_os("BROWSER_DEBUG").is_some() {
                            let text = this
                                .page
                                .as_ref()
                                .and_then(|p| Some(p.doc.as_ref()?.text_content(node)))
                                .unwrap_or_default();
                            let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
                            eprintln!("block hover {hovered}: {node:?} {text:?}");
                        }
                        if changed {
                            this.interaction_changed(window, cx);
                        }
                    })
                    .ok();
            }),
            on_inline_hover: Rc::new(move |para, node, window, cx| {
                inline_hover
                    .update(cx, |this, cx| {
                        let next = match node {
                            Some(node) => Some((para, node)),
                            // Leaving this paragraph only clears hover if it was this one's.
                            None if this.hover_inline.is_some_and(|(p, _)| p == para) => None,
                            None => return,
                        };
                        if this.hover_inline != next {
                            if std::env::var_os("BROWSER_DEBUG").is_some() {
                                let text = node
                                    .and_then(|n| Some(this.page.as_ref()?.doc.as_ref()?.text_content(n)))
                                    .unwrap_or_default();
                                eprintln!("inline hover: paragraph {para} → {node:?} {:?}", text.trim());
                            }
                            this.hover_inline = next;
                            this.interaction_changed(window, cx);
                        }
                    })
                    .ok();
            }),
            on_link_click: Rc::new(move |url, window, cx| {
                let url = url.clone();
                click
                    .update(cx, |this, cx| this.navigate(url, window, cx))
                    .ok();
            }),
            on_link_hover: Rc::new(move |url, _window, cx| {
                hover
                    .update(cx, |this, cx| {
                        if this.hovered_link.as_ref() != url {
                            this.hovered_link = url.cloned();
                            cx.notify();
                        }
                    })
                    .ok();
            }),
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let can_back = self.history_ix > 0;
        let can_forward = self.history_ix + 1 < self.history.len();
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .p_2()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("back")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowLeft)
                    .tooltip("Back")
                    .disabled(!can_back)
                    .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
            )
            .child(
                Button::new("forward")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowRight)
                    .tooltip("Forward")
                    .disabled(!can_forward)
                    .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx))),
            )
            .child(
                Button::new("reload")
                    .ghost()
                    .small()
                    .icon(IconName::RotateCw)
                    .tooltip("Reload")
                    .loading(self.loading.is_some())
                    .disabled(self.history.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
            )
            .child(div().flex_1().child(Input::new(&self.url_bar)))
    }

    fn render_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let palette = &self.palette;
        let message = |text: String, color: Hsla| {
            div()
                .p_4()
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        if let Some(err) = &self.error {
            return message(err.clone(), cx.theme().danger);
        }
        let Some(page) = &self.page else {
            let text = if self.loading.is_some() { "Loading…" } else { "Type an address and press Enter." };
            return message(text.into(), palette.muted);
        };

        let layout = page.layout.clone();
        let rcx = self.render_cx(cx);
        let rcx_fixed = rcx.clone();
        let last = layout.rows.len().saturating_sub(1);
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
}

impl Render for Browser {
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
            .key_context("Browser")
            .on_action(cx.listener(|this, _: &Back, window, cx| this.go_back(window, cx)))
            .on_action(cx.listener(|this, _: &Forward, window, cx| this.go_forward(window, cx)))
            .on_action(cx.listener(|this, _: &Reload, window, cx| this.reload(window, cx)))
            .on_action(cx.listener(|this, _: &FocusAddressBar, window, cx| {
                this.url_bar.update(cx, |state, cx| {
                    state.focus_handle(cx).focus(window, cx);
                    state.select_all(window, cx);
                });
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
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
                    .children(status),
            )
    }
}

fn main() {
    // The component library's icons are embedded SVGs served through this asset source.
    // Pages' SVG icons are served alongside the app's own assets.
    gpui_kit::application().with_assets(render::PageAssets(gpui_kit::assets::Assets)).run(|cx| {
        gpui_kit::init(cx);

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-[", Back, Some("Browser")),
            KeyBinding::new("cmd-]", Forward, Some("Browser")),
            KeyBinding::new("cmd-r", Reload, Some("Browser")),
            KeyBinding::new("cmd-l", FocusAddressBar, Some("Browser")),
        ]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let args = Args::parse();
        let bounds = Bounds::centered(None, size(px(args.width), px(args.height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Browser".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let start_url = args.url.as_deref().and_then(|arg| net::parse_input(arg).ok());
        let capturing = args.capture.is_some();
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                let mut browser = Browser::new(window, cx);
                browser.capture = args.capture;
                if let Some(url) = start_url {
                    browser.navigate(url, window, cx);
                }
                browser
            })
        })
        .expect("failed to open window");
        // Screenshot runs shouldn't steal focus from whatever you're doing.
        cx.activate(!capturing);
    });
}
