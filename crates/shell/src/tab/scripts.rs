//! The tab's side of a page's scripts: applying the DOM snapshots their
//! work produces, and telling them about clicks.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;
use page::NodeId;

use super::Tab;
use crate::loader;

/// How long a click waits for scripts to answer whether they cancelled it.
const CLICK_TIMEOUT: Duration = Duration::from_millis(300);

impl Tab {
    /// A page was installed: follow its script thread's updates.
    pub(super) fn follow_scripts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._scripts_task = None;
        let Some(updates) = self.page.as_ref().and_then(|p| p.scripts.as_ref()).map(|h| h.updates.clone()) else { return };
        let page_gen = self.page_gen;
        self._scripts_task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Ok(msg) = updates.recv().await {
                let alive = this.update_in(cx, |this, window, cx| {
                    if this.page_gen != page_gen {
                        return false;
                    }
                    match msg {
                        js::FromJs::Document(doc) => this.script_dom(doc, window, cx),
                        js::FromJs::Navigate(url) => this.navigate(url, window, cx),
                        js::FromJs::Submit(form) => this.submit_native(form, None, window, cx),
                        js::FromJs::UrlChanged { url, replace } => this.script_url_changed(url, replace, cx),
                        js::FromJs::Focus(node) => this.script_focus(node, window, cx),
                        js::FromJs::Traverse(delta) => {
                            if delta < 0 {
                                this.go_back(window, cx)
                            } else if delta > 0 {
                                this.go_forward(window, cx)
                            }
                        }
                    }
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// Scripts changed the DOM: show the new one (restyled; stylesheets
    /// re-read only if they changed).
    fn script_dom(&mut self, doc: Arc<page::Document>, window: &mut Window, cx: &mut Context<Self>) {
        // Debugging: FEATHER_DUMP_DOM=file.html keeps the latest snapshot.
        if let Some(path) = std::env::var_os("FEATHER_DUMP_DOM") {
            std::fs::write(path, js::to_html(&doc)).ok();
        }
        let Some(page) = &mut self.page else { return };
        let sheets_changed = page.doc.as_ref().is_none_or(|old| loader::style_key(old) != loader::style_key(&doc));
        page.title = doc.title();
        page.doc = Some(doc.clone());
        cx.notify();
        if !sheets_changed {
            self.restyle(Duration::ZERO, window, cx);
            return;
        }
        let page_gen = self.page_gen;
        self._script_styles = Some(cx.spawn_in(window, async move |this, cx| {
            let styles = loader::deep(move || loader::styles_for(&doc)).await;
            this.update_in(cx, |this, window, cx| {
                if this.page_gen != page_gen {
                    return;
                }
                if let Some(page) = &mut this.page {
                    page.styles = Some(styles);
                }
                // The interaction styler was built from the old rules.
                this.hover_styler = None;
                // New rules may use web fonts nothing asked for yet.
                this.load_fonts(cx);
                this.restyle(Duration::ZERO, window, cx);
            })
            .ok();
        }));
    }

    /// The element under the pointer (the innermost one we know of).
    fn click_target(&self) -> Option<NodeId> {
        if let Some((_, node)) = self.hover_inline {
            return Some(node);
        }
        let doc = self.page.as_ref()?.doc.as_ref()?;
        let depth = |mut n: NodeId| {
            let mut d = 0;
            while let Some(p) = doc.parent(n) {
                d += 1;
                n = p;
            }
            d
        };
        self.hover_blocks.iter().copied().max_by_key(|&n| depth(n))
    }

    /// The target and its ancestors, if scripts listen for clicks on any.
    fn heard_click(&self) -> Option<(Arc<js::ScriptHost>, NodeId)> {
        let page = self.page.as_ref()?;
        let host = page.scripts.clone()?;
        let target = self.click_target()?;
        let doc = page.doc.as_ref()?;
        let mut path = vec![target];
        let mut n = target;
        while let Some(p) = doc.parent(n) {
            path.push(p);
            n = p;
        }
        host.hears_click(&path).then_some((host, target))
    }

    /// A link (or button) was clicked: scripts listening get to cancel it
    /// first; `act` is the default action.
    pub(super) fn link_clicked(
        &mut self,
        act: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((host, target)) = self.heard_click() else {
            if std::env::var_os("BROWSER_DEBUG").is_some() {
                eprintln!("click: no script listening (target {:?})", self.click_target());
            }
            act(self, window, cx);
            return;
        };
        self.script_clicked = true;
        let page_gen = self.page_gen;
        cx.spawn_in(window, async move |this, cx| {
            let prevented = cx.background_spawn(async move { host.click(target, CLICK_TIMEOUT) }).await;
            if std::env::var_os("BROWSER_DEBUG").is_some() {
                eprintln!("click: scripts {}", if prevented { "handled it" } else { "let it through" });
            }
            this.update_in(cx, |this, window, cx| {
                if this.page_gen == page_gen && !prevented {
                    act(this, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// A key pressed (or let go) in the page: its scripts hear it, at the
    /// focused field if there is one.
    pub(super) fn script_key(&mut self, kind: &'static str, keystroke: &Keystroke) {
        let Some(host) = self.page.as_ref().and_then(|p| p.scripts.clone()) else { return };
        let (key, code) = dom_key(keystroke);
        let m = &keystroke.modifiers;
        host.send(js::ToJs::Key { kind, key, code, target: self.forms.focused, modifiers: [m.control, m.shift, m.alt, m.platform] });
    }

    /// A mouse event in the page: scripts listening under the pointer get
    /// it (`position` in window coordinates).
    pub(super) fn script_mouse(&mut self, kind: &'static str, position: Point<Pixels>, button: i16, delta: (f32, f32)) {
        let Some(page) = self.page.as_ref() else { return };
        let Some(host) = page.scripts.clone() else { return };
        let Some(target) = self.click_target() else { return };
        let Some(doc) = page.doc.as_ref() else { return };
        let mut path = vec![target];
        let mut n = target;
        while let Some(p) = doc.parent(n) {
            path.push(p);
            n = p;
        }
        if !host.hears_mouse(&path) {
            return;
        }
        let buttons = if self.mouse_down { 1 } else { 0 };
        host.send(js::ToJs::Mouse {
            kind,
            node: target,
            x: f32::from(position.x),
            y: f32::from(position.y) - crate::CHROME_HEIGHT,
            button,
            buttons,
            delta,
        });
    }

    /// Mouse up: a click on something that isn't a link still reaches the
    /// page's listeners (links and buttons went through `link_clicked`).
    pub(super) fn script_mouse_up(&mut self) {
        if std::mem::take(&mut self.script_clicked) || self.hovered_link.is_some() {
            return;
        }
        if let Some((host, target)) = self.heard_click() {
            host.send(js::ToJs::Click { node: target, reply: None });
        }
    }
}

/// A GPUI keystroke as DOM `KeyboardEvent.key` and `.code`.
fn dom_key(k: &Keystroke) -> (String, String) {
    let named = match k.key.as_str() {
        "enter" => Some(("Enter", "Enter")),
        "escape" => Some(("Escape", "Escape")),
        "backspace" => Some(("Backspace", "Backspace")),
        "tab" => Some(("Tab", "Tab")),
        "space" => Some((" ", "Space")),
        "left" => Some(("ArrowLeft", "ArrowLeft")),
        "right" => Some(("ArrowRight", "ArrowRight")),
        "up" => Some(("ArrowUp", "ArrowUp")),
        "down" => Some(("ArrowDown", "ArrowDown")),
        "delete" => Some(("Delete", "Delete")),
        "home" => Some(("Home", "Home")),
        "end" => Some(("End", "End")),
        "pageup" => Some(("PageUp", "PageUp")),
        "pagedown" => Some(("PageDown", "PageDown")),
        "shift" => Some(("Shift", "ShiftLeft")),
        "control" => Some(("Control", "ControlLeft")),
        "alt" => Some(("Alt", "AltLeft")),
        "platform" | "cmd" => Some(("Meta", "MetaLeft")),
        _ => None,
    };
    if let Some((key, code)) = named {
        return (key.into(), code.into());
    }
    if let Some(f) = k.key.strip_prefix('f').filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())) {
        return (format!("F{f}"), format!("F{f}"));
    }
    let key = k.key_char.clone().filter(|c| c.chars().count() == 1).unwrap_or_else(|| k.key.clone());
    let ch = k.key.chars().next().unwrap_or(' ');
    let code = if k.key.len() == 1 && ch.is_ascii_alphabetic() {
        format!("Key{}", ch.to_ascii_uppercase())
    } else if k.key.len() == 1 && ch.is_ascii_digit() {
        format!("Digit{ch}")
    } else {
        k.key.clone()
    };
    (key, code)
}
