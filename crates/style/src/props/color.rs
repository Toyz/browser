//! Colors: names, hex, `rgb()`, `hsl()`, `lab()`, `oklch()`, `color-mix()`...

use super::*;

/// A CSS color from a string (`#fff`, `rgb(0 0 0 / 50%)`, `red`...), as
/// 0xRRGGBBAA; `None` if it isn't one (or depends on context).
pub fn parse_color_str(text: &str) -> Option<u32> {
    let mut p = Parser::new(text.trim());
    match parse_color(&mut p).ok()? {
        Color::Rgba(v) => Some(v),
        Color::LightDark(light, _) => Some(light),
        Color::Foreground | Color::CurrentColor => Some(0x000000ff),
        _ => None,
    }
}

pub(crate) fn parse_color<'i>(p: &mut Parser<'i>) -> PResult<'i, Color> {
    if let Ok(c) = p.try_parse(|p| {
        let name = p.expect_ident()?.to_ascii_lowercase();
        match system_color(&name) {
            Some(c) => Ok(c),
            None => err(p),
        }
    }) {
        return Ok(c);
    }
    if let Ok(c) = p.try_parse(|p| {
        p.expect_function_matching("light-dark")?;
        p.parse_nested_block(|p| {
            let light = parse_color(p)?;
            p.expect_comma()?;
            let dark = parse_color(p)?;
            Ok(match (light, dark) {
                (Color::Rgba(l), Color::Rgba(d)) => Color::LightDark(l, d),
                // Keywords on either side: no way to pick later; light it is.
                _ => light,
            })
        })
    }) {
        return Ok(c);
    }
    let c = cssparser_color::Color::parse(p)?;
    Ok(convert_color(c))
}

pub(crate) fn system_color(name: &str) -> Option<Color> {
    Some(match name {
        "canvastext" | "buttontext" | "fieldtext" | "marktext" | "selecteditemtext" | "highlighttext" => {
            Color::Foreground
        }
        "linktext" | "visitedtext" | "activetext" | "accentcolor" => Color::Link,
        "graytext" => Color::Muted,
        "canvas" | "field" => Color::Rgba(0xffffffff),
        "buttonface" | "buttonborder" => Color::Rgba(0xefefefff),
        "highlight" | "selecteditem" => Color::Rgba(0xb4d5feff),
        "mark" => Color::Rgba(0xffff00ff),
        _ => return None,
    })
}

pub(crate) fn pack(r: f32, g: f32, b: f32, a: f32) -> Color {
    let ch = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    Color::Rgba((ch(r) << 24) | (ch(g) << 16) | (ch(b) << 8) | ch(a))
}

pub(crate) fn gamma(c: f32) -> f32 {
    if c <= 0.0031308 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

pub(crate) fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    if s == 0.0 {
        return (l, l, l);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f32| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

pub(crate) fn oklab_to_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_.powi(3), m_.powi(3), s_.powi(3));
    (
        gamma(4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3),
        gamma(-1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3),
        gamma(-0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3),
    )
}

pub(crate) fn lab_to_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    // CIE Lab (D50) → XYZ → linear sRGB (Bradford-adapted matrix).
    const E: f32 = 216.0 / 24389.0;
    const K: f32 = 24389.0 / 27.0;
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let inv = |f: f32| if f.powi(3) > E { f.powi(3) } else { (116.0 * f - 16.0) / K };
    let x = 0.964_22 * inv(fx);
    let y = if l > K * E { fy.powi(3) } else { l / K };
    let z = 0.825_21 * inv(fz);
    (
        gamma(3.133_856 * x - 1.616_866_7 * y - 0.490_614_6 * z),
        gamma(-0.978_768_4 * x + 1.916_141_5 * y + 0.033_454 * z),
        gamma(0.071_945_3 * x - 0.228_991_4 * y + 1.405_242_7 * z),
    )
}

pub(crate) fn convert_color(c: cssparser_color::Color) -> Color {
    use cssparser_color::Color as C;
    let o = |v: Option<f32>| v.unwrap_or(0.0);
    let alpha = |v: Option<f32>| v.unwrap_or(1.0);
    match c {
        C::CurrentColor => Color::CurrentColor,
        C::Rgba(rgba) => Color::Rgba(
            ((rgba.red as u32) << 24)
                | ((rgba.green as u32) << 16)
                | ((rgba.blue as u32) << 8)
                | (rgba.alpha.clamp(0.0, 1.0) * 255.0).round() as u32,
        ),
        C::Hsl(h) => {
            let (r, g, b) = hsl_to_rgb(o(h.hue), o(h.saturation), o(h.lightness));
            pack(r, g, b, alpha(h.alpha))
        }
        C::Hwb(h) => {
            let (w, bl) = (o(h.whiteness), o(h.blackness));
            let (r, g, b) = if w + bl >= 1.0 {
                let gray = w / (w + bl);
                (gray, gray, gray)
            } else {
                let (r, g, b) = hsl_to_rgb(o(h.hue), 1.0, 0.5);
                let f = |c: f32| c * (1.0 - w - bl) + w;
                (f(r), f(g), f(b))
            };
            pack(r, g, b, alpha(h.alpha))
        }
        C::Lab(c) => {
            let (r, g, b) = lab_to_srgb(o(c.lightness), o(c.a), o(c.b));
            pack(r, g, b, alpha(c.alpha))
        }
        C::Lch(c) => {
            let h = o(c.hue).to_radians();
            let (r, g, b) = lab_to_srgb(o(c.lightness), o(c.chroma) * h.cos(), o(c.chroma) * h.sin());
            pack(r, g, b, alpha(c.alpha))
        }
        C::Oklab(c) => {
            let (r, g, b) = oklab_to_srgb(o(c.lightness), o(c.a), o(c.b));
            pack(r, g, b, alpha(c.alpha))
        }
        C::Oklch(c) => {
            let h = o(c.hue).to_radians();
            let (r, g, b) = oklab_to_srgb(o(c.lightness), o(c.chroma) * h.cos(), o(c.chroma) * h.sin());
            pack(r, g, b, alpha(c.alpha))
        }
        C::ColorFunction(f) => {
            // `color(srgb ...)` and friends; wide-gamut spaces are treated as sRGB.
            let (c1, c2, c3) = (o(f.c1), o(f.c2), o(f.c3));
            let (r, g, b) = if format!("{:?}", f.color_space) == "SrgbLinear" {
                (gamma(c1), gamma(c2), gamma(c3))
            } else {
                (c1, c2, c3)
            };
            pack(r, g, b, alpha(f.alpha))
        }
    }
}
