//! `url()`, gradients, and background layers: images, sizes and positions.

use super::*;

/// Run `f` with `url()` values resolving against `base` (a stylesheet's URL,
/// or the document's for inline styles).
pub fn with_base<R>(base: Option<&url::Url>, f: impl FnOnce() -> R) -> R {
    let previous = CSS_BASE.with(|b| b.replace(base.cloned()));
    let result = f();
    CSS_BASE.with(|b| *b.borrow_mut() = previous);
    result
}

pub(crate) fn resolve_css_url(raw: &str) -> Option<Arc<str>> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('#') {
        return None; // `url(#fragment)` refers to SVG in the page itself
    }
    CSS_BASE.with(|base| match &*base.borrow() {
        Some(base) => base.join(raw).ok(),
        None => url::Url::parse(raw).ok(),
    })
    .map(|u| Arc::from(u.as_str()))
}

/// `url(...)` (quoted or not), resolved.
pub(crate) fn parse_url<'i>(p: &mut Parser<'i>) -> PResult<'i, Option<Arc<str>>> {
    match p.next()?.clone() {
        Token::UnquotedUrl(u) => Ok(resolve_css_url(&u)),
        Token::Function(f) if f.eq_ignore_ascii_case("url") => {
            let raw = p.parse_nested_block(|p| Ok(p.expect_string()?.to_string()))?;
            Ok(resolve_css_url(&raw))
        }
        _ => err(p),
    }
}

/// One color standing in for a gradient: linear gradients by their first
/// stop; radial ones (dot patterns, vignettes) by their last, which is
/// usually what fills most of the box.
pub(crate) fn gradient_color<'i>(p: &mut Parser<'i>, radial: bool) -> PResult<'i, Color> {
    p.parse_nested_block(|p| {
        let mut stops = Vec::new();
        loop {
            if let Ok(c) = p.try_parse(parse_color) {
                stops.push(c);
                continue;
            }
            if p.next().is_err() {
                break;
            }
        }
        // A transparent stop: a pattern (grid lines, dots) or a fade
        // overlay. One solid color would cover the box; drawing none is
        // closer.
        if stops.iter().any(|c| c.is_transparent()) {
            return Ok(Color::TRANSPARENT);
        }
        let pick = if radial { stops.last() } else { stops.first() };
        pick.copied().ok_or_else(|| p.new_error_for_next_token())
    })
}

/// What we understand of a `background` / `mask` value (first image layer).
#[derive(Default)]
pub(crate) struct ImageLayer {
    pub(crate) color: Option<Color>,
    pub(crate) url: Option<Arc<str>>,
    pub(crate) gradient: Option<Color>,
    /// Every layer's gradient, top first.
    pub(crate) full: Vec<Gradient>,
    pub(crate) repeat: Option<BgRepeat>,
    /// An image we can't use (e.g. `image-set()`), still meaning "masked".
    pub(crate) other_image: bool,
    pub(crate) position: Option<BgPos>,
    pub(crate) size: Option<BgSizeSpec>,
}

pub(crate) fn parse_image_layer<'i>(p: &mut Parser<'i>) -> PResult<'i, ImageLayer> {
    let mut layer = ImageLayer::default();
    while !p.is_exhausted() {
        if let Ok(c) = p.try_parse(parse_color) {
            layer.color = Some(c);
            continue;
        }
        if let Ok(url) = p.try_parse(parse_url) {
            if layer.url.is_none() {
                layer.url = url;
            }
            continue;
        }
        if layer.position.is_none()
            && let Ok(pos) = p.try_parse(parse_bg_position)
        {
            layer.position = Some(pos);
            if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                layer.size = p.try_parse(parse_bg_size).ok();
            }
            continue;
        }
        // Other tokens (attachment, origin, commas between layers) are skipped.
        let token = p.next()?.clone();
        if let Token::Ident(id) = &token {
            let r = match id.to_ascii_lowercase().as_str() {
                "no-repeat" => Some(BgRepeat { x: false, y: false }),
                "repeat-x" => Some(BgRepeat { x: true, y: false }),
                "repeat-y" => Some(BgRepeat { x: false, y: true }),
                "repeat" | "space" | "round" => Some(BgRepeat { x: true, y: true }),
                _ => None,
            };
            if layer.repeat.is_none() {
                layer.repeat = r;
            }
        }
        if let Token::Function(name) = token {
            let name = name.to_ascii_lowercase();
            if name.contains("gradient") {
                let state = p.state();
                if layer.gradient.is_none() {
                    layer.gradient = gradient_color(p, name.contains("radial")).ok();
                }
                p.reset(&state);
                if let Ok(g) = parse_gradient(p, &name) {
                    layer.full.push(g);
                }
            } else if name.contains("image") || name == "element" || name == "paint" {
                layer.other_image = true;
            }
        }
    }
    Ok(layer)
}

impl ImageLayer {
    pub(crate) fn gradients(&self) -> Option<Arc<[Gradient]>> {
        (!self.full.is_empty()).then(|| self.full.clone().into())
    }
}

/// A whole `*-gradient()`: the shape and every color stop. (Positioned
/// just after the function token.)
pub(crate) fn parse_gradient<'i>(p: &mut Parser<'i>, name: &str) -> PResult<'i, Gradient> {
    let repeating = name.starts_with("repeating-");
    let radial = name.contains("radial");
    let conic = name.contains("conic");
    if !radial && !conic && !name.contains("linear") {
        return err(p);
    }
    p.parse_nested_block(|p| {
        let mut shape = if conic {
            GradientShape::Conic { from: 0.0, at: BgPos::CENTER }
        } else if radial {
            GradientShape::Radial { circle: false, size: RadialSize::FarthestCorner, at: BgPos::CENTER }
        } else {
            GradientShape::Linear(180.0)
        };
        // The part before the first color, if any.
        let state = p.state();
        let starts_with_color = parse_color(p).is_ok();
        p.reset(&state);
        if !starts_with_color {
            if conic {
                let (mut from, mut at) = (0.0, BgPos::CENTER);
                while !p.is_exhausted() {
                    match p.next()?.clone() {
                        Token::Comma => break,
                        Token::Ident(id) if id.eq_ignore_ascii_case("from") => from = p.try_parse(parse_angle).unwrap_or(0.0),
                        Token::Ident(id) if id.eq_ignore_ascii_case("at") => at = parse_bg_position(p).unwrap_or(BgPos::CENTER),
                        _ => {}
                    }
                }
                shape = GradientShape::Conic { from, at };
            } else if radial {
                let (mut circle, mut size, mut at) = (false, RadialSize::FarthestCorner, BgPos::CENTER);
                let mut lengths = Vec::new();
                while !p.is_exhausted() {
                    let token = p.next()?.clone();
                    match token {
                        Token::Comma => break,
                        Token::Ident(id) => match id.to_ascii_lowercase().as_str() {
                            "circle" => circle = true,
                            "ellipse" => circle = false,
                            "closest-side" => size = RadialSize::ClosestSide,
                            "farthest-side" => size = RadialSize::FarthestSide,
                            "closest-corner" => size = RadialSize::ClosestCorner,
                            "farthest-corner" => size = RadialSize::FarthestCorner,
                            "at" => at = parse_bg_position(p).unwrap_or(BgPos::CENTER),
                            _ => {}
                        },
                        Token::Dimension { value, unit, .. } => lengths.push(stop_px(value, &unit)),
                        _ => {}
                    }
                }
                if let Some(&rx) = lengths.first() {
                    let ry = lengths.get(1).copied().unwrap_or(rx);
                    if lengths.len() == 1 {
                        circle = true;
                    }
                    size = RadialSize::Explicit(rx, ry);
                }
                shape = GradientShape::Radial { circle, size, at };
            } else {
                if let Ok(deg) = p.try_parse(parse_angle) {
                    shape = GradientShape::Linear(deg);
                } else if p.try_parse(|p| p.expect_ident_matching("to")).is_ok() {
                    let (mut x, mut y) = (0.0f32, 0.0f32);
                    while let Ok(side) = p.try_parse(|p| p.expect_ident().cloned()) {
                        match side.to_ascii_lowercase().as_str() {
                            "left" => x = -1.0,
                            "right" => x = 1.0,
                            "top" => y = -1.0,
                            "bottom" => y = 1.0,
                            _ => {}
                        }
                    }
                    shape = match (x, y) {
                        (0.0, y) => GradientShape::Linear(if y < 0.0 { 0.0 } else { 180.0 }),
                        (x, 0.0) => GradientShape::Linear(if x > 0.0 { 90.0 } else { 270.0 }),
                        (x, y) => GradientShape::LinearCorner(x, y),
                    };
                }
                p.expect_comma()?;
            }
        }
        let mut stops = Vec::new();
        while !p.is_exhausted() {
            match p.try_parse(parse_color) {
                Ok(color) => {
                    let mut positions = Vec::new();
                    while let Ok(pos) = p.try_parse(parse_stop_pos) {
                        positions.push(pos);
                    }
                    if positions.is_empty() {
                        stops.push(GradientStop { color, pos: None });
                    }
                    for pos in positions.into_iter().take(2) {
                        stops.push(GradientStop { color, pos: Some(pos) });
                    }
                }
                // A color hint (`10%` alone): ignored.
                Err(_) => {
                    while let Ok(t) = p.next() {
                        if matches!(t, Token::Comma) {
                            break;
                        }
                    }
                    continue;
                }
            }
            if p.is_exhausted() {
                break;
            }
            p.expect_comma()?;
        }
        if stops.len() < 2 {
            return err(p);
        }
        Ok(Gradient { shape, stops, repeating })
    })
}

pub(crate) fn stop_px(value: f32, unit: &str) -> f32 {
    match unit.to_ascii_lowercase().as_str() {
        "em" | "rem" => value * 16.0,
        // Conic stops: angles, as degrees.
        "turn" => value * 360.0,
        "rad" => value.to_degrees(),
        "grad" => value * 0.9,
        _ => value,
    }
}

pub(crate) fn parse_stop_pos<'i>(p: &mut Parser<'i>) -> PResult<'i, StopPos> {
    match p.next()?.clone() {
        Token::Percentage { unit_value, .. } => Ok(StopPos::Fraction(unit_value)),
        Token::Dimension { value, unit, .. } => Ok(StopPos::Px(stop_px(value, &unit))),
        Token::Number { value: 0.0, .. } => Ok(StopPos::Fraction(0.0)),
        _ => err(p),
    }
}

pub(crate) fn parse_bg_size<'i>(p: &mut Parser<'i>) -> PResult<'i, BgSizeSpec> {
    if p.try_parse(|p| p.expect_ident_matching("contain")).is_ok() {
        return Ok(BgSizeSpec::Contain);
    }
    if p.try_parse(|p| p.expect_ident_matching("cover")).is_ok() {
        return Ok(BgSizeSpec::Cover);
    }
    let w = parse_len(p)?;
    let h = p.try_parse(parse_len).unwrap_or(LenSpec::Auto);
    Ok(match (&w, &h) {
        (LenSpec::Auto, LenSpec::Auto) => BgSizeSpec::Auto,
        _ => BgSizeSpec::Explicit(w, h),
    })
}

pub(crate) fn parse_bg_position<'i>(p: &mut Parser<'i>) -> PResult<'i, BgPos> {
    // Each item: an edge keyword, `center`, a percentage (a fraction of the
    // free space) or a length (an offset from the left/top, or from the
    // edge a keyword just named: `right 10px`).
    let mut pos = BgPos::TOP_LEFT;
    let mut seen = 0;
    let mut x_set = false;
    let mut y_set = false;
    // The axis of a keyword waiting for its offset, and which way it counts.
    let mut edge: Option<(bool, f32)> = None;
    while seen < 4 {
        let state = p.state();
        enum Item {
            Edge(bool, f32),
            Center,
            Fraction(f32),
            Px(f32),
        }
        let item = match p.next() {
            Ok(Token::Ident(id)) => match id.to_ascii_lowercase().as_str() {
                "left" => Some(Item::Edge(true, 0.0)),
                "right" => Some(Item::Edge(true, 1.0)),
                "top" => Some(Item::Edge(false, 0.0)),
                "bottom" => Some(Item::Edge(false, 1.0)),
                "center" => Some(Item::Center),
                _ => None,
            },
            Ok(Token::Percentage { unit_value, .. }) => Some(Item::Fraction(*unit_value)),
            Ok(Token::Dimension { value, unit, .. }) => {
                let v = match unit.to_ascii_lowercase().as_str() {
                    "px" => *value,
                    "em" | "rem" => *value * 16.0,
                    _ => 0.0,
                };
                Some(Item::Px(v))
            }
            Ok(Token::Number { value, .. }) => Some(Item::Px(*value)),
            _ => None,
        };
        let Some(item) = item else {
            p.reset(&state);
            break;
        };
        match item {
            Item::Edge(is_x, v) => {
                if is_x {
                    pos.x = v;
                    x_set = true;
                } else {
                    pos.y = v;
                    y_set = true;
                }
                edge = Some((is_x, if v > 0.5 { -1.0 } else { 1.0 }));
                seen += 1;
                continue;
            }
            Item::Center if !x_set && seen == 0 => {
                pos.x = 0.5;
                pos.y = 0.5;
            }
            Item::Center => {
                if x_set {
                    pos.y = 0.5;
                    y_set = true;
                } else {
                    pos.x = 0.5;
                    x_set = true;
                }
            }
            Item::Fraction(v) | Item::Px(v) if edge.is_some() && matches!(item, Item::Px(_)) => {
                // `right 10px`: from that edge.
                let (is_x, sign) = edge.expect("checked");
                if is_x { pos.dx = sign * v } else { pos.dy = sign * v }
            }
            Item::Fraction(v) => {
                if !x_set {
                    pos.x = v;
                    x_set = true;
                } else {
                    pos.y = v;
                    y_set = true;
                }
            }
            Item::Px(v) => {
                if !x_set {
                    pos.x = 0.0;
                    pos.dx = v;
                    x_set = true;
                } else {
                    pos.y = 0.0;
                    pos.dy = v;
                    y_set = true;
                }
            }
        }
        let _ = y_set;
        edge = None;
        seen += 1;
    }
    if seen == 0 { err(p) } else { Ok(pos) }
}

pub(crate) fn parse_background<'i>(p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    let layer = parse_image_layer(p)?;
    out.push(Decl::BackgroundColor(layer.color.unwrap_or(Color::TRANSPARENT)));
    out.push(Decl::BackgroundImage(layer.gradient));
    out.push(Decl::Gradients(layer.gradients()));
    out.push(Decl::BackgroundRepeat(layer.repeat.unwrap_or_default()));
    out.push(Decl::BackgroundUrl(layer.url));
    out.push(Decl::BackgroundSize(layer.size.unwrap_or(BgSizeSpec::Auto)));
    out.push(Decl::BackgroundPosition(layer.position.unwrap_or(BgPos::TOP_LEFT)));
    Ok(())
}

// ---------------------------------------------------------------------------
// Fonts
