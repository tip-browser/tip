//! Box tree construction from DOM + computed styles.

use std::rc::Rc;

use super::{Inner, LayoutBox, Replaced};
use crate::dom::{Document, NodeData, NodeId};
use crate::style::{ComputedStyle, Display, ListStyle, Styles};

pub fn build_tree(doc: &Document, styles: &Styles) -> Option<LayoutBox> {
    let html = doc.document_element()?;
    let mut boxes = build_element(doc, styles, html);
    if boxes.is_empty() {
        return None;
    }
    let mut root = boxes.remove(0);
    root.inline_level = false;
    if root.inner == Inner::InlineFlow {
        root.inner = Inner::Flow;
    }
    Some(root)
}

fn replaced_kind(doc: &Document, node: NodeId) -> Option<Replaced> {
    let tag = doc.tag(node)?;
    Some(match tag {
        "img" => Replaced::Image,
        "input" => match doc.attr(node, "type").unwrap_or("text").to_ascii_lowercase().as_str() {
            "checkbox" => Replaced::Checkbox,
            "radio" => Replaced::Radio,
            "submit" => Replaced::Button(doc.attr(node, "value").unwrap_or("Submit").to_string()),
            "reset" => Replaced::Button(doc.attr(node, "value").unwrap_or("Reset").to_string()),
            "button" => Replaced::Button(doc.attr(node, "value").unwrap_or("").to_string()),
            "image" => Replaced::Image,
            "range" => Replaced::Range,
            "color" | "file" => Replaced::Placeholder,
            _ => Replaced::TextInput,
        },
        "textarea" => Replaced::TextArea,
        "select" => Replaced::Select,
        "iframe" | "canvas" | "video" | "embed" | "object" | "svg" | "progress" | "meter" | "audio" => Replaced::Placeholder,
        _ => return None,
    })
}

fn inner_for(display: Display) -> Option<Inner> {
    Some(match display {
        Display::None | Display::TableColumn => return None,
        Display::Block | Display::ListItem | Display::InlineBlock => Inner::Flow,
        Display::Inline => Inner::InlineFlow,
        Display::Flex | Display::InlineFlex => Inner::Flex,
        Display::Grid | Display::InlineGrid => Inner::Grid,
        Display::Table | Display::InlineTable => Inner::Table,
        Display::TableRowGroup => Inner::TableRowGroup,
        Display::TableRow => Inner::TableRow,
        Display::TableCell => Inner::TableCell,
        Display::TableCaption => Inner::TableCaption,
        Display::Contents => Inner::Flow,
    })
}

fn to_roman(mut n: usize, upper: bool) -> String {
    let table = [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
    let mut s = String::new();
    for (v, r) in table {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    if upper { s.to_uppercase() } else { s }
}

fn marker_text(style: ListStyle, index: i64) -> Option<String> {
    Some(match style {
        ListStyle::None => return None,
        ListStyle::Disc => "•".into(),
        ListStyle::Circle => "◦".into(),
        ListStyle::Square => "▪".into(),
        ListStyle::Decimal => format!("{index}."),
        ListStyle::LowerAlpha | ListStyle::UpperAlpha => {
            let mut n = index.max(1) as u32;
            let mut s = String::new();
            while n > 0 {
                n -= 1;
                s.insert(0, (b'a' + (n % 26) as u8) as char);
                n /= 26;
            }
            if style == ListStyle::UpperAlpha {
                s = s.to_uppercase();
            }
            format!("{s}.")
        }
        ListStyle::LowerRoman => format!("{}.", to_roman(index.max(1) as usize, false)),
        ListStyle::UpperRoman => format!("{}.", to_roman(index.max(1) as usize, true)),
    })
}

fn list_index(doc: &Document, styles: &Styles, node: NodeId) -> i64 {
    if let Some(v) = doc.attr(node, "value").and_then(|v| v.trim().parse::<i64>().ok()) {
        return v;
    }
    let parent = doc.parent(node);
    let reversed = parent.is_some_and(|p| doc.has_attr(p, "reversed"));
    let start = parent.and_then(|p| doc.attr(p, "start")).and_then(|s| s.trim().parse::<i64>().ok());
    let mut count = 0i64;
    if let Some(p) = parent {
        for &c in doc.children(p) {
            if styles.get(c).is_some_and(|s| s.display == Display::ListItem) {
                count += 1;
            }
            if c == node {
                break;
            }
        }
    }
    if reversed {
        let total = parent
            .map(|p| doc.children(p).iter().filter(|&&c| styles.get(c).is_some_and(|s| s.display == Display::ListItem)).count() as i64)
            .unwrap_or(1);
        start.unwrap_or(total) - count + 1
    } else {
        start.unwrap_or(1) + count - 1
    }
}

fn anon_style(parent: &ComputedStyle) -> Rc<ComputedStyle> {
    let mut s = ComputedStyle::inherit(parent);
    s.display = Display::Block;
    Rc::new(s)
}

fn pseudo_box(node: NodeId, ps: &Rc<ComputedStyle>) -> Option<LayoutBox> {
    let text = ps.content.clone()?;
    let inner = inner_for(ps.display)?;
    let inline_level = ps.display.is_inline_level();
    let mut b = LayoutBox::new(Some(node), ps.clone(), inner, inline_level);
    b.is_pseudo = true;
    if !text.is_empty() {
        let mut t = LayoutBox::new(Some(node), ps.clone(), Inner::Text(text), true);
        t.is_pseudo = true;
        b.children.push(t);
    }
    Some(b)
}

fn build_element(doc: &Document, styles: &Styles, node: NodeId) -> Vec<LayoutBox> {
    let Some(style) = styles.get(node).cloned() else { return vec![] };
    if style.display == Display::None {
        return vec![];
    }
    let tag = doc.tag(node).unwrap_or("");
    if tag == "br" {
        return vec![LayoutBox::new(Some(node), style, Inner::Break, true)];
    }
    let mut children: Vec<LayoutBox> = Vec::new();
    if let Some(ps) = styles.before.get(&node) {
        children.extend(pseudo_box(node, ps));
    }
    let replaced = replaced_kind(doc, node);
    if replaced.is_none() {
        for &c in doc.children(node) {
            match &doc.node(c).data {
                NodeData::Element(_) => children.extend(build_element(doc, styles, c)),
                NodeData::Text(t) => {
                    if !t.is_empty() {
                        children.push(LayoutBox::new(Some(node), style.clone(), Inner::Text(t.clone()), true));
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(ps) = styles.after.get(&node) {
        children.extend(pseudo_box(node, ps));
    }
    if style.display == Display::Contents && replaced.is_none() {
        return children;
    }
    let Some(mut inner) = inner_for(style.display) else { return vec![] };
    if let Some(r) = replaced {
        inner = Inner::Replaced(r);
    }
    let mut b = LayoutBox::new(Some(node), style.clone(), inner, style.display.is_inline_level());
    if matches!(tag, "td" | "th") {
        b.colspan = doc.attr(node, "colspan").and_then(|v| v.trim().parse().ok()).unwrap_or(1usize).clamp(1, 100);
    }
    if style.display == Display::ListItem {
        let idx = list_index(doc, styles, node);
        if let Some(m) = marker_text(style.list_style, idx) {
            if style.list_inside {
                children.insert(0, LayoutBox::new(Some(node), style.clone(), Inner::Text(format!("{m} ")), true));
            } else {
                b.marker = Some(m);
            }
        }
    }
    b.children = children;
    fixup(&mut b);
    vec![b]
}

fn is_ws_text(b: &LayoutBox) -> bool {
    match &b.inner {
        Inner::Text(t) => b.style.white_space.collapses() && t.chars().all(|c| c.is_ascii_whitespace()),
        _ => false,
    }
}

fn neutral(b: &LayoutBox) -> bool {
    b.is_out_of_flow() || b.is_float()
}

fn blockify(c: &mut LayoutBox) {
    c.inline_level = false;
    if c.inner == Inner::InlineFlow {
        c.inner = Inner::Flow;
    }
}

fn fixup(b: &mut LayoutBox) {
    match b.inner {
        Inner::InlineFlow => {
            if b.children.iter().any(|c| !c.inline_level && !neutral(c)) {
                b.inner = Inner::Flow;
                b.inline_level = false;
                fixup(b);
            }
        }
        Inner::Flow | Inner::TableCell | Inner::TableCaption => {
            let has_inline = b.children.iter().any(|c| c.inline_level && !neutral(c) && !is_ws_text(c));
            let has_block = b.children.iter().any(|c| !c.inline_level && !neutral(c));
            if has_inline && has_block {
                let kids = std::mem::take(&mut b.children);
                let mut run: Vec<LayoutBox> = Vec::new();
                let flush = |run: &mut Vec<LayoutBox>, out: &mut Vec<LayoutBox>, parent: &ComputedStyle| {
                    if run.is_empty() {
                        return;
                    }
                    if run.iter().all(|c| is_ws_text(c)) {
                        run.clear();
                        return;
                    }
                    // Neutral (floated / absolute) boxes at the edges of a run stay outside it.
                    let mut anon = LayoutBox::new(None, anon_style(parent), Inner::Flow, false);
                    anon.children = std::mem::take(run);
                    out.push(anon);
                };
                let mut out = Vec::new();
                for c in kids {
                    if c.inline_level || (neutral(&c) && !run.is_empty()) {
                        run.push(c);
                    } else {
                        flush(&mut run, &mut out, &b.style);
                        out.push(c);
                    }
                }
                flush(&mut run, &mut out, &b.style);
                b.children = out;
            } else if !has_inline {
                b.children.retain(|c| !is_ws_text(c));
            }
        }
        Inner::Flex | Inner::Grid => {
            let kids = std::mem::take(&mut b.children);
            let mut out = Vec::new();
            let mut run: Vec<LayoutBox> = Vec::new();
            for mut c in kids {
                if c.is_text() || c.inner == Inner::Break {
                    run.push(c);
                    continue;
                }
                if !run.is_empty() {
                    if !run.iter().all(|r| is_ws_text(r)) {
                        let mut anon = LayoutBox::new(None, anon_style(&b.style), Inner::Flow, false);
                        anon.children = std::mem::take(&mut run);
                        out.push(anon);
                    }
                    run.clear();
                }
                blockify(&mut c);
                if c.inner == Inner::Flow {
                    fixup(&mut c);
                }
                out.push(c);
            }
            if !run.is_empty() && !run.iter().all(|r| is_ws_text(r)) {
                let mut anon = LayoutBox::new(None, anon_style(&b.style), Inner::Flow, false);
                anon.children = run;
                out.push(anon);
            }
            b.children = out;
        }
        Inner::Table | Inner::TableRowGroup => {
            let row_level = b.inner == Inner::TableRowGroup;
            let kids = std::mem::take(&mut b.children);
            let mut out = Vec::new();
            let mut stray: Vec<LayoutBox> = Vec::new();
            let wrap = |stray: &mut Vec<LayoutBox>, out: &mut Vec<LayoutBox>, style: &ComputedStyle| {
                if stray.is_empty() || stray.iter().all(|s| is_ws_text(s)) {
                    stray.clear();
                    return;
                }
                let mut row = LayoutBox::new(None, anon_style(style), Inner::TableRow, false);
                row.children = std::mem::take(stray);
                fixup(&mut row);
                out.push(row);
            };
            for c in kids {
                let ok = match c.inner {
                    Inner::TableRow => true,
                    Inner::TableRowGroup | Inner::TableCaption => !row_level,
                    _ => false,
                };
                if ok {
                    wrap(&mut stray, &mut out, &b.style);
                    out.push(c);
                } else {
                    stray.push(c);
                }
            }
            wrap(&mut stray, &mut out, &b.style);
            b.children = out;
        }
        Inner::TableRow => {
            let kids = std::mem::take(&mut b.children);
            let mut out = Vec::new();
            let mut stray: Vec<LayoutBox> = Vec::new();
            for c in kids {
                if c.inner == Inner::TableCell {
                    if !stray.is_empty() && !stray.iter().all(|s| is_ws_text(s)) {
                        let mut cell = LayoutBox::new(None, anon_style(&b.style), Inner::TableCell, false);
                        cell.children = std::mem::take(&mut stray);
                        fixup(&mut cell);
                        out.push(cell);
                    }
                    stray.clear();
                    out.push(c);
                } else {
                    stray.push(c);
                }
            }
            if !stray.is_empty() && !stray.iter().all(|s| is_ws_text(s)) {
                let mut cell = LayoutBox::new(None, anon_style(&b.style), Inner::TableCell, false);
                cell.children = stray;
                fixup(&mut cell);
                out.push(cell);
            }
            b.children = out;
        }
        _ => {}
    }
}
