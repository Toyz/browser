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
use gpui_kit::*;
use futures::StreamExt as _;
use futures::stream::FuturesUnordered;
use render::{FontBook, ImageState, ImageStore, LayoutTree, Palette, RenderCx};
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

/// Fetch a subresource (stylesheet) as text, or `None` on any failure.
fn fetch_text(url: &Url) -> Option<String> {
    let response = net::fetch(url).ok()?;
    (200..300).contains(&response.status).then_some(response.body)
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
    let layout = render::build(&doc, &styles.styler(env));
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
    page: Option<LoadedPage>,
    error: Option<String>,
    list_state: ListState,
    hovered_link: Option<Url>,
    loading: Option<Task<()>>,
    images: ImageStore,
    images_pending: usize,
    _image_task: Option<Task<()>>,
    _restyle: Option<Task<()>>,
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
            page: None,
            error: None,
            list_state: ListState::new(0, ListAlignment::Top, px(1000.)),
            hovered_link: None,
            loading: None,
            images: ImageStore::default(),
            images_pending: 0,
            _image_task: None,
            _restyle: None,
            capture: None,
            _capture_task: None,
            _subscriptions: vec![subscription, resize],
        }
    }

    /// Go somewhere new: push onto history (dropping any forward entries).
    fn navigate(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
        if !self.history.is_empty() {
            self.history.truncate(self.history_ix + 1);
        }
        self.history.push(url.clone());
        self.history_ix = self.history.len() - 1;
        self.show(url, false, window, cx);
    }

    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_ix > 0 {
            self.history_ix -= 1;
            self.show(self.history[self.history_ix].clone(), false, window, cx);
        }
    }

    fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history_ix + 1 < self.history.len() {
            self.history_ix += 1;
            self.show(self.history[self.history_ix].clone(), false, window, cx);
        }
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
            self.scroll_to_fragment(url.fragment());
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
                        this.page = Some(page);
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
                .take(MAX_IMAGES)
                .map(|(url, _)| (**url).clone())
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
        let (Some(doc), Some(styles)) = (page.doc.clone(), page.styles.clone()) else { return };
        if styles.signature(&env) == page.signature {
            return;
        }
        self._restyle = Some(cx.spawn_in(window, async move |this, cx| {
            // Wait for the resize to settle.
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let Ok(env) = cx.update(|window, _| media_env(window)) else { return };
            let signature = styles.signature(&env);
            let layout = cx
                .background_spawn(async move { render::build(&doc, &styles.styler(env)) })
                .await;
            this.update(cx, |this, cx| {
                let Some(page) = &mut this.page else { return };
                page.layout = Arc::new(layout);
                page.signature = signature;
                // Keep roughly the same place in the page.
                let top = this.list_state.logical_scroll_top();
                let rows = page.layout.rows.len();
                this.list_state.reset(rows);
                this.list_state.scroll_to(ListOffset {
                    item_ix: top.item_ix.min(rows.saturating_sub(1)),
                    offset_in_item: top.offset_in_item,
                });
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
        RenderCx {
            palette: self.palette.clone(),
            images: self.images.clone(),
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
            .into_any_element()
    }
}

impl Render for Browser {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
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
                        cx.listener(|this, _, window, cx| this.focus_handle.focus(window, cx)),
                    )
                    .child(self.render_content(cx))
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
