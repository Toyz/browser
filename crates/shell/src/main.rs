//! Feather: a web browser so light it doesn't have a web engine.
//!
//! The window: a Chrome-style tab strip in the title bar, a toolbar with the
//! omnibox, and the active tab's page (native GPUI elements) below.

extern crate gpui_kit as gpui;

mod capture;
mod history;
mod loader;
mod omnibox;
mod tab;

use std::rc::Rc;

use capture::Args;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, Theme, ThemeMode, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use history::History;
use omnibox::{Omnibox, OmniboxEvent};
use render::{FontBook, Palette};
use tab::{Tab, TabEvent};
use url::Url;

actions!(
    feather,
    [
        Quit, Back, Forward, Reload, FocusAddressBar, NewTab, CloseTab, NextTab, PrevTab,
        SelectTab1, SelectTab2, SelectTab3, SelectTab4, SelectTab5, SelectTab6, SelectTab7, SelectTab8, SelectLastTab,
    ]
);

/// Height of the tab strip (the title bar).
const TAB_STRIP_HEIGHT: f32 = 40.;
/// Height of the toolbar under it, including its bottom border.
const TOOLBAR_HEIGHT: f32 = 45.;
/// Everything above the page.
pub const CHROME_HEIGHT: f32 = TAB_STRIP_HEIGHT + TOOLBAR_HEIGHT;

const TAB_MAX_WIDTH: f32 = 240.;
const TAB_MIN_WIDTH: f32 = 48.;
/// Radius of the active tab's top corners and the curves at its feet.
const TAB_RADIUS: f32 = 8.;

/// `--dark` / `--light` from the command line, else follow the system.
static FORCED_APPEARANCE: std::sync::OnceLock<Option<bool>> = std::sync::OnceLock::new();

pub fn is_dark(window: &Window) -> bool {
    FORCED_APPEARANCE.get().copied().flatten().unwrap_or_else(|| {
        matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
    })
}

/// The browser's own surfaces, Chrome-style: a darker tab strip, with the
/// active tab and the toolbar in one lighter color so the tab "is" the toolbar.
#[derive(Clone, Copy)]
pub struct ChromeColors {
    pub strip: Hsla,
    pub surface: Hsla,
    /// The omnibox, when not focused.
    pub field: Hsla,
}

impl ChromeColors {
    pub fn get(cx: &App) -> Self {
        if cx.theme().mode.is_dark() {
            Self { strip: rgb(0x1b1c1f).into(), surface: rgb(0x2b2c30).into(), field: rgb(0x1b1c1f).into() }
        } else {
            Self { strip: rgb(0xdfe3e8).into(), surface: rgb(0xffffff).into(), field: rgb(0xeef1f4).into() }
        }
    }
}

/// Match the chrome to the appearance.
fn apply_theme(window: &mut Window, cx: &mut App) {
    let mode = if is_dark(window) { ThemeMode::Dark } else { ThemeMode::Light };
    Theme::change(mode, Some(window), cx);
}

struct OpenTab {
    tab: Entity<Tab>,
    _subscriptions: [Subscription; 2],
}

struct Feather {
    palette: Palette,
    tabs: Vec<OpenTab>,
    active: usize,
    omnibox: Entity<Omnibox>,
    _subscriptions: Vec<Subscription>,
}

impl Feather {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let omnibox = cx.new(|cx| Omnibox::new(window, cx));
        let navigate = cx.subscribe_in(&omnibox, window, |this, _, event: &OmniboxEvent, window, cx| match event {
            OmniboxEvent::Navigate(url) => {
                let url = url.clone();
                this.active_tab().update(cx, |tab, cx| tab.navigate(url, window, cx));
            }
        });
        let resize = cx.observe_window_bounds(window, |this: &mut Self, window, cx| {
            for open in &this.tabs {
                open.tab.update(cx, |tab, cx| tab.viewport_changed(window, cx));
            }
        });
        // Dark mode: the chrome follows, and pages see `prefers-color-scheme`.
        let appearance = cx.observe_window_appearance(window, |this: &mut Self, window, cx| {
            apply_theme(window, cx);
            for open in &this.tabs {
                open.tab.update(cx, |tab, cx| tab.viewport_changed(window, cx));
            }
            cx.notify();
        });
        apply_theme(window, cx);
        let fonts = Rc::new(FontBook::new(cx.text_system().all_font_names()));
        let mut this = Self {
            palette: Palette::light(fonts),
            tabs: Vec::new(),
            active: 0,
            omnibox,
            _subscriptions: vec![navigate, resize, appearance],
        };
        this.open_tab(None, true, window, cx);
        this
    }

    fn active_tab(&self) -> Entity<Tab> {
        self.tabs[self.active].tab.clone()
    }

    /// Open a tab after the active one (or at the end, for Cmd-T).
    fn open_tab(&mut self, url: Option<Url>, activate: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<Tab> {
        let palette = self.palette.clone();
        let tab = cx.new(|cx| Tab::new(palette, cx));
        let observe = cx.observe_in(&tab, window, |this, tab, window, cx| {
            if tab == this.active_tab() {
                this.sync_chrome(window, cx);
            }
            cx.notify();
        });
        let events = cx.subscribe_in(&tab, window, |this, _, event: &TabEvent, window, cx| match event {
            TabEvent::OpenInNewTab(url) => {
                this.open_tab(Some(url.clone()), false, window, cx);
            }
            TabEvent::TypeInOmnibox(text) => {
                // A character at a time, like a person (autocomplete reacts to each).
                this.omnibox.update(cx, |o, cx| {
                    o.focus(window, cx);
                    for end in text.char_indices().map(|(i, c)| i + c.len_utf8()) {
                        o.type_text(&text[..end], window, cx);
                    }
                });
            }
        });
        let ix = if url.is_some() && !self.tabs.is_empty() { self.active + 1 } else { self.tabs.len() };
        self.tabs.insert(ix, OpenTab { tab: tab.clone(), _subscriptions: [observe, events] });
        if let Some(url) = url {
            tab.update(cx, |tab, cx| tab.navigate(url, window, cx));
        }
        if activate {
            self.activate(ix, window, cx);
            if self.active_tab().read(cx).url().is_none() {
                self.omnibox.update(cx, |o, cx| o.focus(window, cx));
            }
        }
        cx.notify();
        tab
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = ix;
        let tab = self.active_tab();
        tab.update(cx, |tab, cx| {
            tab.focus(window, cx);
            // It may have been in the background through a resize.
            tab.viewport_changed(window, cx);
        });
        self.sync_chrome(window, cx);
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            window.remove_window();
            return;
        }
        self.tabs.remove(ix);
        if self.active > ix || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.activate(self.active, window, cx);
    }

    /// Point the omnibox and window title at the active tab.
    fn sync_chrome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active_tab().read(cx);
        let (url, title) = (tab.url(), tab.title());
        self.omnibox.update(cx, |o, cx| o.set_url(url, window, cx));
        window.set_window_title(&title);
    }

    fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        let ix = (self.active as isize + delta).rem_euclid(n) as usize;
        self.activate(ix, window, cx);
    }

    fn render_tab(&self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let tab = self.tabs[ix].tab.read(cx);
        let active = ix == self.active;
        let next_active = ix + 1 == self.active;
        let ChromeColors { strip, surface, .. } = ChromeColors::get(cx);
        let icon: AnyElement = if tab.is_loading() {
            Spinner::new().xsmall().color(theme.primary).into_any_element()
        } else if let Some(image) = tab.favicon() {
            img(image).size(px(16.)).flex_none().into_any_element()
        } else {
            Icon::new(Lucide::Globe).small().text_color(theme.muted_foreground).into_any_element()
        };
        // Chrome's active tab flares out into the toolbar at its feet: a square
        // of the tab's color behind a quarter circle of the strip's color.
        let foot = |left: bool| {
            let r = px(TAB_RADIUS);
            let corner = div().size(r).bg(strip);
            div()
                .absolute()
                .bottom_0()
                .size(r)
                .bg(surface)
                .when(left, |d| d.left(-r))
                .when(!left, |d| d.right(-r))
                .child(if left { corner.rounded_br(r) } else { corner.rounded_bl(r) })
        };
        div()
            .id(("tab", ix))
            .relative()
            .flex_1()
            .min_w(px(TAB_MIN_WIDTH))
            .max_w(px(TAB_MAX_WIDTH))
            .h(px(TAB_STRIP_HEIGHT - 6.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .rounded_t(px(TAB_RADIUS))
            .when(active, |d| d.bg(surface).text_color(theme.foreground).child(foot(true)).child(foot(false)))
            .when(!active, |d| {
                d.text_color(theme.muted_foreground)
                    .hover(|d| d.bg(surface.opacity(0.5)))
                    // A divider between inactive tabs.
                    .when(!next_active, |d| {
                        d.child(div().absolute().right_0().top(px(9.)).bottom(px(9.)).w(px(1.)).bg(theme.border))
                    })
            })
            .child(icon)
            .child(div().flex_1().min_w_0().truncate().child(tab.title()))
            .child(
                div()
                    .id(("close-tab", ix))
                    .flex_none()
                    .rounded_full()
                    .p(px(2.))
                    .hover(|d| d.bg(theme.secondary))
                    .child(Icon::new(Lucide::X).xsmall())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_tab(ix, window, cx);
                    })),
            )
            // Tabs are buttons, not part of the draggable title bar.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.activate(ix, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_tab(ix, window, cx);
                }),
            )
    }

    fn render_tab_strip(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let new_tab = div()
            .id("new-tab")
            .flex_none()
            .ml_1()
            .mb(px(4.))
            .p_1()
            .rounded_full()
            .hover(|d| d.bg(theme.secondary))
            .child(Icon::new(Lucide::Plus).small())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_tab(None, true, window, cx);
            }));
        let tabs: Vec<_> = (0..self.tabs.len()).map(|ix| self.render_tab(ix, cx)).collect();
        let fullscreen = window.is_fullscreen();
        TitleBar::new()
            .h(px(TAB_STRIP_HEIGHT))
            .bg(ChromeColors::get(cx).strip)
            .border_b_0()
            // Room for the traffic lights (not needed in fullscreen).
            .when(fullscreen, |t| t.pl_2())
            .child(
                div()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_end()
                    .pl_2()
                    .pr(px(80.))
                    .children(tabs)
                    .child(new_tab),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tab = self.active_tab().read(cx);
        let (can_back, can_forward, loading, has_url) =
            (tab.can_go_back(), tab.can_go_forward(), tab.is_loading(), tab.url().is_some());
        div()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .bg(ChromeColors::get(cx).surface)
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("back")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowLeft)
                    .tooltip("Back")
                    .disabled(!can_back)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.active_tab().update(cx, |t, cx| t.go_back(window, cx));
                    })),
            )
            .child(
                Button::new("forward")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowRight)
                    .tooltip("Forward")
                    .disabled(!can_forward)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.active_tab().update(cx, |t, cx| t.go_forward(window, cx));
                    })),
            )
            .child(
                Button::new("reload")
                    .ghost()
                    .small()
                    .icon(if loading { Icon::new(Lucide::X) } else { Icon::new(Lucide::RotateCw) })
                    .tooltip("Reload")
                    .disabled(!has_url)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.active_tab().update(cx, |t, cx| t.reload(window, cx));
                    })),
            )
            .child(div().flex_1().px_1().child(self.omnibox.clone()))
    }
}

impl Render for Feather {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg) = (theme.background, theme.foreground);
        div()
            .key_context("Browser")
            .on_action(cx.listener(|this, _: &Back, window, cx| this.active_tab().update(cx, |t, cx| t.go_back(window, cx))))
            .on_action(cx.listener(|this, _: &Forward, window, cx| {
                this.active_tab().update(cx, |t, cx| t.go_forward(window, cx))
            }))
            .on_action(cx.listener(|this, _: &Reload, window, cx| this.active_tab().update(cx, |t, cx| t.reload(window, cx))))
            .on_action(cx.listener(|this, _: &FocusAddressBar, window, cx| {
                this.omnibox.update(cx, |o, cx| o.focus(window, cx))
            }))
            .on_action(cx.listener(|this, _: &NewTab, window, cx| {
                this.open_tab(None, true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| this.close_tab(this.active, window, cx)))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle(-1, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab1, window, cx| this.activate(0, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab2, window, cx| this.activate(1, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab3, window, cx| this.activate(2, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab4, window, cx| this.activate(3, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab5, window, cx| this.activate(4, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab6, window, cx| this.activate(5, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab7, window, cx| this.activate(6, window, cx)))
            .on_action(cx.listener(|this, _: &SelectTab8, window, cx| this.activate(7, window, cx)))
            .on_action(cx.listener(|this, _: &SelectLastTab, window, cx| {
                this.activate(this.tabs.len() - 1, window, cx)
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .text_color(fg)
            .child(self.render_tab_strip(window, cx))
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(self.active_tab()))
    }
}

// gpui-kit embeds only the components' own icons; these are ours.
gpui_kit::assets::icon_assets!(ChromeIcons, [Feather, LockKeyhole, TriangleAlert, Search, Globe, Clock, X, Plus, RotateCw, File, FolderOpen, Check, ChevronDown]);

/// The components' icons, then ours.
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        // (The components' source errors on paths it doesn't have.)
        match ChromeIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut all = gpui_kit::assets::Assets.list(path).unwrap_or_default();
        all.extend(ChromeIcons.list(path)?);
        Ok(all)
    }
}

fn main() {
    // Icons are embedded SVGs served through the asset source; pages' SVG
    // icons are served alongside them.
    gpui_kit::application().with_assets(render::PageAssets(AppAssets)).run(|cx| {
        gpui_kit::init(cx);
        omnibox::init(cx);

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-[", Back, Some("Browser")),
            KeyBinding::new("cmd-]", Forward, Some("Browser")),
            KeyBinding::new("cmd-r", Reload, Some("Browser")),
            KeyBinding::new("cmd-l", FocusAddressBar, Some("Browser")),
            KeyBinding::new("cmd-t", NewTab, Some("Browser")),
            KeyBinding::new("cmd-w", CloseTab, Some("Browser")),
            KeyBinding::new("ctrl-tab", NextTab, Some("Browser")),
            KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Browser")),
            KeyBinding::new("cmd-shift-]", NextTab, Some("Browser")),
            KeyBinding::new("cmd-shift-[", PrevTab, Some("Browser")),
            KeyBinding::new("cmd-alt-right", NextTab, Some("Browser")),
            KeyBinding::new("cmd-alt-left", PrevTab, Some("Browser")),
            KeyBinding::new("cmd-1", SelectTab1, Some("Browser")),
            KeyBinding::new("cmd-2", SelectTab2, Some("Browser")),
            KeyBinding::new("cmd-3", SelectTab3, Some("Browser")),
            KeyBinding::new("cmd-4", SelectTab4, Some("Browser")),
            KeyBinding::new("cmd-5", SelectTab5, Some("Browser")),
            KeyBinding::new("cmd-6", SelectTab6, Some("Browser")),
            KeyBinding::new("cmd-7", SelectTab7, Some("Browser")),
            KeyBinding::new("cmd-8", SelectTab8, Some("Browser")),
            KeyBinding::new("cmd-9", SelectLastTab, Some("Browser")),
        ]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let args = Args::parse();
        FORCED_APPEARANCE.set(args.dark).ok();
        let capturing = args.capture.is_some();
        History::init(!capturing, cx);
        let bounds = Bounds::centered(None, size(px(args.width), px(args.height)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Feather".into()),
                // Traffic lights centered in the tab strip.
                traffic_light_position: Some(point(px(13.), px(TAB_STRIP_HEIGHT / 2. - 7.))),
                ..TitleBar::title_bar_options()
            }),
            ..TitleBar::window_options()
        };
        let urls: Vec<Url> = args.urls.iter().filter_map(|arg| net::parse_input(arg).ok()).collect();
        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| {
                let mut feather = Feather::new(window, cx);
                // Extra URLs open as background tabs, each right after the first.
                let mut urls = urls.into_iter();
                if urls.len() == 0
                    && let Some(job) = args.capture.clone()
                {
                    // Nothing to load: capture the new tab page.
                    feather.active_tab().update(cx, |tab, cx| tab.start_capture(job, window, cx));
                }
                if let Some(url) = urls.next() {
                    let tab = feather.active_tab();
                    tab.update(cx, |tab, cx| {
                        tab.capture = args.capture.clone();
                        tab.navigate(url, window, cx);
                    });
                }
                for url in urls.rev() {
                    feather.open_tab(Some(url), false, window, cx);
                }
                feather.sync_chrome(window, cx);
                feather
            })
        })
        .expect("failed to open window");
        // Screenshot runs shouldn't steal focus from whatever you're doing.
        cx.activate(!capturing);
    });
}

#[cfg(test)]
mod tests {
    use super::{AppAssets, Lucide};
    use gpui_kit::AssetSource as _;

    #[test]
    fn chrome_icons_load() {
        for icon in [Lucide::X, Lucide::LockKeyhole, Lucide::Globe, Lucide::Feather] {
            let path = icon.path();
            let r = AppAssets.load(&path);
            println!("{path}: {:?}", r.as_ref().map(|o| o.as_ref().map(|b| b.len())));
            assert!(matches!(r, Ok(Some(_))), "{path}");
        }
    }
}
