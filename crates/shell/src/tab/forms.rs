//! Forms: native widgets for the page's controls, and submitting them.
//!
//! The page's CSS draws each control's box (border, background, padding);
//! inside it goes a real widget: a gpui-kit text editor for `<input>` and
//! `<textarea>`, our own check marks and radio dots, a dropdown for
//! `<select>`. Buttons and labels are activated through the inline layout's
//! link hit-testing (see `render::activation_url`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use page::{Document, NodeId};
use render::{Control, ControlKind};
use style::ComputedStyle;
use url::Url;

use super::Tab;

pub enum Field {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl Field {
    fn value(&self, cx: &App) -> String {
        match self {
            Field::Line(e) => e.read(cx).value().to_string(),
            Field::Area(e) => e.read(cx).value().to_string(),
        }
    }
}

/// Form state for the page on screen.
#[derive(Default)]
pub struct Forms {
    fields: HashMap<NodeId, Field>,
    /// Checked checkboxes/radios, once the user has changed one (until then
    /// the `checked` attributes decide).
    pub checked: Option<Arc<HashSet<NodeId>>>,
    selected: HashMap<NodeId, usize>,
    open_select: Option<NodeId>,
    /// The text field with keyboard focus (`:focus`).
    pub focused: Option<NodeId>,
    /// The last control toggled, and when: a click on a checkbox inside a
    /// `<label>` reaches both (the label activates the same box).
    last_toggle: Option<(NodeId, std::time::Instant)>,
    _subscriptions: Vec<Subscription>,
}

fn tag(doc: &Document, n: NodeId) -> &str {
    doc.element(n).map(|e| &*e.name.local).unwrap_or("")
}

fn input_type(doc: &Document, n: NodeId) -> String {
    doc.element(n).and_then(|e| e.attr("type")).unwrap_or("text").to_ascii_lowercase()
}

fn by_id(doc: &Document, id: &str) -> Option<NodeId> {
    doc.descendants(doc.root()).find(|&n| doc.element(n).and_then(|e| e.attr("id")) == Some(id))
}

/// The form a control belongs to: `form="id"`, else its nearest `<form>`.
fn form_of(doc: &Document, n: NodeId) -> Option<NodeId> {
    if let Some(id) = doc.element(n).and_then(|e| e.attr("form")) {
        return by_id(doc, id);
    }
    let mut p = doc.parent(n);
    while let Some(id) = p {
        if tag(doc, id) == "form" {
            return Some(id);
        }
        p = doc.parent(id);
    }
    None
}

/// The control a `<label>` is for.
fn labelled(doc: &Document, label: NodeId) -> Option<NodeId> {
    if let Some(id) = doc.element(label).and_then(|e| e.attr("for")) {
        return by_id(doc, id);
    }
    doc.descendants(label).skip(1).find(|&n| matches!(tag(doc, n), "input" | "select" | "textarea" | "button"))
}

impl Tab {
    fn doc(&self) -> Option<Arc<Document>> {
        self.page.as_ref()?.doc.clone()
    }

    fn is_checked(&self, doc: &Document, n: NodeId) -> bool {
        match &self.forms.checked {
            Some(set) => set.contains(&n),
            None => doc.element(n).is_some_and(|e| e.attr("checked").is_some()),
        }
    }

    /// The checked set, materialized from the attributes on first change.
    fn checked_mut(&mut self, doc: &Document) -> &mut HashSet<NodeId> {
        let set = self.forms.checked.get_or_insert_with(|| {
            Arc::new(
                doc.descendants(doc.root())
                    .filter(|&n| tag(doc, n) == "input" && doc.element(n).is_some_and(|e| e.attr("checked").is_some()))
                    .collect(),
            )
        });
        Arc::make_mut(set)
    }

    /// The widget inside a control's box.
    pub(super) fn control_element(
        &mut self,
        control: &Control,
        style: &ComputedStyle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let node = control.node;
        let fg = self.palette.color(style.color);
        // Chrome's control accent: checked boxes are blue everywhere.
        let accent: Hsla = rgb(0x0075ff).into();
        let font = px(style.font_size);
        match &control.kind {
            ControlKind::Text { password, multiline } => {
                if !self.forms.fields.contains_key(&node) {
                    let field = self.new_field(control, *password, *multiline, window, cx);
                    self.forms.fields.insert(node, field);
                }
                let placeholder = self.palette.color(control.placeholder_color);
                let field = match &self.forms.fields[&node] {
                    Field::Line(state) => PlaceholderColor {
                        color: placeholder,
                        child: Input::new(state)
                            .appearance(false)
                            .px_0()
                            .disabled(control.disabled)
                            .text_color(fg)
                            .text_size(font)
                            .size_full()
                            .into_any_element(),
                    }
                    .into_any_element(),
                    Field::Area(state) => Textarea::new(state)
                        .appearance(false)
                        .disabled(control.disabled)
                        .h_full()
                        .into_any_element(),
                };
                div().size_full().flex().items_center().text_color(fg).text_size(font).child(field).into_any_element()
            }
            ControlKind::Checkbox | ControlKind::Radio => {
                let radio = matches!(control.kind, ControlKind::Radio);
                let checked = self.doc().is_some_and(|doc| self.is_checked(&doc, node));
                let edge = if checked { accent } else { rgb(0x767676).into() };
                div()
                    .id(("control", node_key(node)))
                    .size_full()
                    .border_1()
                    .border_color(edge)
                    .when(radio, |d| d.rounded_full())
                    .when(!radio, |d| d.rounded(px(2.)))
                    .bg(if checked && !radio { accent } else { rgb(0xffffff).into() })
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(checked && !radio, |d| d.child(Icon::new(Lucide::Check).size(px(10.)).text_color(gpui_kit::white())))
                    .when(checked && radio, |d| d.child(div().size(px(7.)).rounded_full().bg(accent)))
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(node, window, cx)))
                    .into_any_element()
            }
            ControlKind::Select { options, selected } => {
                let current = self.forms.selected.get(&node).copied().unwrap_or(*selected);
                let label = options.get(current).map(|(_, l)| l.clone()).unwrap_or_default();
                let open = self.forms.open_select == Some(node);
                let menu = open.then(|| {
                    let theme = cx.theme();
                    let items = options.iter().enumerate().map(|(ix, (_, label))| {
                        div()
                            .id(("option", ix))
                            .px_2()
                            .py_0p5()
                            .whitespace_nowrap()
                            .when(ix == current, |d| d.bg(theme.accent))
                            .hover(|d| d.bg(theme.accent))
                            .child(label.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.forms.selected.insert(node, ix);
                                this.forms.open_select = None;
                                cx.notify();
                            }))
                    });
                    deferred(
                        anchored().child(
                            div()
                                .id("select-menu")
                                .occlude()
                                .mt(px(4.))
                                .py_1()
                                .min_w(px(120.))
                                .max_h(px(320.))
                                .overflow_y_scroll()
                                .rounded_md()
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.popover)
                                .text_color(theme.popover_foreground)
                                .text_size(font)
                                .shadow_lg()
                                .children(items)
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                    this.forms.open_select = None;
                                    cx.notify();
                                })),
                        ),
                    )
                    .with_priority(1)
                });
                div()
                    .id(("control", node_key(node)))
                    .size_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .text_color(fg)
                    .text_size(font)
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    // Pages that style selects draw their own arrow (a background image).
                    .when(style.background_url.is_none(), |d| {
                        d.child(Icon::new(Lucide::ChevronDown).size(px(12.)).text_color(fg))
                    })
                    .children(menu)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.forms.open_select = if this.forms.open_select == Some(node) { None } else { Some(node) };
                        cx.notify();
                    }))
                    .into_any_element()
            }
        }
    }

    fn new_field(&mut self, control: &Control, password: bool, multiline: bool, window: &mut Window, cx: &mut Context<Self>) -> Field {
        let node = control.node;
        let placeholder = control.placeholder.clone().unwrap_or_default();
        let value = control.value.clone();
        if multiline {
            let state = cx.new(|cx| TextareaState::new(window, cx).placeholder(placeholder).default_value(value));
            let sub = cx.subscribe_in(&state, window, move |this, _, event: &InputEvent, window, cx| {
                this.field_event(node, event, window, cx)
            });
            self.forms._subscriptions.push(sub);
            Field::Area(state)
        } else {
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value).masked(password));
            let sub = cx.subscribe_in(&state, window, move |this, _, event: &InputEvent, window, cx| {
                this.field_event(node, event, window, cx)
            });
            self.forms._subscriptions.push(sub);
            Field::Line(state)
        }
    }

    fn field_event(&mut self, node: NodeId, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            // Enter in a text field submits its form.
            InputEvent::PressEnter { .. } if matches!(self.forms.fields.get(&node), Some(Field::Line(_))) => {
                if let Some(doc) = self.doc()
                    && let Some(form) = form_of(&doc, node)
                {
                    self.submit(form, None, window, cx);
                }
            }
            InputEvent::Focus => {
                self.forms.focused = Some(node);
                self.interaction_changed(window, cx);
            }
            InputEvent::Blur if self.forms.focused == Some(node) => {
                self.forms.focused = None;
                self.interaction_changed(window, cx);
            }
            _ => {}
        }
    }

    /// A button, label, checkbox... was clicked.
    pub(super) fn activate(&mut self, node: NodeId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc() else { return };
        if std::env::var_os("BROWSER_DEBUG").is_some() {
            eprintln!("activate {node:?} <{}> form={:?}", tag(&doc, node), form_of(&doc, node));
        }
        if doc.element(node).is_some_and(|e| e.attr("disabled").is_some()) {
            return;
        }
        match tag(&doc, node) {
            "label" => {
                if let Some(target) = labelled(&doc, node) {
                    match self.forms.fields.get(&target) {
                        Some(Field::Line(state)) => state.update(cx, |s, cx| s.focus(window, cx)),
                        Some(Field::Area(state)) => state.update(cx, |s, cx| s.focus(window, cx)),
                        None => self.activate(target, window, cx),
                    }
                }
            }
            "button" => match doc.element(node).and_then(|e| e.attr("type")).unwrap_or("submit").to_ascii_lowercase().as_str() {
                "reset" => self.reset(form_of(&doc, node), window, cx),
                "button" => {}
                _ => {
                    if let Some(form) = form_of(&doc, node) {
                        self.submit(form, Some(node), window, cx);
                    }
                }
            },
            "input" => match input_type(&doc, node).as_str() {
                "submit" | "image" => {
                    if let Some(form) = form_of(&doc, node) {
                        self.submit(form, Some(node), window, cx);
                    }
                }
                "reset" => self.reset(form_of(&doc, node), window, cx),
                "checkbox" => {
                    let now = std::time::Instant::now();
                    if self.forms.last_toggle.is_some_and(|(n, t)| n == node && now - t < Duration::from_millis(100)) {
                        return;
                    }
                    self.forms.last_toggle = Some((node, now));
                    let set = self.checked_mut(&doc);
                    if !set.remove(&node) {
                        set.insert(node);
                    }
                    self.form_state_changed(window, cx);
                }
                "radio" => {
                    let name = doc.element(node).and_then(|e| e.attr("name")).map(str::to_owned);
                    let form = form_of(&doc, node);
                    let group: Vec<NodeId> = match &name {
                        Some(name) => doc
                            .descendants(doc.root())
                            .filter(|&n| {
                                tag(&doc, n) == "input"
                                    && input_type(&doc, n) == "radio"
                                    && doc.element(n).and_then(|e| e.attr("name")) == Some(name.as_str())
                                    && form_of(&doc, n) == form
                            })
                            .collect(),
                        None => vec![node],
                    };
                    let set = self.checked_mut(&doc);
                    for n in group {
                        set.remove(&n);
                    }
                    set.insert(node);
                    self.form_state_changed(window, cx);
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Checked state changed: redraw, and restyle for `:checked` rules.
    fn form_state_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        let has_rules = self.page.as_ref().and_then(|p| p.styles.as_ref()).is_some_and(|s| s.has_state_rules());
        if has_rules {
            self.restyle(Duration::ZERO, window, cx);
        }
    }

    fn reset(&mut self, form: Option<NodeId>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc() else { return };
        let in_form = |n: NodeId| form.is_none() || form_of(&doc, n) == form;
        for (&node, field) in &self.forms.fields {
            if !in_form(node) {
                continue;
            }
            let initial: SharedString = match tag(&doc, node) {
                "textarea" => doc.text_content(node).into(),
                _ => doc.element(node).and_then(|e| e.attr("value")).unwrap_or_default().to_owned().into(),
            };
            match field {
                Field::Line(s) => s.update(cx, |s, cx| s.set_value(initial, window, cx)),
                Field::Area(s) => s.update(cx, |s, cx| s.set_value(initial, window, cx)),
            }
        }
        self.forms.selected.retain(|&n, _| !in_form(n));
        self.forms.checked = None;
        self.form_state_changed(window, cx);
    }

    /// The form's data set, in tree order (HTML's "constructing the entry list").
    fn entries(&self, doc: &Document, form: NodeId, submitter: Option<NodeId>, cx: &App) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for n in doc.descendants(form) {
            let Some(e) = doc.element(n) else { continue };
            let Some(name) = e.attr("name").filter(|n| !n.is_empty()) else { continue };
            if e.attr("disabled").is_some() {
                continue;
            }
            let value = match &*e.name.local {
                "input" => match input_type(doc, n).as_str() {
                    "checkbox" | "radio" => {
                        if !self.is_checked(doc, n) {
                            continue;
                        }
                        e.attr("value").unwrap_or("on").to_owned()
                    }
                    "submit" | "image" | "button" | "reset" => {
                        if submitter != Some(n) {
                            continue;
                        }
                        e.attr("value").unwrap_or_default().to_owned()
                    }
                    "file" => continue,
                    _ => match self.forms.fields.get(&n) {
                        Some(f) => f.value(cx),
                        None => e.attr("value").unwrap_or_default().to_owned(),
                    },
                },
                "textarea" => match self.forms.fields.get(&n) {
                    Some(f) => f.value(cx),
                    None => doc.text_content(n),
                },
                "select" => {
                    let options: Vec<NodeId> = doc.descendants(n).filter(|&o| tag(doc, o) == "option").collect();
                    let initial = options
                        .iter()
                        .position(|&o| doc.element(o).is_some_and(|e| e.attr("selected").is_some()))
                        .unwrap_or(0);
                    let ix = self.forms.selected.get(&n).copied().unwrap_or(initial);
                    let Some(&o) = options.get(ix) else { continue };
                    let oe = doc.element(o).expect("option element");
                    oe.attr("value").map(str::to_owned).unwrap_or_else(|| doc.text_content(o).trim().to_owned())
                }
                "button" => {
                    if submitter != Some(n) {
                        continue;
                    }
                    e.attr("value").unwrap_or_default().to_owned()
                }
                _ => continue,
            };
            out.push((name.to_owned(), value));
        }
        out
    }

    /// Submit a form (GET: as the query string; POST: urlencoded body).
    fn submit(&mut self, form: NodeId, submitter: Option<NodeId>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc() else { return };
        let Some(page_url) = self.page.as_ref().map(|p| p.url.clone()) else { return };
        let attr = |name: &str| {
            submitter
                .and_then(|s| doc.element(s))
                .and_then(|e| e.attr(&format!("form{name}")))
                .or_else(|| doc.element(form).and_then(|e| e.attr(name)))
        };
        let mut action: Url = match attr("action").map(str::trim).filter(|a| !a.is_empty()) {
            Some(a) => match doc.base_url().join(a) {
                Ok(u) => u,
                Err(_) => return,
            },
            None => page_url,
        };
        action.set_fragment(None);
        let body: String = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(self.entries(&doc, form, submitter, cx))
            .finish();
        let post = attr("method").is_some_and(|m| m.eq_ignore_ascii_case("post"));
        if post {
            self.pending_post = Some((action.clone(), body));
        } else {
            action.set_query(Some(&body));
        }
        self.navigate(action, window, cx);
    }
}

/// A stable number for a node, for element ids.
fn node_key(n: NodeId) -> usize {
    use slotmap::Key as _;
    n.data().as_ffi() as usize
}

/// gpui-kit's text field takes its placeholder color from the app theme each
/// time it renders. A page's `::placeholder` color goes in by lending the
/// theme that color while the field renders, and taking it back after.
struct PlaceholderColor {
    color: Hsla,
    child: AnyElement,
}

impl IntoElement for PlaceholderColor {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for PlaceholderColor {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let theme = gpui_kit::component::Theme::global_mut(cx);
        let saved = std::mem::replace(&mut theme.muted_foreground, self.color);
        let id = self.child.request_layout(window, cx);
        gpui_kit::component::Theme::global_mut(cx).muted_foreground = saved;
        (id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}
