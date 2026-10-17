//! CSSOM plumbing: `document.styleSheets`, `element.sheet`, `insertRule` / `deleteRule`,
//! constructed sheets and `document.adoptedStyleSheets`.
//!
//! The script-facing classes live in the prelude. A sheet that script has edited keeps its
//! rules as a list of rule texts on the page host, which then replaces the element's own CSS
//! in the cascade.

use super::ph;
use crate::dom::NodeId;
use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::Value;

/// Split CSS source into its top-level rules (`a { … }`, `@media … { … }`, `@import …;`).
pub fn split_rules(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            // a comment before a rule is not part of it
            let end = src[i + 2..].find("*/").map(|e| i + 2 + e + 2).unwrap_or(b.len());
            if depth == 0 && src[start..i].trim().is_empty() {
                start = end;
            }
            i = end;
            continue;
        }
        match c {
            b'"' | b'\'' => {
                i += 1;
                while i < b.len() && b[i] != c {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth <= 0 {
                    depth = 0;
                    let r = src[start..=i].trim();
                    if !r.is_empty() {
                        out.push(r.to_string());
                    }
                    start = i + 1;
                }
            }
            b';' if depth == 0 => {
                let r = src[start..=i].trim();
                if !r.is_empty() && r != ";" {
                    out.push(r.to_string());
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn str_list(it: &mut Interp, v: &[String]) -> Value {
    let items = v.iter().map(|s| Value::str(s)).collect();
    it.new_array(items)
}

fn read_list(it: &mut Interp, v: &Value) -> R<Vec<String>> {
    let items = it.iterate_to_vec(v)?;
    Ok(items.iter().map(|x| it.to_string_lossy(x)).collect())
}

/// The source rules of a `<style>` / `<link rel=stylesheet>` (or its edited list).
fn sheet_rules(it: &mut Interp, n: NodeId) -> R<Option<Vec<String>>> {
    let h = ph(it);
    if let Some(r) = h.cssom.get(&n) {
        return Ok(Some(r.clone()));
    }
    match h.doc.tag(n) {
        Some("style") => Ok(Some(split_rules(&h.doc.text_content(n)))),
        Some("link") => {
            let Some(u) = h.doc.attr(n, "href").and_then(|href| h.resolve_url(href)) else { return Ok(None) };
            if u.origin() != h.url.origin() && u.scheme != "data" {
                throw!(it, "SecurityError", "Failed to read the 'cssRules' property from 'CSSStyleSheet': Cannot access rules");
            }
            let text = h.sheet_texts.get(&u.to_string()).cloned();
            Ok(text.map(|t| split_rules(&t)))
        }
        _ => Ok(None),
    }
}

pub fn install(it: &mut Interp) {
    let g = it.global.clone();
    it.define_method(&g, "__tip_css_split", 1, |it, _, args| {
        let s = it.to_string_lossy(&arg(args, 0));
        Ok(str_list(it, &split_rules(&s)))
    });
    it.define_method(&g, "__tip_sheet_rules", 1, |it, _, args| {
        let Some(n) = super::node_of(&arg(args, 0)) else { return Ok(Value::Null) };
        Ok(match sheet_rules(it, n)? {
            Some(r) => str_list(it, &r),
            None => Value::Null,
        })
    });
    it.define_method(&g, "__tip_sheet_set", 2, |it, _, args| {
        let Some(n) = super::node_of(&arg(args, 0)) else { return Ok(Value::Undefined) };
        let rules = read_list(it, &arg(args, 1))?;
        let h = ph(it);
        h.cssom.insert(n, rules);
        h.doc.version += 1;
        Ok(Value::Undefined)
    });
    it.define_method(&g, "__tip_csheet_set", 2, |it, _, args| {
        let id = it.to_number(&arg(args, 0))? as u64;
        let rules = read_list(it, &arg(args, 1))?;
        let h = ph(it);
        h.constructed_sheets.insert(id, rules);
        if h.adopted_sheets.contains(&id) {
            h.doc.version += 1;
        }
        Ok(Value::Undefined)
    });
    it.define_method(&g, "__tip_adopt", 1, |it, _, args| {
        let ids: Vec<u64> = it.iterate_to_vec(&arg(args, 0))?.iter().filter_map(|v| if let Value::Num(n) = v { Some(*n as u64) } else { None }).collect();
        let h = ph(it);
        h.adopted_sheets = ids;
        h.doc.version += 1;
        Ok(Value::Undefined)
    });
}

#[cfg(test)]
mod tests {
    use super::split_rules;

    #[test]
    fn splits_top_level_rules() {
        let css = "/* c */ @import url(a.css); a { color: red } @media (min-width: 1px) { b { x: \"}\" } } .c{}";
        assert_eq!(split_rules(css), vec!["@import url(a.css);", "a { color: red }", "@media (min-width: 1px) { b { x: \"}\" } }", ".c{}"]);
    }
}
