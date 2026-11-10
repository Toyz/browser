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
    if let Ok(c) = p.try_parse(parse_color_mix) {
        return Ok(c);
    }
    if let Ok(c) = p.try_parse(parse_color_with_math) {
        return Ok(c);
    }
    let c = cssparser_color::Color::parse(p)?;
    Ok(convert_color(c))
}

/// A math function's result's kind.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Unit {
    Number,
    Percent,
    Degrees,
}

type Value = (f64, Unit);

/// `+`/`-` of `a` and `b` (same kinds only).
fn add(a: Value, b: Value, sign: f64) -> Option<Value> {
    (a.1 == b.1).then_some((a.0 + sign * b.0, a.1))
}

/// A sum: products joined by `+`/`-`.
fn math_sum<'i>(p: &mut Parser<'i>) -> PResult<'i, Value> {
    let mut acc = math_product(p)?;
    loop {
        let sign = p.try_parse(|p| match p.next()? {
            Token::Delim('+') => Ok(1.0),
            Token::Delim('-') => Ok(-1.0),
            _ => Err(ParseError::<()>::custom(())),
        });
        let Ok(sign) = sign else { break };
        let b = math_product(p)?;
        acc = add(acc, b, sign).ok_or_else(|| ParseError::custom(()))?;
    }
    Ok(acc)
}

fn math_product<'i>(p: &mut Parser<'i>) -> PResult<'i, Value> {
    let mut acc = math_value(p)?;
    loop {
        let op = p.try_parse(|p| match p.next()? {
            Token::Delim('*') => Ok(true),
            Token::Delim('/') => Ok(false),
            _ => Err(ParseError::<()>::custom(())),
        });
        let Ok(multiply) = op else { break };
        let b = math_value(p)?;
        acc = if multiply {
            match (acc.1, b.1) {
                (Unit::Number, u) => (acc.0 * b.0, u),
                (u, Unit::Number) => (acc.0 * b.0, u),
                _ => return Err(ParseError::custom(())),
            }
        } else {
            if b.1 != Unit::Number || b.0 == 0.0 {
                return Err(ParseError::custom(()));
            }
            (acc.0 / b.0, acc.1)
        };
    }
    Ok(acc)
}

/// `a, b, c` inside a math function.
fn math_args<'i>(p: &mut Parser<'i>) -> PResult<'i, Vec<Value>> {
    let mut args = vec![math_sum(p)?];
    while p.try_parse(|p| p.expect_comma()).is_ok() {
        args.push(math_sum(p)?);
    }
    Ok(args)
}

fn math_value<'i>(p: &mut Parser<'i>) -> PResult<'i, Value> {
    let token = p.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok((f64::from(value), Unit::Number)),
        Token::Percentage { unit_value, .. } => Ok((f64::from(unit_value) * 100.0, Unit::Percent)),
        Token::Dimension { value, ref unit, .. } => {
            let v = f64::from(value);
            match unit.to_ascii_lowercase().as_str() {
                "deg" => Ok((v, Unit::Degrees)),
                "rad" => Ok((v.to_degrees(), Unit::Degrees)),
                "grad" => Ok((v * 0.9, Unit::Degrees)),
                "turn" => Ok((v * 360.0, Unit::Degrees)),
                _ => Err(ParseError::<()>::custom(())),
            }
        }
        Token::ParenthesisBlock => p.parse_nested_block(math_sum),
        Token::Function(ref name) => {
            let name = name.to_ascii_lowercase();
            p.parse_nested_block(|p| {
                let args = math_args(p)?;
                let same = args.iter().all(|a| a.1 == args[0].1);
                let pick = |f: fn(f64, f64) -> f64| args.iter().map(|a| a.0).reduce(f).map(|v| (v, args[0].1));
                let v = match (name.as_str(), args.len()) {
                    ("calc", 1) => Some(args[0]),
                    ("min", _) if same => pick(f64::min),
                    ("max", _) if same => pick(f64::max),
                    ("clamp", 3) if same => Some((args[1].0.clamp(args[0].0, args[2].0.max(args[0].0)), args[0].1)),
                    _ => None,
                };
                v.ok_or_else(|| ParseError::custom(()))
            })
        }
        _ => Err(ParseError::custom(())),
    }
}

/// `rgb()`/`hsl()`/`hwb()` with math in its arguments (GitHub's labels:
/// `hsl(55, calc(99 * 1%), calc((49 + ...) * 1%))`, after `var()`s): the
/// math worked out, then parsed as usual.
fn parse_color_with_math<'i>(p: &mut Parser<'i>) -> PResult<'i, Color> {
    let name = p.expect_function()?.to_ascii_lowercase();
    if !matches!(name.as_str(), "rgb" | "rgba" | "hsl" | "hsla" | "hwb") {
        return err(p);
    }
    let text = p.parse_nested_block(|p| {
        let mut out = String::new();
        let mut math = false;
        while !p.is_exhausted() {
            let token = p.next()?.clone();
            let piece = match &token {
                Token::Function(f) if matches!(f.to_ascii_lowercase().as_str(), "calc" | "min" | "max" | "clamp") => {
                    math = true;
                    let f = f.to_ascii_lowercase();
                    let v = p.parse_nested_block(|p| {
                        let args = math_args(p)?;
                        let same = args.iter().all(|a| a.1 == args[0].1);
                        let pick = |g: fn(f64, f64) -> f64| args.iter().map(|a| a.0).reduce(g).map(|v| (v, args[0].1));
                        let v = match (f.as_str(), args.len()) {
                            ("calc", 1) => Some(args[0]),
                            ("min", _) if same => pick(f64::min),
                            ("max", _) if same => pick(f64::max),
                            ("clamp", 3) if same => Some((args[1].0.clamp(args[0].0, args[2].0.max(args[0].0)), args[0].1)),
                            _ => None,
                        };
                        v.ok_or_else(|| ParseError::custom(()))
                    })?;
                    match v.1 {
                        Unit::Number => format!("{}", v.0),
                        Unit::Percent => format!("{}%", v.0),
                        Unit::Degrees => format!("{}deg", v.0),
                    }
                }
                Token::Number { value, .. } => format!("{value}"),
                Token::Percentage { unit_value, .. } => format!("{}%", unit_value * 100.0),
                Token::Dimension { value, unit, .. } => format!("{value}{unit}"),
                Token::Comma => ",".into(),
                Token::Delim('/') => "/".into(),
                Token::Ident(i) => i.to_string(),
                _ => return Err(ParseError::custom(())),
            };
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&piece);
        }
        if math { Ok(out) } else { Err(ParseError::custom(())) }
    }).map_err(|_| ParseError::custom(()))?;
    let text = format!("{name}({text})");
    let mut input = Parser::new(&text);
    match cssparser_color::Color::parse(&mut input) {
        Ok(c) => Ok(convert_color(c)),
        Err(_) => err(p),
    }
}

/// `color-mix(in <space>, <color> [<p>%]?, <color> [<p>%]?)`: mixed with
/// premultiplied alpha, in sRGB (close to the requested space for the
/// "this color at 20%" mixes stylesheets use it for).
fn parse_color_mix<'i>(p: &mut Parser<'i>) -> PResult<'i, Color> {
    p.expect_function_matching("color-mix")?;
    p.parse_nested_block(|p| {
        p.expect_ident_matching("in")?;
        // The space, and a hue interpolation method (`shorter hue`).
        p.expect_ident()?;
        while p.try_parse(|p| p.expect_comma()).is_err() {
            p.next()?;
        }
        let part = |p: &mut Parser<'i>| -> PResult<'i, (Color, Option<f32>)> {
            let pct = |p: &mut Parser<'i>| p.try_parse(|p| p.expect_percentage()).ok();
            let before = pct(p);
            let color = parse_color(p)?;
            Ok((color, before.or_else(|| pct(p))))
        };
        let (a, pa) = part(p)?;
        p.expect_comma()?;
        let (b, pb) = part(p)?;
        let (Color::Rgba(a) | Color::LightDark(a, _), Color::Rgba(b) | Color::LightDark(b, _)) = (a, b) else {
            return err(p);
        };
        // Percentages: one implies the other; both scale to 100%, and a
        // sum under it lowers the alpha.
        let (pa, pb) = match (pa, pb) {
            (None, None) => (0.5, 0.5),
            (Some(x), None) => (x, 1.0 - x),
            (None, Some(y)) => (1.0 - y, y),
            (Some(x), Some(y)) => (x, y),
        };
        let sum = pa + pb;
        if sum <= 0.0 {
            return err(p);
        }
        let (wa, wb) = (pa / sum, pb / sum);
        let ch = |v: u32, shift: u32| ((v >> shift) & 0xff) as f32 / 255.0;
        let (aa, ab) = (ch(a, 0), ch(b, 0));
        let alpha = aa * wa + ab * wb;
        let mix = |shift: u32| {
            if alpha <= 0.0 { 0.0 } else { (ch(a, shift) * aa * wa + ch(b, shift) * ab * wb) / alpha }
        };
        Ok(pack(mix(24), mix(16), mix(8), alpha * sum.min(1.0)))
    })
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

#[cfg(test)]
mod math_tests {
    use super::parse_color_str;

    #[test]
    fn color_arguments_may_be_math() {
        // GitHub's dark-mode label text, once its var()s are in.
        let label = "hsl(55,calc(99*1%),calc((49 + calc((.6 - calc(247*.2126/255 + 225*.7152/255 + 1*.0722/255))*100*max(0,min(calc(1/(.6 - calc(247*.2126/255 + 225*.7152/255 + 1*.0722/255))),1))))*1%))";
        assert_eq!(parse_color_str(label), parse_color_str("hsl(55, 99%, 49%)"));
        // Its light-mode text: white on a dark label.
        let switch = "hsl(0deg,0%,calc(max(0,min(calc(1/(.453 - .2)),1))*100%))";
        assert_eq!(parse_color_str(switch), parse_color_str("#fff"));
        assert_eq!(parse_color_str("rgb(calc(255 / 2), 0, clamp(0, 300, 255))"), parse_color_str("rgb(127.5, 0, 255)"));
        assert!(parse_color_str("hsl(calc(1px + 2), 50%, 50%)").is_none());
    }
}
