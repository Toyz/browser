//! Declarations to computed values: `apply`, the fix-ups after it (`finalize`), and the CSS-wide keywords.

use super::*;

pub(super) fn resolve_bg_size(spec: &crate::props::BgSizeSpec, cx: &ResolveCx) -> BgSize {
    use crate::props::BgSizeSpec as S;
    match spec {
        S::Auto => BgSize::Auto,
        S::Contain => BgSize::Contain,
        S::Cover => BgSize::Cover,
        S::Explicit(w, h) => BgSize::Explicit(w.resolve(cx), h.resolve(cx)),
    }
}

pub(super) fn non_auto(l: Length) -> Length {
    if l.is_auto() { Length::ZERO } else { l }
}

/// Apply an `animation-*` longhand list to the animations (cycling it).
pub(super) fn each_animation<T>(s: &mut ComputedStyle, values: &[T], set: impl Fn(&mut Animation, &T)) {
    let Some(list) = &s.animations else { return };
    if values.is_empty() {
        return;
    }
    let mut list = list.to_vec();
    for (i, a) in list.iter_mut().enumerate() {
        set(a, &values[i % values.len()]);
    }
    s.animations = Some(list.into());
}

/// Post-cascade fixups.
pub(super) fn finalize(s: &mut ComputedStyle) {
    let dark = s.dark;
    s.color = scheme_color(s.color, dark);
    for side in Side::ALL {
        s.border_color.set(side, scheme_color(s.border_color.get(side), dark));
    }
    s.background = s.background.map(|c| scheme_color(c, dark));
    s.background_image = s.background_image.map(|c| scheme_color(c, dark));
    for side in Side::ALL {
        if s.border_style.get(side) == BorderStyle::None {
            s.border_width.set(side, 0.0);
        }
        if s.border_color.get(side) == Color::CurrentColor {
            s.border_color.set(side, s.color);
        }
    }
    if let Some(shadows) = &mut s.shadows
        && shadows.iter().any(|sh| matches!(sh.color, Color::CurrentColor | Color::LightDark(..)))
    {
        let color = s.color;
        *shadows = shadows
            .iter()
            .map(|sh| Shadow {
                color: match sh.color {
                    Color::CurrentColor => color,
                    c => scheme_color(c, dark),
                },
                ..*sh
            })
            .collect();
    }
    if s.background == Some(Color::CurrentColor) {
        s.background = Some(s.color);
    }
    if s.background_image == Some(Color::CurrentColor) {
        s.background_image = Some(s.color);
    }
    // Gradient text: one color per text run is all text gets, so the
    // background's first color fills the glyphs and the box shows none.
    if s.clip_text {
        let fill = s.text_fill.unwrap_or(s.color);
        // The glyphs sample the gradient across the box (see TextGradient).
        let scale = match s.background_size {
            BgSize::Explicit(Length::Percent(p), _) if p > 0.0 => p / 100.0,
            _ => 1.0,
        };
        if fill.is_transparent()
            && let Some(g) = s.gradients.as_ref().and_then(|g| g.first())
            && g.stops.len() >= 2
        {
            let n = g.stops.len();
            let mut stops: Vec<(f32, Color)> = Vec::with_capacity(n);
            for (i, st) in g.stops.iter().enumerate() {
                let even = i as f32 / (n - 1) as f32;
                let pos = match st.pos {
                    Some(StopPos::Fraction(f)) => f,
                    _ => even,
                };
                let prev = stops.last().map_or(0.0, |&(p, _)| p);
                stops.push((pos.max(prev), scheme_color(st.color, s.dark)));
            }
            s.text_gradient = Some(Arc::new(TextGradient { stops: stops.into(), scale, pos: s.background_position.x, repeat: s.background_repeat.x }));
        } else if let Some(tg) = &s.text_gradient
            && (tg.pos != s.background_position.x || tg.scale != scale)
            && s.gradients.is_none()
        {
            // An animation frame moving the background.
            s.text_gradient = Some(Arc::new(TextGradient { pos: s.background_position.x, scale, ..(**tg).clone() }));
        }
        if fill.is_transparent() {
            let first = s.gradients.as_ref().and_then(|g| g.first()).and_then(|g| g.stops.first()).map(|st| st.color);
            if let Some(c) = first.or(s.background_image).or(s.background) {
                s.color = scheme_color(c, s.dark);
                s.text_fill = None;
            }
        }
        s.background = None;
        s.background_image = None;
        s.gradients = None;
    } else if let Some(fill) = s.text_fill.filter(|c| !c.is_transparent()) {
        s.color = fill;
        s.text_gradient = None;
    }
    if let Some(layers) = &s.gradients {
        let (color, dark) = (s.color, s.dark);
        let resolve = |c: Color| match scheme_color(c, dark) {
            Color::CurrentColor => color,
            c => c,
        };
        s.gradients = Some(
            layers
                .iter()
                .map(|g| Gradient {
                    stops: g.stops.iter().map(|st| GradientStop { color: resolve(st.color), pos: st.pos }).collect(),
                    ..g.clone()
                })
                .collect(),
        );
    }
    // `display: -webkit-box` with `-webkit-line-clamp` (titles cut to N
    // lines) lays its text out as a block, as browsers do.
    if s.line_clamp.is_some() && s.display == Display::Flex {
        s.display = Display::Block;
    }
    // Out-of-flow boxes are always block-level ("blockified").
    if matches!(s.position, Position::Absolute | Position::Fixed) {
        s.display = s.display.blockified();
    }
}

pub(super) fn apply_wide(s: &mut ComputedStyle, parent: &ComputedStyle, name: &str, wide: Wide) {
    let Some((inherits, copy)) = crate::props::meta::lookup(name) else { return };
    let initial = ComputedStyle::initial();
    let src = match wide {
        Wide::Inherit => parent,
        Wide::Initial => &initial,
        Wide::Unset if inherits => parent,
        Wide::Unset => &initial,
    };
    copy(s, src);
    // Decorations aren't inherited, but a parent's still draw under the
    // child's text: `text-decoration: none` doesn't remove them.
    if matches!(name, "text-decoration" | "text-decoration-line") {
        s.underline |= parent.underline;
        s.strikethrough |= parent.strikethrough;
    }
}

impl Styler {
    pub(super) fn apply(&self, s: &mut ComputedStyle, parent: &ComputedStyle, d: &Decl) {
        let cx = self.resolve_cx(s.font_size, s.query_container);
        let len = |spec: &LenSpec| spec.resolve(&cx);
        match d {
            Decl::Display(v) => s.display = *v,
            Decl::Position(v) => s.position = *v,
            Decl::Inset(side, v) => s.inset.set(*side, len(v)),
            Decl::Margin(side, v) => s.margin.set(*side, len(v)),
            Decl::Padding(side, v) => s.padding.set(*side, non_auto(len(v))),
            Decl::BorderWidth(side, v) => s.border_width.set(*side, len(v).px_or_zero().max(0.0)),
            Decl::BorderStyle(side, v) => s.border_style.set(*side, *v),
            Decl::BorderColor(side, v) => s.border_color.set(*side, *v),
            Decl::BorderRadius(corner, v) => s.border_radius[*corner] = non_auto(len(v)),
            Decl::BackgroundColor(c) => s.background = Some(*c),
            Decl::BackgroundImage(c) => s.background_image = *c,
            Decl::Gradients(g) => s.gradients = g.clone(),
            Decl::BackgroundRepeat(r) => s.background_repeat = *r,
            Decl::BackgroundUrl(u) => s.background_url = u.clone(),
            Decl::BackgroundSize(v) => s.background_size = resolve_bg_size(v, &cx),
            Decl::BackgroundPosition(v) => s.background_position = *v,
            Decl::MaskUrl(u) => s.mask_url = u.clone(),
            Decl::MaskSize(v) => s.mask_size = resolve_bg_size(v, &cx),
            Decl::MaskPosition(v) => s.mask_position = *v,
            Decl::Width(v) => {
                s.width = len(v);
                // (`width: fit-content` follows this with `FitWidth(true)`.)
                s.fit_width = false;
            }
            Decl::Height(v) => s.height = len(v),
            Decl::MinWidth(v) => s.min_width = len(v),
            Decl::MinHeight(v) => s.min_height = len(v),
            Decl::MaxWidth(v) => s.max_width = len(v),
            Decl::MaxHeight(v) => s.max_height = len(v),
            Decl::FlexDirection(v) => s.flex_direction = *v,
            Decl::FlexWrap(v) => s.flex_wrap = *v,
            Decl::JustifyContent(v) => s.justify_content = Some(*v),
            Decl::AlignItems(v) => s.align_items = Some(*v),
            Decl::AlignContent(v) => s.align_content = Some(*v),
            Decl::AlignSelf(v) => s.align_self = *v,
            Decl::RowGap(v) => s.row_gap = non_auto(len(v)),
            Decl::ColumnGap(v) => s.column_gap = non_auto(len(v)),
            Decl::FlexGrow(v) => s.flex_grow = v.max(0.0),
            Decl::FlexShrink(v) => s.flex_shrink = v.max(0.0),
            Decl::FlexBasis(v) => s.flex_basis = len(v),
            Decl::GridColumns(GridCols::None) => {
                s.grid_columns = None;
                s.grid_auto = None;
            }
            Decl::GridColumns(GridCols::Count(n)) => {
                s.grid_columns = Some(*n);
                s.grid_auto = None;
            }
            Decl::GridColumns(GridCols::AutoFill(min, fit)) => {
                let min = match len(min) {
                    Length::Px(v) if v > 0.0 => v,
                    _ => 200.0,
                };
                // Layout knows the container's width better (see
                // `grid_auto`); this is the fallback.
                s.grid_columns = Some(((self.env.width - 32.0) / min).floor().clamp(1.0, 12.0) as u16);
                s.grid_auto = Some((min, *fit));
            }
            Decl::GridColumn(v) => s.grid_column = *v,
            Decl::GridTracks(None) => s.grid_tracks = None,
            Decl::GridContentMin(v) => s.grid_content_min = *v,
            Decl::GridTemplateFull { rows, value } => {
                let value = value.as_ref().map(|v| Arc::new(resolve_template(v, &len)));
                if *rows { s.grid_template_rows = value } else { s.grid_template_columns = value }
            }
            Decl::GridAutoTracks { rows, tracks } => {
                let tracks: Arc<[TrackSize]> = tracks.iter().map(|t| resolve_track(t, &len)).collect();
                if *rows { s.grid_auto_rows = tracks } else { s.grid_auto_columns = tracks }
            }
            Decl::GridAutoFlow { column, dense } => s.grid_auto_flow = (*column, *dense),
            Decl::GridPlace { row, end, line } => {
                let lines = if *row { &mut s.grid_row_lines } else { &mut s.grid_column_lines };
                if *end { lines.1 = line.clone() } else { lines.0 = line.clone() }
            }
            Decl::JustifyItems(v) => s.justify_items = *v,
            Decl::JustifySelf(v) => s.justify_self = *v,
            Decl::GridTracks(Some(specs)) => {
                let tracks: Vec<Track> = specs
                    .iter()
                    .map(|t| match t {
                        TrackSpec::Len(l) => match len(l) {
                            Length::Px(v) => Track::Px(v),
                            _ => Track::Auto,
                        },
                        TrackSpec::Fr(f) => Track::Fr(*f),
                        TrackSpec::Max(l) => match len(l) {
                            Length::Px(v) => Track::Max(v),
                            _ => Track::Fr(1.0),
                        },
                        TrackSpec::Auto => Track::Auto,
                    })
                    .collect();
                // The column count follows the tracks.
                s.grid_columns = Some(tracks.len().clamp(1, 64) as u16);
                s.grid_tracks = Some(tracks.into());
            }
            Decl::GridAreas(v) => s.grid_areas = v.clone(),
            Decl::GridArea(v) => s.grid_area = v.clone(),
            Decl::GridRowStart(v) => s.grid_row_start = *v,
            Decl::FitWidth(v) => s.fit_width = *v,
            Decl::GridColumnStart(v) => s.grid_column_start = *v,
            Decl::Overflow(v) => s.overflow = *v,
            Decl::Opacity(v) => s.opacity = *v,
            Decl::Translate(x, y) => {
                // Percentages are of the box itself, so keep them apart.
                let offset = |spec: &LenSpec| match spec {
                    LenSpec::Lin(l) => Offset { px: len(&LenSpec::Lin(Lin { pct: 0.0, ..*l })).px_or_zero(), pct: l.pct },
                    other => match len(other) {
                        Length::Px(v) => Offset { px: v, pct: 0.0 },
                        Length::Percent(p) => Offset { px: 0.0, pct: p },
                        Length::Auto => Offset::default(),
                    },
                };
                s.translate = (offset(x), offset(y));
            }
            Decl::Rotate(v) => s.rotate = *v,
            Decl::ClipText(v) => s.clip_text = *v,
            Decl::BgClipBorder(v) => s.bg_clip_border = Some(v.clone()),
            Decl::AspectRatio(v) => s.aspect_ratio = *v,
            Decl::TextFill(c) => s.text_fill = Some(*c),
            Decl::SvgPaint(name, value) => {
                let mut list: Vec<SvgPaint> = s.svg_paint.as_deref().map(<[_]>::to_vec).unwrap_or_default();
                list.retain(|(n, _)| n != name);
                list.push((name.clone(), value.clone()));
                s.svg_paint = Some(list.into());
            }
            Decl::Scale(x, y) => s.scale = (*x, *y),
            Decl::TransitionProperty(v) => Arc::make_mut(s.transitions.get_or_insert_default()).properties = v.clone(),
            Decl::TransitionDuration(v) => Arc::make_mut(s.transitions.get_or_insert_default()).durations = v.clone(),
            Decl::TransitionDelay(v) => Arc::make_mut(s.transitions.get_or_insert_default()).delays = v.clone(),
            Decl::TransitionTiming(v) => Arc::make_mut(s.transitions.get_or_insert_default()).easings = v.clone(),
            Decl::Transition(items) => {
                s.transitions = Some(Arc::new(Transitions {
                    properties: items.iter().map(|i| i.0.clone()).collect(),
                    durations: items.iter().map(|i| i.1).collect(),
                    delays: items.iter().map(|i| i.2).collect(),
                    easings: items.iter().map(|i| i.3).collect(),
                }))
            }
            Decl::Animation(v) => s.animations = v.clone(),
            Decl::AnimationName(names) => {
                let old = s.animations.clone();
                let list: Vec<Animation> = names
                    .iter()
                    .filter(|n| &***n != "none")
                    .enumerate()
                    .map(|(i, name)| {
                        let base = old.as_ref().and_then(|o| o.get(i).cloned());
                        Animation {
                            name: name.clone(),
                            ..base.unwrap_or(Animation {
                                name: name.clone(),
                                duration: 0.0,
                                delay: 0.0,
                                easing: Easing::EASE,
                                iterations: 1.0,
                                direction: AnimDirection::Normal,
                                fill_forwards: false,
                                fill_backwards: false,
                            })
                        }
                    })
                    .collect();
                s.animations = (!list.is_empty()).then(|| list.into());
            }
            Decl::AnimationDuration(v) => each_animation(s, v, |a, x| a.duration = *x),
            Decl::AnimationDelay(v) => each_animation(s, v, |a, x| a.delay = *x),
            Decl::AnimationTiming(v) => each_animation(s, v, |a, x| a.easing = *x),
            Decl::AnimationIterations(v) => each_animation(s, v, |a, x| a.iterations = *x),
            Decl::AnimationDirection(v) => each_animation(s, v, |a, x| a.direction = *x),
            Decl::AnimationFill(v) => each_animation(s, v, |a, x| (a.fill_forwards, a.fill_backwards) = *x),
            Decl::BoxShadow(None) => s.shadows = None,
            Decl::BoxShadow(Some(specs)) => {
                let px = |l: Option<&LenSpec>| l.map(|l| len(l).px_or_zero()).unwrap_or(0.0);
                let shadows: Vec<Shadow> = specs
                    .iter()
                    .map(|sp| Shadow {
                        x: px(sp.lengths.first()),
                        y: px(sp.lengths.get(1)),
                        blur: px(sp.lengths.get(2)).max(0.0),
                        spread: px(sp.lengths.get(3)),
                        color: sp.color,
                        inset: sp.inset,
                    })
                    // `0 0 #0000` placeholders (Tailwind's ring layers) draw nothing.
                    .filter(|sh| !sh.color.is_transparent())
                    .collect();
                s.shadows = (!shadows.is_empty()).then(|| shadows.into());
            }
            Decl::Filter(None) => s.filter = None,
            Decl::Filter(Some(specs)) => {
                let px = |l: Option<&LenSpec>| l.map(|l| len(l).px_or_zero()).unwrap_or(0.0);
                let list: Vec<Filter> = specs
                    .iter()
                    .map(|f| match f {
                        FilterSpec::Color(c) => *c,
                        FilterSpec::Blur(l) => Filter::Blur(px(Some(l)).max(0.0)),
                        FilterSpec::DropShadow(sp) => Filter::DropShadow(Shadow {
                            x: px(sp.lengths.first()),
                            y: px(sp.lengths.get(1)),
                            blur: px(sp.lengths.get(2)).max(0.0),
                            spread: 0.0,
                            color: sp.color,
                            inset: false,
                        }),
                    })
                    .collect();
                s.filter = (!list.is_empty()).then(|| list.into());
            }
            Decl::Masked(v) => s.masked = *v,
            Decl::Content(v) => s.content = v.clone(),
            Decl::Float(v) => s.float = *v,
            Decl::BorderBox(v) => s.border_box = *v,
            Decl::Cursor(v) => s.cursor = *v,
            Decl::PointerEventsNone(v) => s.pointer_events_none = *v,
            Decl::ZIndex(v) => s.z_index = *v,
            Decl::ObjectFit(v) => s.object_fit = *v,
            Decl::TextEllipsis(v) => s.text_ellipsis = *v,
            Decl::ContainerType(v) => s.container_type = *v,
            Decl::LineClamp(v) => s.line_clamp = *v,
            Decl::ColorScheme(v) => s.dark = *v && self.env.dark,
            Decl::VerticalAlign(crate::props::VAlignSpec::Keyword(k)) => s.vertical_align = *k,
            Decl::VerticalAlign(crate::props::VAlignSpec::Len(l)) => {
                s.vertical_align = match len(l) {
                    Length::Px(v) => VerticalAlign::Shift(v),
                    // Percentages are of the line height.
                    Length::Percent(p) => VerticalAlign::Shift(p / 100.0 * s.font_size * 1.2),
                    Length::Auto => VerticalAlign::Baseline,
                }
            }
            Decl::Clear(v) => s.clear = *v,
            Decl::TextDecoration { underline, strikethrough } => {
                s.underline = parent.underline || *underline;
                s.strikethrough = parent.strikethrough || *strikethrough;
            }
            Decl::FontSize(spec) => {
                let parent_size = parent.font_size;
                s.font_size = match spec {
                    FontSizeSpec::Smaller => parent_size / 1.2,
                    FontSizeSpec::Larger => parent_size * 1.2,
                    FontSizeSpec::Len(l) => match l.resolve(&self.resolve_cx(parent_size, parent.query_container)) {
                        Length::Px(v) => v,
                        Length::Percent(p) => parent_size * p / 100.0,
                        Length::Auto => parent_size,
                    },
                }
                .clamp(1.0, 1000.0);
            }
            Decl::LineHeight(spec) => {
                s.line_height = match spec {
                    LineHeightSpec::Normal => LineHeight::Normal,
                    LineHeightSpec::Number(n) => LineHeight::Number(*n),
                    LineHeightSpec::Len(l) => match len(l) {
                        Length::Px(v) => LineHeight::Px(v),
                        Length::Percent(p) => LineHeight::Px(s.font_size * p / 100.0),
                        Length::Auto => LineHeight::Normal,
                    },
                }
            }
            Decl::FontWeight(WeightSpec::Abs(w)) => s.font_weight = *w,
            Decl::FontWeight(WeightSpec::Bolder) => {
                s.font_weight = match parent.font_weight {
                    0..350 => 400,
                    350..550 => 700,
                    _ => 900,
                }
            }
            Decl::FontWeight(WeightSpec::Lighter) => {
                s.font_weight = match parent.font_weight {
                    0..550 => 100,
                    550..750 => 400,
                    _ => 700,
                }
            }
            Decl::Italic(v) => s.italic = *v,
            Decl::FontFamily(v) => s.font_family = *v,
            Decl::FontNames(v) => s.font_names = v.clone(),
            Decl::Color(Color::CurrentColor) => s.color = parent.color,
            Decl::Color(c) => s.color = *c,
            Decl::WhiteSpace(v) => s.white_space = *v,
            Decl::TextAlign(v) => s.text_align = *v,
            Decl::ListStyle(v) => s.list_style = *v,
            Decl::TextTransform(v) => s.text_transform = *v,
            Decl::Visible(v) => s.visible = *v,
            Decl::Wide(name, wide) => apply_wide(s, parent, name, *wide),
            Decl::Custom(..) | Decl::Unresolved { .. } => {}
        }
    }

    pub(super) fn resolve_cx(&self, font_size: f32, container: Option<(f32, f32)>) -> ResolveCx {
        let (container_width, container_height) = container.unwrap_or((self.env.width, self.env.height));
        ResolveCx {
            font_size,
            root_font_size: self.root_font_size.get(),
            viewport_width: self.env.width,
            viewport_height: self.env.height,
            container_width,
            container_height,
        }
    }
}


/// A track size with its lengths resolved.
fn resolve_track(t: &TrackSizeSpec, len: &impl Fn(&LenSpec) -> Length) -> TrackSize {
    let length = |l: &LenSpec| match len(l) {
        Length::Px(v) => TrackSize::Px(v),
        Length::Percent(p) => TrackSize::Percent(p / 100.0),
        _ => TrackSize::Auto,
    };
    match t {
        TrackSizeSpec::Len(l) => length(l),
        TrackSizeSpec::Fr(f) => TrackSize::Fr(*f),
        TrackSizeSpec::Auto => TrackSize::Auto,
        TrackSizeSpec::MinContent => TrackSize::MinContent,
        TrackSizeSpec::MaxContent => TrackSize::MaxContent,
        TrackSizeSpec::FitContent(l) => TrackSize::FitContent(len(l).px_or_zero()),
        TrackSizeSpec::MinMax(min, max) => TrackSize::MinMax(Box::new(resolve_track(min, len)), Box::new(resolve_track(max, len))),
    }
}

fn resolve_template(t: &GridTemplateSpec, len: &impl Fn(&LenSpec) -> Length) -> GridTemplate {
    GridTemplate {
        entries: t
            .entries
            .iter()
            .map(|e| match e {
                TemplateEntrySpec::Track(t) => TemplateEntry::Track(resolve_track(t, len)),
                TemplateEntrySpec::Repeat { count, tracks, names } => TemplateEntry::Repeat {
                    count: *count,
                    tracks: tracks.iter().map(|t| resolve_track(t, len)).collect(),
                    names: names.clone(),
                },
            })
            .collect(),
        names: t.names.clone(),
    }
}
