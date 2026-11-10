//! Transitions and `@keyframes` animations.

use super::*;

pub(super) fn p_transition_property<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::TransitionProperty(p.parse_comma_separated(|p| {
    Ok(Arc::from(&*p.expect_ident_cloned()?.to_ascii_lowercase()))
})?));
    Ok(())
}

pub(super) fn p_transition_duration<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::TransitionDuration(p.parse_comma_separated(parse_time)?));
    Ok(())
}

pub(super) fn p_transition_delay<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::TransitionDelay(p.parse_comma_separated(parse_time)?));
    Ok(())
}

pub(super) fn p_transition_timing_function<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::TransitionTiming(p.parse_comma_separated(parse_easing)?));
    Ok(())
}

pub(super) fn p_transition<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let items = p.parse_comma_separated(|p| {
        let (mut property, mut times, mut easing) = (Arc::from("all"), Vec::new(), Easing::EASE);
        while !p.is_exhausted() {
            if let Ok(t) = p.try_parse(parse_time) {
                times.push(t);
            } else if let Ok(e) = p.try_parse(parse_easing) {
                easing = e;
            } else if let Ok(id) = p.try_parse(|p| p.expect_ident_cloned()) {
                property = Arc::from(&*id.to_ascii_lowercase());
            } else {
                p.next()?;
            }
        }
        Ok((property, times.first().copied().unwrap_or(0.0), times.get(1).copied().unwrap_or(0.0), easing))
    })?;
    out.push(D::Transition(items));
}
    Ok(())
}

pub(super) fn p_animation_name<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::AnimationName(p.parse_comma_separated(|p| {
    Ok(match p.next()?.clone() {
        Token::Ident(s) | Token::QuotedString(s) => Arc::from(&*s),
        _ => Arc::from("none"),
    })
})?));
    Ok(())
}

pub(super) fn p_animation_duration<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::AnimationDuration(p.parse_comma_separated(parse_time)?));
    Ok(())
}

pub(super) fn p_animation_delay<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::AnimationDelay(p.parse_comma_separated(parse_time)?));
    Ok(())
}

pub(super) fn p_animation_timing_function<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::AnimationTiming(p.parse_comma_separated(parse_easing)?));
    Ok(())
}

pub(super) fn p_animation_iteration_count<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::AnimationIterations(p.parse_comma_separated(parse_iterations)?));
    Ok(())
}

pub(super) fn p_animation_direction<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::AnimationDirection(p.parse_comma_separated(parse_direction)?));
    Ok(())
}

pub(super) fn p_animation_fill_mode<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
out.push(D::AnimationFill(p.parse_comma_separated(parse_fill)?));
    Ok(())
}

pub(super) fn p_animation<'i>(_name: &str, p: &mut Parser<'i>, out: &mut Vec<Decl>) -> PResult<'i, ()> {
    use Decl as D;
{
    let items = p.parse_comma_separated(|p| {
        let mut a = Animation {
            name: Arc::from("none"),
            duration: 0.0,
            delay: 0.0,
            easing: Easing::EASE,
            iterations: 1.0,
            direction: AnimDirection::Normal,
            fill_forwards: false,
            fill_backwards: false,
        };
        let mut times = 0;
        while !p.is_exhausted() {
            if let Ok(t) = p.try_parse(parse_time) {
                if times == 0 { a.duration = t } else { a.delay = t }
                times += 1;
            } else if let Ok(e) = p.try_parse(parse_easing) {
                a.easing = e;
            } else if let Ok(n) = p.try_parse(parse_iterations) {
                a.iterations = n;
            } else if let Ok(d) = p.try_parse(parse_direction) {
                a.direction = d;
            } else if let Ok((f, b)) = p.try_parse(parse_fill) {
                (a.fill_forwards, a.fill_backwards) = (f, b);
            } else if let Ok(name) = p.try_parse(|p| match p.next()?.clone() {
                Token::Ident(s) if !matches!(&*s.to_ascii_lowercase(), "running" | "paused") => Ok(s),
                Token::QuotedString(s) => Ok(s),
                _ => err(p),
            }) {
                a.name = Arc::from(&*name);
            } else {
                p.next()?;
            }
        }
        Ok(a)
    })?;
    let items: Vec<Animation> = items.into_iter().filter(|a| &*a.name != "none").collect();
    out.push(D::Animation((!items.is_empty()).then(|| items.into())));
}
    Ok(())
}

/// The properties this module parses.
pub(super) const PROPERTIES: &[(&str, ParseFn)] = &[
    ("transition-property", p_transition_property),
    ("transition-duration", p_transition_duration),
    ("transition-delay", p_transition_delay),
    ("transition-timing-function", p_transition_timing_function),
    ("transition", p_transition),
    ("animation-name", p_animation_name),
    ("animation-duration", p_animation_duration),
    ("animation-delay", p_animation_delay),
    ("animation-timing-function", p_animation_timing_function),
    ("animation-iteration-count", p_animation_iteration_count),
    ("animation-direction", p_animation_direction),
    ("animation-fill-mode", p_animation_fill_mode),
    ("animation", p_animation),
];
