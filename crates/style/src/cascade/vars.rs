//! Custom properties: `var()` substitution.

use super::*;

pub(super) fn value_still_has_var(v: &str) -> bool {
    crate::props::contains_var(v)
}

/// Replace `var(--name, fallback)` references. `None` if a reference can't be
/// resolved (the declaration is then "invalid at computed-value time").
pub(super) fn substitute_vars(raw: &str, custom: &CustomProps, depth: usize) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    let Some(start) = lower.find("var(") else {
        return Some(raw.to_owned());
    };
    // Find the matching close paren.
    let body_start = start + 4;
    let mut paren = 1;
    let mut end = None;
    for (i, c) in raw[body_start..].char_indices() {
        match c {
            '(' => paren += 1,
            ')' => {
                paren -= 1;
                if paren == 0 {
                    end = Some(body_start + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let body = &raw[body_start..end];
    let (name, fallback) = match body.find(',') {
        Some(i) => (body[..i].trim(), Some(&body[i + 1..])),
        None => (body.trim(), None),
    };
    let replacement = match custom.get(name) {
        Some(value) if !value.trim().is_empty() => substitute_vars(value, custom, depth + 1)?,
        _ => substitute_vars(fallback?, custom, depth + 1)?,
    };
    let rest = substitute_vars(&raw[end + 1..], custom, depth + 1)?;
    Some(format!("{}{}{}", &raw[..start], replacement.trim(), rest))
}

impl Styler {
    pub(super) fn resolve_vars(&self, name: &Arc<str>, raw: &str, custom: &CustomProps) -> Option<Rc<Vec<Decl>>> {
        let value = substitute_vars(raw, custom, 0)?;
        let key = (name.clone(), value);
        if let Some(hit) = self.var_cache.borrow().get(&key) {
            return Some(hit.clone());
        }
        let mut out = Vec::new();
        if !value_still_has_var(&key.1) {
            crate::props::with_base(self.base.as_ref(), || parse_declaration(name, &key.1, &mut out));
        }
        let out = Rc::new(out);
        self.var_cache.borrow_mut().insert(key, out.clone());
        Some(out)
    }
}
