//! The omnibox: an address bar that also searches, suggests from history,
//! and finishes what you're typing.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use url::Url;

use crate::history::{self, History};

actions!(omnibox, [SelectPrev, SelectNext, Revert]);

/// No JavaScript needed, which is the main requirement around here.
const SEARCH_URL: &str = "https://html.duckduckgo.com/html/";
const MAX_SUGGESTIONS: usize = 6;

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, Some("Omnibox > Input")),
        KeyBinding::new("down", SelectNext, Some("Omnibox > Input")),
        KeyBinding::new("escape", Revert, Some("Omnibox > Input")),
    ]);
}

pub fn search_url(query: &str) -> Url {
    let mut url = Url::parse(SEARCH_URL).expect("valid search URL");
    url.query_pairs_mut().append_pair("q", query);
    url
}

/// Does this look like an address rather than a search?
fn looks_like_address(text: &str) -> bool {
    let t = text.trim();
    !t.contains(char::is_whitespace)
        && (t.contains("://")
            || t.starts_with("localhost")
            || t.split('/').next().is_some_and(|host| host.contains('.') && !host.ends_with('.')))
}

#[derive(Clone)]
enum Suggestion {
    Search(String),
    Visit { url: Url, title: String },
}

impl Suggestion {
    fn target(&self) -> Option<Url> {
        match self {
            Suggestion::Search(q) => Some(search_url(q)),
            Suggestion::Visit { url, .. } => Some(url.clone()),
        }
    }
}

pub enum OmniboxEvent {
    Navigate(Url),
}

pub struct Omnibox {
    input: Entity<InputState>,
    /// The active tab's address.
    url: Option<Url>,
    suggestions: Vec<Suggestion>,
    selected: usize,
    /// What was actually typed (the input may hold an inline completion too).
    typed: String,
    /// Ignore the `Change` events our own `set_value` calls cause.
    setting: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OmniboxEvent> for Omnibox {}

impl Omnibox {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search or enter address"));
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.changed(window, cx),
            InputEvent::PressEnter { .. } => this.submit(window, cx),
            InputEvent::Focus => {
                // Show the full address while editing, all selected.
                let full = this.url.as_ref().map(|u| u.to_string()).unwrap_or_default();
                this.set_text(&full, window, cx);
                this.input.update(cx, |s, cx| s.select_all(window, cx));
                this.typed = full;
                cx.notify();
            }
            InputEvent::Blur => {
                this.suggestions.clear();
                this.show_url(window, cx);
                cx.notify();
            }
        });
        // Redraw (the caret blinking) when the field changes.
        let redraw = cx.observe(&input, |_, _, cx| cx.notify());
        Self {
            input,
            url: None,
            suggestions: Vec::new(),
            selected: 0,
            typed: String::new(),
            setting: false,
            _subscriptions: vec![sub, redraw],
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
    }

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// The active tab moved (or changed): show its address, unless the user
    /// is in the middle of typing one.
    pub fn set_url(&mut self, url: Option<Url>, window: &mut Window, cx: &mut Context<Self>) {
        if self.url == url {
            return;
        }
        self.url = url;
        if !self.is_focused(window, cx) {
            self.show_url(window, cx);
        }
        cx.notify();
    }

    fn show_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.url.as_ref().map(history::display_url).unwrap_or_default();
        self.set_text(&shown, window, cx);
    }

    fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.setting = true;
        let text = SharedString::from(text.to_owned());
        self.input.update(cx, |s, cx| s.set_value(text, window, cx));
        self.setting = false;
    }

    /// Replace the text as if typed (screenshot mode).
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = SharedString::from(text.to_owned());
        self.input.update(cx, |s, cx| s.set_value(text, window, cx));
        self.changed(window, cx);
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.setting {
            return;
        }
        let value = self.input.read(cx).value().to_string();
        // Typing forward (not deleting) gets an inline completion.
        let grew = value.len() > self.typed.len() && value.starts_with(&self.typed);
        self.typed = value.clone();
        self.suggestions.clear();
        self.selected = 0;
        if value.trim().is_empty() {
            cx.notify();
            return;
        }
        let matches = cx.global::<History>().search(&value, MAX_SUGGESTIONS);
        let mut completed = None;
        if grew
            && let Some(best) = matches.iter().find(|m| m.prefix)
        {
            let shown = history::display_url(&best.entry.url);
            let lower = value.to_lowercase();
            let full = best.entry.url.as_str();
            // Complete the host first, like Chrome, unless they're typing a path.
            let candidate = if shown.to_lowercase().starts_with(&lower) {
                if !value.contains('/') {
                    shown.split('/').next().unwrap_or(&shown).to_owned()
                } else {
                    shown
                }
            } else {
                full.to_owned()
            };
            if candidate.len() > value.len() && candidate.to_lowercase().starts_with(&lower) {
                let completion = format!("{value}{}", &candidate[value.len()..]);
                self.set_text(&completion, window, cx);
                let range = value.len()..completion.len();
                self.input.update(cx, |s, cx| s.set_selected_range(range, cx));
                completed = Some(completion);
            }
        }
        // The first row is what Enter does.
        let first = completed.as_deref().unwrap_or(&value);
        if looks_like_address(first) {
            if let Ok(url) = net::parse_input(first) {
                self.suggestions.push(Suggestion::Visit { title: String::new(), url });
            }
        } else {
            self.suggestions.push(Suggestion::Search(value.trim().to_owned()));
        }
        for m in matches {
            if self.suggestions.iter().any(|s| matches!(s, Suggestion::Visit { url, .. } if *url == m.entry.url)) {
                continue;
            }
            self.suggestions.push(Suggestion::Visit { url: m.entry.url, title: m.entry.title });
        }
        if looks_like_address(first) && !value.trim().is_empty() {
            self.suggestions.push(Suggestion::Search(value.trim().to_owned()));
        }
        self.suggestions.truncate(MAX_SUGGESTIONS + 1);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.input.read(cx).value().to_string();
        let target = match self.suggestions.get(self.selected) {
            Some(s) => s.target(),
            None if looks_like_address(&value) => net::parse_input(&value).ok(),
            None if value.trim().is_empty() => None,
            None => Some(search_url(value.trim())),
        };
        if let Some(url) = target {
            self.go(url, window, cx);
        }
    }

    fn go(&mut self, url: Url, window: &mut Window, cx: &mut Context<Self>) {
        self.suggestions.clear();
        self.url = Some(url.clone());
        self.typed.clear();
        cx.emit(OmniboxEvent::Navigate(url));
        // The tab takes focus; Blur restores the display form of the address.
        let _ = window;
        cx.notify();
    }

    fn select(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if self.suggestions.is_empty() {
            return;
        }
        let n = self.suggestions.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        // Arrowing through suggestions previews them in the box.
        let text = match &self.suggestions[self.selected] {
            Suggestion::Search(q) => q.clone(),
            Suggestion::Visit { url, .. } => url.to_string(),
        };
        self.set_text(&text, window, cx);
        cx.notify();
    }

    fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.suggestions.clear();
        let full = self.url.as_ref().map(|u| u.to_string()).unwrap_or_default();
        self.set_text(&full, window, cx);
        self.input.update(cx, |s, cx| s.select_all(window, cx));
        self.typed = full;
        cx.notify();
    }
}

impl Render for Omnibox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.is_focused(window, cx);
        // Visited sites show their own icon.
        let favicons: Vec<_> = self
            .suggestions
            .iter()
            .map(|s| match s {
                Suggestion::Visit { url, .. } if focused => crate::favicons::Favicons::get(url, cx),
                _ => None,
            })
            .collect();
        let theme = cx.theme();
        let scheme = self.url.as_ref().map(|u| u.scheme());
        // Only plain http gets the red warning.
        let secure = scheme != Some("http");
        let icon = match scheme {
            _ if focused => Lucide::Search,
            Some("https") => Lucide::LockKeyhole,
            Some("file") if self.url.as_ref().is_some_and(|u| u.path().ends_with('/')) => Lucide::FolderOpen,
            Some("file") | Some("view-source") => Lucide::File,
            Some(_) => Lucide::TriangleAlert,
            None => Lucide::Search,
        };
        let field = crate::ChromeColors::get(cx).field;
        let open = focused && !self.suggestions.is_empty();
        let muted = theme.muted_foreground;

        let rows = self.suggestions.iter().zip(favicons).enumerate().map(|(ix, (s, favicon))| {
            let (icon, primary, secondary): (Lucide, String, String) = match s {
                Suggestion::Search(q) => (Lucide::Search, q.clone(), "DuckDuckGo search".into()),
                Suggestion::Visit { url, title } if title.is_empty() => {
                    (Lucide::Globe, history::display_url(url), String::new())
                }
                Suggestion::Visit { url, title } => (Lucide::Clock, title.clone(), history::display_url(url)),
            };
            let target = s.target();
            div()
                .id(("suggestion", ix))
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .text_sm()
                .when(ix == self.selected, |d| d.bg(theme.accent))
                .hover(|d| d.bg(theme.accent))
                .map(|d| match favicon {
                    Some(image) => d.child(img(image).size(px(16.)).flex_none()),
                    None => d.child(Icon::new(icon).small().text_color(muted)),
                })
                .child(div().flex_none().max_w(relative(0.6)).truncate().child(primary))
                .when(!secondary.is_empty(), |d| {
                    d.child(div().text_color(muted).truncate().child(format!("— {secondary}")))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        if let Some(url) = target.clone() {
                            this.go(url, window, cx);
                        }
                    }),
                )
        });

        div()
            .key_context("Omnibox")
            .on_action(cx.listener(|this, _: &SelectPrev, window, cx| this.select(-1, window, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, window, cx| this.select(1, window, cx)))
            .on_action(cx.listener(|this, _: &Revert, window, cx| this.revert(window, cx)))
            .relative()
            .flex_1()
            .child(
                Input::new(&self.input)
                    .appearance(false)
                    .prefix(Icon::new(icon).small().text_color(if secure || focused { muted } else { theme.danger }))
                    .h(px(30.))
                    .px_3()
                    .rounded_full()
                    .bg(if focused { theme.background } else { field })
                    .border_1()
                    .border_color(if focused { theme.ring } else { field }),
            )
            .when(open, |d| {
                d.child(deferred(
                    div()
                        .id("suggestions")
                        .occlude()
                        .absolute()
                        .top(px(34.))
                        .left_0()
                        .right_0()
                        .py_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .text_color(theme.popover_foreground)
                        .shadow_lg()
                        .children(rows),
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` macro would shadow the built-in one.
    use super::{looks_like_address, search_url};

    #[test]
    fn addresses_vs_searches() {
        assert!(looks_like_address("example.com"));
        assert!(looks_like_address("en.wikipedia.org/wiki/Rust"));
        assert!(looks_like_address("localhost:8080"));
        assert!(looks_like_address("https://x"));
        assert!(!looks_like_address("rust gpui"));
        assert!(!looks_like_address("hello"));
        assert!(!looks_like_address("what is 1.5"));
        assert_eq!(search_url("a b").as_str(), "https://html.duckduckgo.com/html/?q=a+b");
    }
}
