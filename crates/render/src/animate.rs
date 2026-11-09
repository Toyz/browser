//! Animating paint: interpolating the paint properties of two computed
//! styles (what CSS transitions and `@keyframes` animate here). Like a
//! browser compositor's fast path, only properties that don't move anything
//! are animated: colors, backgrounds, opacity, translation, shadows, radii.

use style::{Color, ComputedStyle, Length, Shadow};

/// Colors as 0xRRGGBBAA, resolving the UA keywords (light palette).
fn rgba(c: Color) -> Option<u32> {
    Some(match c {
        Color::Rgba(v) | Color::LightDark(v, _) => v,
        Color::Foreground => 0x1a1a1aff,
        Color::Link => 0x1a0dabff,
        Color::Muted => 0x6b6b6bff,
        Color::CurrentColor => return None,
    })
}

fn mix_u32(a: u32, b: u32, t: f32) -> u32 {
    let ch = |v: u32, s: u32| ((v >> s) & 0xff) as f32;
    let mut out = 0;
    for s in [24, 16, 8, 0] {
        let v = ch(a, s) + (ch(b, s) - ch(a, s)) * t;
        out |= (v.round().clamp(0.0, 255.0) as u32) << s;
    }
    out
}

pub fn mix_color(a: Color, b: Color, t: f32) -> Color {
    match (rgba(a), rgba(b)) {
        // From/to transparent: keep the other color's hue, fade alpha.
        (Some(x), Some(y)) if x & 0xff == 0 => Color::Rgba(mix_u32(y & 0xffffff00, y, t)),
        (Some(x), Some(y)) if y & 0xff == 0 => Color::Rgba(mix_u32(x, x & 0xffffff00, t)),
        (Some(x), Some(y)) => Color::Rgba(mix_u32(x, y, t)),
        _ if t < 0.5 => a,
        _ => b,
    }
}

fn mix_f(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn mix_len(a: Length, b: Length, t: f32) -> Length {
    match (a, b) {
        (Length::Px(x), Length::Px(y)) => Length::Px(mix_f(x, y, t)),
        (Length::Percent(x), Length::Percent(y)) => Length::Percent(mix_f(x, y, t)),
        (Length::Px(0.0), Length::Percent(y)) => Length::Percent(y * t),
        (Length::Percent(x), Length::Px(0.0)) => Length::Percent(x * (1.0 - t)),
        _ if t < 0.5 => a,
        _ => b,
    }
}

fn mix_background(a: Option<Color>, b: Option<Color>, t: f32) -> Option<Color> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(mix_color(a.unwrap_or(Color::TRANSPARENT), b.unwrap_or(Color::TRANSPARENT), t)),
    }
}

fn mix_shadows(a: &ComputedStyle, b: &ComputedStyle, t: f32) -> Option<std::sync::Arc<[Shadow]>> {
    let none: [Shadow; 0] = [];
    let (x, y) = (a.shadows.as_deref().unwrap_or(&none), b.shadows.as_deref().unwrap_or(&none));
    if x.is_empty() && y.is_empty() {
        return None;
    }
    let n = x.len().max(y.len());
    // Missing layers animate from/to a transparent copy of the other side.
    let get = |s: &[Shadow], other: &[Shadow], i: usize| {
        s.get(i).copied().unwrap_or_else(|| Shadow { color: Color::TRANSPARENT, ..other[i] })
    };
    Some(
        (0..n)
            .map(|i| {
                let (p, q) = (get(x, y, i), get(y, x, i));
                Shadow {
                    x: mix_f(p.x, q.x, t),
                    y: mix_f(p.y, q.y, t),
                    blur: mix_f(p.blur, q.blur, t),
                    spread: mix_f(p.spread, q.spread, t),
                    color: mix_color(p.color, q.color, t),
                    inset: if t < 0.5 { p.inset } else { q.inset },
                }
            })
            .collect(),
    )
}

/// `to`, with its paint properties `t` of the way from `from`'s.
pub fn mix(from: &ComputedStyle, to: &ComputedStyle, t: f32) -> ComputedStyle {
    let mut s = to.clone();
    s.color = mix_color(from.color, to.color, t);
    // Gradient text sliding (`background-position` on clipped text).
    if let (Some(a), Some(b)) = (&from.text_gradient, &to.text_gradient)
        && a.stops == b.stops
    {
        s.text_gradient = Some(std::sync::Arc::new(style::TextGradient { pos: mix_f(a.pos, b.pos, t), scale: mix_f(a.scale, b.scale, t), ..(**b).clone() }));
    }
    s.background = mix_background(from.background, to.background, t);
    s.background_image = mix_background(from.background_image, to.background_image, t);
    for side in style::Side::ALL {
        s.border_color.set(side, mix_color(from.border_color.get(side), to.border_color.get(side), t));
    }
    for (r, (a, b)) in s.border_radius.iter_mut().zip(from.border_radius.iter().zip(&to.border_radius)) {
        *r = mix_len(*a, *b, t);
    }
    s.opacity = mix_f(from.opacity, to.opacity, t);
    s.background_position = style::BgPos {
        x: mix_f(from.background_position.x, to.background_position.x, t),
        y: mix_f(from.background_position.y, to.background_position.y, t),
    };
    let mix_offset = |a: style::Offset, b: style::Offset| style::Offset { px: mix_f(a.px, b.px, t), pct: mix_f(a.pct, b.pct, t) };
    s.translate = (mix_offset(from.translate.0, to.translate.0), mix_offset(from.translate.1, to.translate.1));
    s.rotate = mix_f(from.rotate, to.rotate, t);
    s.scale = (mix_f(from.scale.0, to.scale.0, t), mix_f(from.scale.1, to.scale.1, t));
    s.shadows = mix_shadows(from, to, t);
    if t < 0.5 {
        s.gradients = from.gradients.clone();
        s.underline = from.underline;
        s.strikethrough = from.strikethrough;
        s.visible = from.visible;
    }
    s
}

/// The CSS property names whose (animatable) values differ.
pub fn changed_properties(a: &ComputedStyle, b: &ComputedStyle) -> Vec<&'static str> {
    let mut out = Vec::new();
    if a.color != b.color {
        out.push("color");
    }
    if a.background != b.background || a.background_image != b.background_image || a.gradients != b.gradients {
        out.push("background-color");
    }
    if a.border_color != b.border_color {
        out.push("border-color");
    }
    if a.opacity != b.opacity {
        out.push("opacity");
    }
    if a.translate != b.translate || a.rotate != b.rotate || a.scale != b.scale {
        out.push("transform");
    }
    if a.shadows != b.shadows {
        out.push("box-shadow");
    }
    if a.border_radius != b.border_radius {
        out.push("border-radius");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_mix() {
        assert_eq!(mix_color(Color::Rgba(0x000000ff), Color::Rgba(0xffffffff), 0.5), Color::Rgba(0x808080ff));
        // From transparent keeps the target's hue.
        assert_eq!(mix_color(Color::TRANSPARENT, Color::Rgba(0xff0000ff), 0.5), Color::Rgba(0xff000080));
    }

    #[test]
    fn easing_curves() {
        let ease = style::Easing::EASE;
        assert!((ease.apply(0.5) - 0.8024).abs() < 0.01, "{}", ease.apply(0.5));
        assert_eq!(style::Easing::LINEAR.apply(0.3), 0.3);
        assert_eq!(style::Easing::Steps(4, false).apply(0.3), 0.25);
    }
}
