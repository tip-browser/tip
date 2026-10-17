//! User input: hit testing, focus, default actions, text editing.

use crate::bindings::events::{self, key_init, mouse_init, EventInit, Mods};
use crate::bindings::{ph, wrap};
use crate::dom::{NodeData, NodeId};
use crate::js::{Interp, Value};
use crate::net::{self, Url};
use crate::page::{HostMsg, NavRequest, Page, Target};
use crate::style::Cursor;

fn element_of(it: &mut Interp, n: NodeId) -> NodeId {
    let h = ph(it);
    if h.doc.is_element(n) { n } else { h.doc.parent(n).unwrap_or(n) }
}

fn ancestors(it: &mut Interp, n: NodeId) -> Vec<NodeId> {
    let h = ph(it);
    let mut out = vec![n];
    let mut cur = h.doc.parent(n);
    while let Some(p) = cur {
        out.push(p);
        cur = h.doc.parent(p);
    }
    out
}

fn input_type(it: &mut Interp, n: NodeId) -> String {
    let h = ph(it);
    match h.doc.tag(n) {
        Some("input") => h.doc.attr(n, "type").unwrap_or("text").to_ascii_lowercase(),
        Some("button") => h.doc.attr(n, "type").unwrap_or("submit").to_ascii_lowercase(),
        Some(t) => t.to_string(),
        None => String::new(),
    }
}

fn owner_form(it: &mut Interp, n: NodeId) -> Option<NodeId> {
    let h = ph(it);
    if let Some(fid) = h.doc.attr(n, "form") {
        if let Some(f) = h.doc.get_element_by_id(fid) {
            return Some(f);
        }
    }
    let mut cur = Some(n);
    while let Some(c) = cur {
        if h.doc.tag_is(c, "form") {
            return Some(c);
        }
        cur = h.doc.parent(c);
    }
    None
}

pub fn focus(it: &mut Interp, n: Option<NodeId>) {
    let old = ph(it).focus;
    if old == n {
        return;
    }
    if let Some(o) = old {
        // change event for edited text controls
        let (changed, is_text) = {
            let h = ph(it);
            let is_text = h.is_text_control(o);
            (is_text && h.focus_value.as_deref() != Some(h.control_value(o).as_str()), is_text)
        };
        let _ = is_text;
        ph(it).focus = None;
        ph(it).invalidate();
        if changed {
            events::fire(it, Target::Node(o), "change", "Event", EventInit { bubbles: true, ..Default::default() });
        }
        events::fire(it, Target::Node(o), "blur", "FocusEvent", EventInit::default());
        events::fire(it, Target::Node(o), "focusout", "FocusEvent", EventInit { bubbles: true, ..Default::default() });
    }
    if let Some(n) = n {
        let h = ph(it);
        h.focus = Some(n);
        h.focus_value = Some(h.control_value(n));
        let len = h.control_value(n).chars().count();
        if h.is_text_control(n) {
            let e = h.form.entry(n).or_default();
            e.caret = len;
            e.anchor = None;
        }
        h.invalidate();
        events::fire(it, Target::Node(n), "focus", "FocusEvent", EventInit::default());
        events::fire(it, Target::Node(n), "focusin", "FocusEvent", EventInit { bubbles: true, ..Default::default() });
    }
}

/// Collect name/value pairs for form submission.
pub fn form_entries(it: &mut Interp, form: NodeId, submitter: Option<NodeId>) -> Vec<(String, String)> {
    let h = ph(it);
    let mut out = Vec::new();
    let controls: Vec<NodeId> = h.doc.element_descendants(form).into_iter().filter(|&e| matches!(h.doc.tag(e), Some("input" | "select" | "textarea" | "button"))).collect();
    for c in controls {
        if h.doc.has_attr(c, "disabled") {
            continue;
        }
        let Some(name) = h.doc.attr(c, "name").map(|s| s.to_string()) else { continue };
        if name.is_empty() {
            continue;
        }
        let tag = h.doc.tag(c).unwrap_or("").to_string();
        let ty = h.doc.attr(c, "type").unwrap_or(if tag == "button" { "submit" } else { "text" }).to_ascii_lowercase();
        match (tag.as_str(), ty.as_str()) {
            ("input", "checkbox") | ("input", "radio") => {
                if h.is_checked(c) {
                    out.push((name, h.doc.attr(c, "value").unwrap_or("on").to_string()));
                }
            }
            ("input", "submit") | ("input", "image") | ("button", "submit") => {
                if Some(c) == submitter {
                    out.push((name, h.doc.attr(c, "value").unwrap_or("").to_string()));
                }
            }
            ("input", "button") | ("input", "reset") | ("button", _) | ("input", "file") => {}
            _ => out.push((name, h.control_value(c))),
        }
    }
    out
}

pub fn validate(it: &mut Interp, n: NodeId) -> bool {
    let h = ph(it);
    let targets: Vec<NodeId> = if h.doc.tag_is(n, "form") { h.doc.element_descendants(n) } else { vec![n] };
    for t in targets {
        if h.doc.has_attr(t, "required") && !h.doc.has_attr(t, "disabled") {
            let ok = match h.doc.attr(t, "type").map(|x| x.to_ascii_lowercase()) {
                Some(ref x) if x == "checkbox" || x == "radio" => h.is_checked(t),
                _ => !h.control_value(t).is_empty(),
            };
            if !ok {
                return false;
            }
        }
    }
    true
}

pub fn submit_form(it: &mut Interp, n: NodeId, submitter: Option<NodeId>, fire_event: bool) {
    let Some(form) = owner_form(it, n) else { return };
    if fire_event {
        if !validate(it, form) {
            ph(it).log("warn", "Form submission blocked: a required field is empty".into());
            return;
        }
        let sv = submitter.map(|s| wrap(it, s)).unwrap_or(Value::Null);
        let ok = events::fire(it, Target::Node(form), "submit", "SubmitEvent", EventInit { bubbles: true, cancelable: true, props: vec![("submitter", sv)] });
        if !ok {
            return;
        }
    }
    let entries = form_entries(it, form, submitter);
    let h = ph(it);
    if h.internal {
        let action = h.doc.attr(form, "data-action").unwrap_or("").to_string();
        let value = entries.first().map(|e| e.1.clone()).unwrap_or_default();
        h.outbox.push(HostMsg::Submit { action, value, node: form });
        return;
    }
    let action = submitter
        .and_then(|s| h.doc.attr(s, "formaction"))
        .or_else(|| h.doc.attr(form, "action"))
        .unwrap_or("")
        .to_string();
    let method = submitter.and_then(|s| h.doc.attr(s, "formmethod")).or_else(|| h.doc.attr(form, "method")).unwrap_or("get").to_ascii_lowercase();
    let Some(mut url) = (if action.is_empty() { Some(h.url.clone()) } else { h.resolve_url(&action) }) else { return };
    let q = entries.iter().map(|(k, v)| format!("{}={}", net::url::form_encode(k), net::url::form_encode(v))).collect::<Vec<_>>().join("&");
    if method == "post" {
        let mut headers = h.request_headers("POST", &url);
        headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
        h.nav = Some(NavRequest::Load { url, method: "POST".into(), body: Some(q.into_bytes()), replace: false, headers });
    } else if method == "dialog" {
        if let Some(d) = (0..1).find_map(|_| {
            let mut cur = h.doc.parent(form);
            while let Some(p) = cur {
                if h.doc.tag_is(p, "dialog") {
                    return Some(p);
                }
                cur = h.doc.parent(p);
            }
            None
        }) {
            h.doc.remove_attr(d, "open");
        }
    } else {
        url.query = Some(q);
        url.fragment = None;
        let headers = h.request_headers("GET", &url);
        h.nav = Some(NavRequest::Load { url, method: "GET".into(), body: None, replace: false, headers });
    }
}

/// Fire a click on `n` and run its activation behaviour.
pub fn click_element(it: &mut Interp, n: NodeId, mods: Mods) {
    let rect = ph(it).node_rect(n);
    let sc = ph(it).scroll;
    let (x, y) = rect.map(|r| (r.x + r.w / 2.0 - sc.0, r.y + r.h / 2.0 - sc.1)).unwrap_or((0.0, 0.0));
    dispatch_click(it, n, x, y, mods, 1);
}

fn dispatch_click(it: &mut Interp, target: NodeId, x: f32, y: f32, mods: Mods, count: i32) {
    if ph(it).doc.has_attr(target, "disabled") {
        return;
    }
    let sc = ph(it).scroll;
    // checkbox/radio toggle before dispatch (reverted if cancelled)
    let ty = input_type(it, target);
    let toggled = if ph(it).doc.tag_is(target, "input") && (ty == "checkbox" || ty == "radio") {
        let was = ph(it).is_checked(target);
        if ty == "checkbox" || !was {
            ph(it).set_checked(target, if ty == "checkbox" { !was } else { true });
            Some(was)
        } else {
            None
        }
    } else {
        None
    };
    let ok = events::fire(it, Target::Node(target), "click", "MouseEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, 0, count, mods) });
    if !ok {
        if let Some(was) = toggled {
            ph(it).set_checked(target, was);
        }
        return;
    }
    if toggled.is_some() {
        events::fire(it, Target::Node(target), "input", "Event", EventInit { bubbles: true, ..Default::default() });
        events::fire(it, Target::Node(target), "change", "Event", EventInit { bubbles: true, ..Default::default() });
        return;
    }
    activate(it, target, mods);
}

fn activate(it: &mut Interp, target: NodeId, mods: Mods) {
    let chain = ancestors(it, target);
    for n in chain {
        let (tag, internal) = {
            let h = ph(it);
            (h.doc.tag(n).unwrap_or("").to_string(), h.internal)
        };
        if internal {
            let h = ph(it);
            if let Some(action) = h.doc.attr(n, "data-action").map(|s| s.to_string()) {
                let arg = h.doc.attr(n, "data-arg").unwrap_or("").to_string();
                h.outbox.push(HostMsg::Action { action, arg, node: n });
                return;
            }
        }
        match tag.as_str() {
            "a" | "area" => {
                let h = ph(it);
                let Some(href) = h.doc.attr(n, "href").map(|s| s.to_string()) else { continue };
                let target_attr = h.doc.attr(n, "target").unwrap_or("").to_string();
                if href.trim_start().to_ascii_lowercase().starts_with("javascript:") {
                    let code = net::url::percent_decode(href.trim_start()[11..].trim());
                    if let Err(e) = it.eval_script(&code, "javascript: URL") {
                        let msg = it.describe_error(&e);
                        ph(it).log("error", format!("Uncaught {msg}"));
                    }
                    return;
                }
                if let Some(url) = h.resolve_url(&href) {
                    if target_attr == "_blank" || mods.meta || mods.ctrl {
                        h.nav = Some(NavRequest::Open(url));
                    } else if url.without_fragment() == h.url.without_fragment() && url.fragment.is_some() {
                        h.nav = Some(NavRequest::SameDocument { url, replace: false });
                    } else {
                        let headers = h.request_headers("GET", &url);
                        h.nav = Some(NavRequest::Load { url, method: "GET".into(), body: None, replace: false, headers });
                    }
                }
                return;
            }
            "button" | "input" => {
                let ty = input_type(it, n);
                match ty.as_str() {
                    "submit" | "image" => submit_form(it, n, Some(n), true),
                    "reset" => {
                        if let Some(f) = owner_form(it, n) {
                            let h = ph(it);
                            for c in h.doc.element_descendants(f) {
                                h.form.remove(&c);
                            }
                            h.invalidate();
                        }
                    }
                    _ => {}
                }
                return;
            }
            "label" => {
                let h = ph(it);
                let control = match h.doc.attr(n, "for") {
                    Some(id) => h.doc.get_element_by_id(id),
                    None => h.doc.element_descendants(n).into_iter().find(|&c| matches!(h.doc.tag(c), Some("input" | "select" | "textarea" | "button"))),
                };
                if let Some(c) = control {
                    if c != target {
                        if ph(it).is_text_control(c) {
                            focus(it, Some(c));
                        } else {
                            click_element(it, c, mods);
                        }
                    }
                }
                return;
            }
            "summary" => {
                let h = ph(it);
                if let Some(d) = h.doc.parent(n).filter(|&p| h.doc.tag_is(p, "details")) {
                    if h.doc.has_attr(d, "open") {
                        h.doc.remove_attr(d, "open");
                    } else {
                        h.doc.set_attr(d, "open", "");
                    }
                    events::fire(it, Target::Node(d), "toggle", "Event", EventInit::default());
                }
                return;
            }
            "select" => {
                if !ph(it).doc.has_attr(n, "disabled") {
                    ph(it).open_select(n);
                }
                return;
            }
            _ => {}
        }
    }
}

fn focusable_ancestor(it: &mut Interp, n: NodeId) -> Option<NodeId> {
    for a in ancestors(it, n) {
        if ph(it).is_focusable(a) {
            return Some(a);
        }
    }
    None
}

fn focus_order(it: &mut Interp) -> Vec<NodeId> {
    let h = ph(it);
    h.doc.element_descendants(0).into_iter().filter(|&e| h.is_focusable(e) && h.doc.attr(e, "tabindex") != Some("-1")).collect()
}

fn scrollable_ancestor(page: &mut Page, n: NodeId, dy: f32, dx: f32) -> Option<NodeId> {
    let l = page.host_mut().layout();
    let chain = ancestors(&mut page.interp, n);
    for a in chain {
        if let Some(b) = l.find_box(a) {
            if b.is_scroll_container() && b.style.overflow_y.scrolls() || b.style.overflow_x.scrolls() && b.is_scroll_container() {
                let (sx, sy) = b.scroll;
                let (mx, my) = b.scroll_max;
                let can_y = (dy > 0.0 && sy < my - 0.5) || (dy < 0.0 && sy > 0.5);
                let can_x = (dx > 0.0 && sx < mx - 0.5) || (dx < 0.0 && sx > 0.5);
                if can_y || can_x {
                    return Some(a);
                }
            }
        }
    }
    None
}

impl Page {
    fn hit_element(&mut self, x: f32, y: f32) -> Option<NodeId> {
        let n = self.host_mut().hit_test(x, y)?;
        Some(element_of(&mut self.interp, n))
    }

    /// Mouse moved to viewport coordinates (x, y). Returns true if a repaint is needed.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let sc = self.host().scroll;
        // open dropdown: track the hovered option
        if let Some(row) = self.host_mut().popup_row_at(x + sc.0, y + sc.1) {
            if let Some(i) = row {
                let h = self.host_mut();
                let enabled = h.select_popup.as_ref().map(|p| h.select_options(p.node)).and_then(|o| o.get(i).map(|x| !x.2)).unwrap_or(false);
                if enabled && h.select_popup.as_ref().is_some_and(|p| p.hover != i) {
                    if let Some(p) = &mut h.select_popup {
                        p.hover = i;
                    }
                    h.invalidate();
                }
            }
            self.host_mut().cursor = Cursor::Default;
            return true;
        }
        // dragging a range thumb
        if let Some(n) = self.host().drag_range {
            if self.host().mouse_down_target.is_some() {
                self.range_from_x(n, x);
                return true;
            }
        }
        // drag-selecting text in a control
        if let Some(n) = self.host().drag_select {
            if self.host().mouse_down_target.is_some() {
                let c = self.caret_at(n, x);
                let h = self.host_mut();
                let e = h.form.entry(n).or_default();
                if e.caret != c {
                    e.caret = c;
                    h.invalidate();
                }
                return true;
            }
        }
        let hit = self.hit_element(x, y);
        let old = self.host().hover;
        let mut changed = false;
        if hit != old {
            if let Some(o) = old {
                if self.host().doc.nodes.len() > o {
                    events::fire(&mut self.interp, Target::Node(o), "mouseout", "MouseEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, -1, 0, Mods::default()) });
                    events::fire(&mut self.interp, Target::Node(o), "mouseleave", "MouseEvent", EventInit { props: mouse_init(x, y, sc, -1, 0, Mods::default()), ..Default::default() });
                }
            }
            self.host_mut().hover = hit;
            self.host_mut().invalidate();
            if let Some(n) = hit {
                events::fire(&mut self.interp, Target::Node(n), "mouseover", "MouseEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, -1, 0, Mods::default()) });
                events::fire(&mut self.interp, Target::Node(n), "mouseenter", "MouseEvent", EventInit { props: mouse_init(x, y, sc, -1, 0, Mods::default()), ..Default::default() });
            }
            changed = true;
        }
        if let Some(n) = hit {
            let has_move_listeners = self.host().listeners.values().any(|ls| ls.iter().any(|l| l.ty == "mousemove" || l.ty == "pointermove"));
            if has_move_listeners {
                events::fire(&mut self.interp, Target::Node(n), "mousemove", "MouseEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, -1, 0, Mods::default()) });
                events::fire(&mut self.interp, Target::Node(n), "pointermove", "PointerEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, -1, 0, Mods::default()) });
                changed = true;
            }
        }
        // cursor
        let cursor = match hit {
            Some(n) => {
                let h = self.host_mut();
                let styles = h.styles();
                let c = styles.get(n).map(|s| s.cursor).unwrap_or(Cursor::Default);
                if c == Cursor::Default && h.is_text_control(n) {
                    Cursor::Text
                } else if c == Cursor::Default && h.doc.has_attr(n, "data-action") {
                    Cursor::Pointer
                } else {
                    c
                }
            }
            None => Cursor::Default,
        };
        self.host_mut().cursor = cursor;
        if changed {
            self.after_task();
        }
        changed
    }

    pub fn mouse_down(&mut self, x: f32, y: f32, button: i32, mods: Mods) {
        self.mouse_down_n(x, y, button, mods, 1)
    }

    /// Mouse button pressed; `count` is the click count (2 = double click).
    pub fn mouse_down_n(&mut self, x: f32, y: f32, button: i32, mods: Mods, count: i32) {
        let sc = self.host().scroll;
        if let Some(row) = self.host_mut().popup_row_at(x + sc.0, y + sc.1) {
            if row.is_none() {
                // click outside closes the dropdown and is consumed
                self.host_mut().close_select();
                self.host_mut().mouse_down_target = None;
            }
            return;
        }
        let hit = self.hit_element(x, y);
        self.host_mut().mouse_down_target = hit;
        self.host_mut().drag_select = None;
        let Some(n) = hit else {
            focus(&mut self.interp, None);
            self.after_task();
            return;
        };
        self.host_mut().active = Some(n);
        self.host_mut().invalidate();
        events::fire(&mut self.interp, Target::Node(n), "pointerdown", "PointerEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, button, count, mods) });
        let ok = events::fire(&mut self.interp, Target::Node(n), "mousedown", "MouseEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, button, count, mods) });
        if ok && button == 0 {
            let f = focusable_ancestor(&mut self.interp, n);
            let was_focused = f.is_some() && self.host().focus == f;
            focus(&mut self.interp, f);
            if let Some(r) = f.filter(|&r| self.is_range(r)) {
                self.host_mut().drag_range = Some(r);
                self.host_mut().focus_value = Some(self.host().control_value(r));
                self.range_from_x(r, x);
            }
            if let Some(f) = f {
                if self.host().is_text_control(f) {
                    let c = self.caret_at(f, x);
                    let v: Vec<char> = self.host().control_value(f).chars().collect();
                    let h = self.host_mut();
                    let e = h.form.entry(f).or_default();
                    match count {
                        2 => {
                            // select the word under the pointer
                            let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
                            let mut a = c.min(v.len());
                            let mut b = a;
                            if a < v.len() && is_word(v[a]) || a > 0 && is_word(v[a - 1]) {
                                while a > 0 && is_word(v[a - 1]) {
                                    a -= 1;
                                }
                                while b < v.len() && is_word(v[b]) {
                                    b += 1;
                                }
                            } else if b < v.len() {
                                b += 1;
                            }
                            e.anchor = Some(a);
                            e.caret = b;
                        }
                        n if n >= 3 => {
                            e.anchor = Some(0);
                            e.caret = v.len();
                        }
                        _ => {
                            if mods.shift && was_focused {
                                e.anchor = Some(e.anchor.unwrap_or(e.caret));
                            } else {
                                e.anchor = Some(c);
                            }
                            e.caret = c;
                            h.drag_select = Some(f);
                        }
                    }
                    h.invalidate();
                }
            }
        }
        self.after_task();
    }

    /// Caret index in text control `n` nearest to viewport x.
    fn caret_at(&mut self, n: NodeId, x: f32) -> usize {
        let h = self.host_mut();
        let l = h.layout();
        let v = h.control_value(n);
        let Some(b) = l.find_box(n) else { return v.chars().count() };
        let s = b.style.clone();
        let key = crate::paint::key_of(&s);
        // account for the horizontal scroll applied when the caret is past the right edge
        let caret_now = h.form.get(&n).map(|f| f.caret).unwrap_or(0);
        let upto: String = v.chars().take(caret_now).collect();
        let cw = crate::text::measure(key, s.font_size, &upto, s.letter_spacing);
        let shift = if h.focus == Some(n) && cw > b.content.w - 2.0 { cw - b.content.w + 2.0 } else { 0.0 };
        let local = x + h.scroll.0 - b.content.x + shift;
        let pos = crate::text::with_fonts(|fs| fs.positions(key, s.font_size, &v, s.letter_spacing));
        for (i, (c, off)) in pos.iter().enumerate() {
            let w = crate::text::with_fonts(|fs| fs.advance(key, s.font_size, *c));
            if local < off + w / 2.0 {
                return i;
            }
        }
        v.chars().count()
    }

    fn is_range(&self, n: NodeId) -> bool {
        let h = self.host();
        h.doc.tag_is(n, "input") && h.doc.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("range"))
    }

    /// Set a range input's value from a viewport x coordinate; fires `input` if it changed.
    fn range_from_x(&mut self, n: NodeId, x: f32) {
        let h = self.host_mut();
        let l = h.layout();
        let Some(b) = l.find_box(n) else { return };
        let thumb = b.content.h.clamp(10.0, 18.0);
        let (x0, x1) = (b.content.x + thumb / 2.0, b.content.right() - thumb / 2.0);
        let f = if x1 > x0 { ((x + h.scroll.0 - x0) / (x1 - x0)).clamp(0.0, 1.0) } else { 0.0 };
        let (min, max, _) = h.range_params(n);
        let v = h.sanitize_range(n, min + (max - min) * f as f64);
        self.set_range(n, v);
    }

    fn set_range(&mut self, n: NodeId, v: String) {
        let h = self.host_mut();
        if h.control_value(n) != v {
            h.set_control_value(n, v);
            events::fire(&mut self.interp, Target::Node(n), "input", "Event", EventInit { bubbles: true, ..Default::default() });
            self.after_task();
        }
    }

    fn range_key(&mut self, n: NodeId, key: &str) -> bool {
        let h = self.host();
        let (min, max, step) = h.range_params(n);
        let cur: f64 = h.control_value(n).parse().unwrap_or(min);
        let big = ((max - min) / 10.0).max(step);
        let target = match key {
            "ArrowRight" | "ArrowUp" => cur + step,
            "ArrowLeft" | "ArrowDown" => cur - step,
            "PageUp" => cur + big,
            "PageDown" => cur - big,
            "Home" => min,
            "End" => max,
            _ => return false,
        };
        let v = self.host().sanitize_range(n, target);
        let before = self.host().control_value(n);
        self.set_range(n, v.clone());
        if before != v {
            events::fire(&mut self.interp, Target::Node(n), "change", "Event", EventInit { bubbles: true, ..Default::default() });
        }
        true
    }

    /// Select all text in a control (e.g. the address bar on focus).
    pub fn select_all(&mut self, n: NodeId) {
        let len = self.host().control_value(n).chars().count();
        let h = self.host_mut();
        let e = h.form.entry(n).or_default();
        e.anchor = Some(0);
        e.caret = len;
        h.drag_select = None;
        h.invalidate();
    }

    fn choose_option(&mut self, sel: NodeId, i: usize) {
        let h = self.host_mut();
        let opts = h.select_options(sel);
        let Some((o, _, disabled)) = opts.get(i).cloned() else { return };
        h.close_select();
        if disabled {
            return;
        }
        let v = h.option_value(o);
        let changed = h.control_value(sel) != v;
        h.set_control_value(sel, v);
        if changed {
            events::fire(&mut self.interp, Target::Node(sel), "input", "Event", EventInit { bubbles: true, ..Default::default() });
            events::fire(&mut self.interp, Target::Node(sel), "change", "Event", EventInit { bubbles: true, ..Default::default() });
        }
        self.after_task();
    }

    pub fn mouse_up(&mut self, x: f32, y: f32, button: i32, mods: Mods, click_count: i32) {
        let sc = self.host().scroll;
        if let Some(row) = self.host_mut().popup_row_at(x + sc.0, y + sc.1) {
            if let Some(i) = row {
                let sel = self.host().select_popup.as_ref().map(|p| p.node);
                if let Some(sel) = sel {
                    self.choose_option(sel, i);
                }
            }
            self.host_mut().mouse_down_target = None;
            return;
        }
        self.host_mut().drag_select = None;
        if let Some(r) = self.host_mut().drag_range.take() {
            let now = self.host().control_value(r);
            if self.host().focus_value.as_deref() != Some(now.as_str()) {
                self.host_mut().focus_value = Some(now);
                events::fire(&mut self.interp, Target::Node(r), "change", "Event", EventInit { bubbles: true, ..Default::default() });
            }
        }
        let hit = self.hit_element(x, y);
        let down = self.host_mut().mouse_down_target.take();
        self.host_mut().active = None;
        self.host_mut().invalidate();
        if let Some(n) = hit {
            events::fire(&mut self.interp, Target::Node(n), "pointerup", "PointerEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, button, click_count, mods) });
            events::fire(&mut self.interp, Target::Node(n), "mouseup", "MouseEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, button, click_count, mods) });
            if button == 0 {
                // click goes to the nearest common ancestor of down/up targets
                let target = match down {
                    Some(d) if d == n => Some(n),
                    Some(d) => {
                        let da = ancestors(&mut self.interp, d);
                        ancestors(&mut self.interp, n).into_iter().find(|a| da.contains(a))
                    }
                    None => None,
                };
                if let Some(t) = target {
                    dispatch_click(&mut self.interp, t, x, y, mods, click_count);
                    if click_count == 2 {
                        events::fire(&mut self.interp, Target::Node(t), "dblclick", "MouseEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, 0, 2, mods) });
                    }
                }
            } else if button == 2 {
                events::fire(&mut self.interp, Target::Node(n), "contextmenu", "MouseEvent", EventInit { bubbles: true, cancelable: true, props: mouse_init(x, y, sc, 2, 1, mods) });
            }
        }
        self.after_task();
    }

    /// Convenience: full click at viewport coordinates.
    pub fn click_at(&mut self, x: f32, y: f32) {
        self.mouse_move(x, y);
        self.mouse_down(x, y, 0, Mods::default());
        self.mouse_up(x, y, 0, Mods::default(), 1);
    }

    /// Click the centre of an element (used by automation).
    pub fn click_node(&mut self, n: NodeId) {
        let r = self.host_mut().node_rect(n);
        if let Some(r) = r {
            // bring into view
            let (vw, vh) = self.host().viewport;
            let sc = self.host().scroll;
            if r.y < sc.1 || r.bottom() > sc.1 + vh {
                self.host_mut().scroll.1 = (r.y - vh / 3.0).max(0.0);
                self.host_mut().clamp_scroll();
            }
            let sc = self.host().scroll;
            let x = (r.x + r.w / 2.0 - sc.0).clamp(0.0, vw - 1.0);
            let y = (r.y + r.h / 2.0 - sc.1).clamp(0.0, vh - 1.0);
            let hit = self.hit_element(x, y);
            let reaches = hit.is_some_and(|h| self.host().doc.is_ancestor(n, h));
            if reaches {
                self.click_at(x, y);
                return;
            }
        }
        // element not hit-testable (covered/zero-size): dispatch directly
        let f = if self.host().is_focusable(n) { Some(n) } else { None };
        focus(&mut self.interp, f);
        click_element(&mut self.interp, n, Mods::default());
        self.after_task();
    }

    /// Scroll wheel; returns true if something scrolled.
    pub fn wheel(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> bool {
        let sc0 = self.host().scroll;
        if let Some(Some(_)) = self.host_mut().popup_row_at(x + sc0.0, y + sc0.1) {
            let h = self.host_mut();
            let n = h.select_popup.as_ref().map(|p| h.select_options(p.node).len()).unwrap_or(0);
            if let Some(p) = &mut h.select_popup {
                let rows = p.geom.map(|g| g.2).unwrap_or(12);
                let max_first = n.saturating_sub(rows);
                let delta = (dy / 20.0).round() as i64;
                p.first = (p.first as i64 + delta).clamp(0, max_first as i64) as usize;
            }
            h.invalidate();
            return true;
        }
        // any other scroll closes an open dropdown
        self.host_mut().close_select();
        let hit = self.hit_element(x, y);
        let sc = self.host().scroll;
        if let Some(n) = hit {
            let mut props = mouse_init(x, y, sc, -1, 0, Mods::default());
            props.push(("deltaX", Value::Num(dx as f64)));
            props.push(("deltaY", Value::Num(dy as f64)));
            props.push(("deltaMode", Value::Num(0.0)));
            let has_wheel = self.host().listeners.values().any(|ls| ls.iter().any(|l| l.ty == "wheel"));
            if has_wheel {
                let ok = events::fire(&mut self.interp, Target::Node(n), "wheel", "WheelEvent", EventInit { bubbles: true, cancelable: true, props });
                self.after_task();
                if !ok {
                    return true;
                }
            }
            if let Some(s) = scrollable_ancestor(self, n, dy, dx) {
                let h = self.host_mut();
                let l = h.layout();
                let max = l.find_box(s).map(|b| b.scroll_max).unwrap_or((0.0, 0.0));
                let e = h.elem_scroll.entry(s).or_insert((0.0, 0.0));
                e.0 = (e.0 + dx).clamp(0.0, max.0);
                e.1 = (e.1 + dy).clamp(0.0, max.1);
                h.invalidate();
                events::fire(&mut self.interp, Target::Node(s), "scroll", "Event", EventInit::default());
                self.after_task();
                return true;
            }
        }
        let before = self.host().scroll;
        let h = self.host_mut();
        h.scroll.0 += dx;
        h.scroll.1 += dy;
        h.clamp_scroll();
        let moved = self.host().scroll != before;
        if moved {
            let has_scroll = self.host().listeners.contains_key(&Target::Window) || self.host().listeners.contains_key(&Target::Node(0));
            if has_scroll {
                events::fire(&mut self.interp, Target::Node(0), "scroll", "Event", EventInit { bubbles: true, ..Default::default() });
                self.after_task();
            }
        }
        moved
    }

    /// Key pressed. `text` is the character(s) produced, if any.
    pub fn key_down(&mut self, key: &str, text: Option<&str>, mods: Mods) {
        if let Some(p) = self.host().select_popup.clone() {
            let opts = self.host().select_options(p.node);
            let n = opts.len();
            let step = |from: usize, dir: i32| -> usize {
                let mut i = from as i32;
                loop {
                    i += dir;
                    if i < 0 || i >= n as i32 {
                        return from;
                    }
                    if !opts[i as usize].2 {
                        return i as usize;
                    }
                }
            };
            let new_hover = match key {
                "ArrowDown" => Some(step(p.hover, 1)),
                "ArrowUp" => Some(step(p.hover, -1)),
                "Home" | "PageUp" => Some((0..n).find(|&i| !opts[i].2).unwrap_or(0)),
                "End" | "PageDown" => Some((0..n).rev().find(|&i| !opts[i].2).unwrap_or(0)),
                "Enter" | " " => {
                    self.choose_option(p.node, p.hover);
                    return;
                }
                "Escape" | "Tab" => {
                    self.host_mut().close_select();
                    return;
                }
                _ => None,
            };
            if let Some(hv) = new_hover {
                let h = self.host_mut();
                if let Some(sp) = &mut h.select_popup {
                    sp.hover = hv;
                    let rows = sp.geom.map(|g| g.2).unwrap_or(12).max(1);
                    if hv < sp.first {
                        sp.first = hv;
                    } else if hv >= sp.first + rows {
                        sp.first = hv + 1 - rows;
                    }
                }
                h.invalidate();
            }
            return;
        }
        let target = self.host().focus.or(self.host().doc.body()).unwrap_or(0);
        let ok = events::fire(&mut self.interp, Target::Node(target), "keydown", "KeyboardEvent", EventInit { bubbles: true, cancelable: true, props: key_init(key, mods, false) });
        if ok {
            if let Some(t) = text.filter(|t| !t.is_empty() && !mods.ctrl && !mods.meta) {
                let ok2 = events::fire(&mut self.interp, Target::Node(target), "keypress", "KeyboardEvent", EventInit { bubbles: true, cancelable: true, props: key_init(key, mods, false) });
                if ok2 {
                    self.default_key(target, key, Some(t), mods);
                }
            } else {
                self.default_key(target, key, None, mods);
            }
        }
        events::fire(&mut self.interp, Target::Node(target), "keyup", "KeyboardEvent", EventInit { bubbles: true, cancelable: true, props: key_init(key, mods, false) });
        self.after_task();
    }

    fn default_key(&mut self, target: NodeId, key: &str, text: Option<&str>, mods: Mods) {
        if self.host().focus == Some(target) && self.is_range(target) && self.range_key(target, key) {
            return;
        }
        let is_text = self.host().focus == Some(target) && self.host().is_text_control(target);
        if is_text {
            self.edit_text(target, key, text, mods);
            return;
        }
        match key {
            "Tab" => {
                let order = focus_order(&mut self.interp);
                if order.is_empty() {
                    return;
                }
                let cur = self.host().focus.and_then(|f| order.iter().position(|&o| o == f));
                let next = match (cur, mods.shift) {
                    (Some(i), false) => order[(i + 1) % order.len()],
                    (Some(i), true) => order[(i + order.len() - 1) % order.len()],
                    (None, false) => order[0],
                    (None, true) => order[order.len() - 1],
                };
                focus(&mut self.interp, Some(next));
            }
            "Enter" | " " if self.host().doc.tag_is(target, "select") && self.host().focus == Some(target) => {
                self.host_mut().open_select(target);
            }
            "ArrowDown" if mods.alt && self.host().doc.tag_is(target, "select") => {
                self.host_mut().open_select(target);
            }
            "Enter" | " " => {
                let tag = self.host().doc.tag(target).unwrap_or("").to_string();
                let is_activatable = matches!(tag.as_str(), "button" | "summary") || (tag == "a" && key == "Enter") || (tag == "input" && matches!(input_type(&mut self.interp, target).as_str(), "submit" | "button" | "reset" | "checkbox" | "radio"));
                if is_activatable && self.host().focus == Some(target) {
                    click_element(&mut self.interp, target, mods);
                } else if key == " " {
                    let vh = self.host().viewport.1;
                    self.scroll_page(0.0, if mods.shift { -vh * 0.85 } else { vh * 0.85 });
                }
            }
            "ArrowDown" | "ArrowUp" if self.host().doc.tag_is(target, "select") => {
                let h = self.host_mut();
                let opts = h.doc.elements_by_tag(target, "option");
                if !opts.is_empty() {
                    let i = h.selected_index(target).max(0);
                    let ni = if key == "ArrowDown" { (i + 1).min(opts.len() as i32 - 1) } else { (i - 1).max(0) };
                    let v = h.option_value(opts[ni as usize]);
                    h.set_control_value(target, v);
                    events::fire(&mut self.interp, Target::Node(target), "change", "Event", EventInit { bubbles: true, ..Default::default() });
                }
            }
            "ArrowDown" => self.scroll_page(0.0, 40.0),
            "ArrowUp" => self.scroll_page(0.0, -40.0),
            "ArrowRight" => self.scroll_page(40.0, 0.0),
            "ArrowLeft" => self.scroll_page(-40.0, 0.0),
            "PageDown" => {
                let vh = self.host().viewport.1;
                self.scroll_page(0.0, vh * 0.85)
            }
            "PageUp" => {
                let vh = self.host().viewport.1;
                self.scroll_page(0.0, -vh * 0.85)
            }
            "Home" => self.scroll_page(0.0, -1e9),
            "End" => self.scroll_page(0.0, 1e9),
            _ => {}
        }
    }

    fn scroll_page(&mut self, dx: f32, dy: f32) {
        let h = self.host_mut();
        h.scroll.0 += dx;
        h.scroll.1 += dy;
        h.clamp_scroll();
    }

    fn edit_text(&mut self, n: NodeId, key: &str, text: Option<&str>, mods: Mods) {
        let multiline = self.host().doc.tag_is(n, "textarea");
        let readonly = self.host().doc.has_attr(n, "readonly");
        let value = self.host().control_value(n);
        let mut chars: Vec<char> = value.chars().collect();
        let state = self.host().form.get(&n).cloned().unwrap_or_default();
        let mut caret = state.caret.min(chars.len());
        let mut anchor = state.anchor.map(|a| a.min(chars.len()));
        let sel = anchor.filter(|&a| a != caret).map(|a| (a.min(caret), a.max(caret)));
        let mut changed = false;
        let cmd = mods.meta || mods.ctrl;
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        // remove the selected text, if any; returns whether something was deleted
        let delete_sel = |chars: &mut Vec<char>, caret: &mut usize, anchor: &mut Option<usize>| -> bool {
            if let Some((a, b)) = anchor.filter(|&a| a != *caret).map(|a| (a.min(*caret), a.max(*caret))) {
                chars.drain(a..b);
                *caret = a;
                *anchor = None;
                true
            } else {
                *anchor = None;
                false
            }
        };
        // caret movement: shift extends the selection, otherwise it collapses
        let move_to = |target: usize, caret: &mut usize, anchor: &mut Option<usize>| {
            if mods.shift {
                if anchor.is_none() {
                    *anchor = Some(*caret);
                }
            } else {
                *anchor = None;
            }
            *caret = target;
        };
        let word_left = |chars: &[char], from: usize| -> usize {
            let mut i = from;
            while i > 0 && !is_word(chars[i - 1]) {
                i -= 1;
            }
            while i > 0 && is_word(chars[i - 1]) {
                i -= 1;
            }
            i
        };
        let word_right = |chars: &[char], from: usize| -> usize {
            let mut i = from;
            while i < chars.len() && !is_word(chars[i]) {
                i += 1;
            }
            while i < chars.len() && is_word(chars[i]) {
                i += 1;
            }
            i
        };
        match key {
            "Backspace" if !readonly => {
                if delete_sel(&mut chars, &mut caret, &mut anchor) {
                    changed = true;
                } else if caret > 0 {
                    let from = if cmd { 0 } else if mods.alt { word_left(&chars, caret) } else { caret - 1 };
                    chars.drain(from..caret);
                    caret = from;
                    changed = true;
                }
            }
            "Delete" if !readonly => {
                if delete_sel(&mut chars, &mut caret, &mut anchor) {
                    changed = true;
                } else if caret < chars.len() {
                    let to = if mods.alt { word_right(&chars, caret) } else { caret + 1 };
                    chars.drain(caret..to);
                    changed = true;
                }
            }
            "ArrowLeft" => {
                let t = if cmd {
                    0
                } else if mods.alt {
                    word_left(&chars, caret)
                } else if let (Some((a, _)), false) = (sel, mods.shift) {
                    a
                } else {
                    caret.saturating_sub(1)
                };
                move_to(t, &mut caret, &mut anchor);
            }
            "ArrowRight" => {
                let t = if cmd {
                    chars.len()
                } else if mods.alt {
                    word_right(&chars, caret)
                } else if let (Some((_, b)), false) = (sel, mods.shift) {
                    b
                } else {
                    (caret + 1).min(chars.len())
                };
                move_to(t, &mut caret, &mut anchor);
            }
            "Home" | "ArrowUp" if !multiline || key == "Home" => move_to(0, &mut caret, &mut anchor),
            "End" | "ArrowDown" if !multiline || key == "End" => {
                let l = chars.len();
                move_to(l, &mut caret, &mut anchor)
            }
            "Enter" => {
                if multiline && !readonly {
                    delete_sel(&mut chars, &mut caret, &mut anchor);
                    chars.insert(caret, '\n');
                    caret += 1;
                    changed = true;
                } else {
                    // change + implicit submission
                    let cur = self.host().control_value(n);
                    if self.host().focus_value.as_deref() != Some(cur.as_str()) {
                        self.host_mut().focus_value = Some(cur.clone());
                        events::fire(&mut self.interp, Target::Node(n), "change", "Event", EventInit { bubbles: true, ..Default::default() });
                    }
                    let h = self.host_mut();
                    if h.internal {
                        if let Some(action) = h.doc.attr(n, "data-action").map(|s| s.to_string()) {
                            h.outbox.push(HostMsg::Submit { action, value: cur, node: n });
                            return;
                        }
                    }
                    if owner_form(&mut self.interp, n).is_some() {
                        let submitter = {
                            let f = owner_form(&mut self.interp, n).unwrap();
                            let h = self.host();
                            h.doc.element_descendants(f).into_iter().find(|&c| matches!(h.doc.tag(c), Some("button")) && h.doc.attr(c, "type").is_none_or(|t| t.eq_ignore_ascii_case("submit")) || (h.doc.tag_is(c, "input") && h.doc.attr(c, "type").is_some_and(|t| t.eq_ignore_ascii_case("submit"))))
                        };
                        submit_form(&mut self.interp, n, submitter, true);
                    }
                    return;
                }
            }
            "a" if cmd => {
                anchor = Some(0);
                caret = chars.len();
            }
            "v" if cmd && !readonly => {
                let clip = crate::platform::clipboard_get().or_else(|| self.host().clipboard.clone()).unwrap_or_default();
                let clip: String = if multiline { clip } else { clip.replace(['\n', '\r'], " ") };
                let had = delete_sel(&mut chars, &mut caret, &mut anchor);
                for c in clip.chars() {
                    chars.insert(caret, c);
                    caret += 1;
                }
                changed = had || !clip.is_empty();
            }
            "c" | "x" if cmd => {
                let password = self.host().doc.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("password"));
                if let (Some((a, b)), false) = (sel, password) {
                    let v: String = chars[a..b].iter().collect();
                    self.host_mut().clipboard = Some(v.clone());
                    crate::platform::clipboard_set(&v);
                    if key == "x" && !readonly {
                        delete_sel(&mut chars, &mut caret, &mut anchor);
                        changed = true;
                    }
                }
            }
            _ => {
                if let Some(t) = text {
                    if !readonly && !cmd {
                        let maxlen = self.host().doc.attr(n, "maxlength").and_then(|m| m.parse::<usize>().ok()).unwrap_or(usize::MAX);
                        if delete_sel(&mut chars, &mut caret, &mut anchor) {
                            changed = true;
                        }
                        for c in t.chars() {
                            if c == '\r' || (c == '\n' && !multiline) || c.is_control() && c != '\n' {
                                continue;
                            }
                            if chars.len() >= maxlen {
                                break;
                            }
                            chars.insert(caret, c);
                            caret += 1;
                            changed = true;
                        }
                    }
                }
            }
        }
        let h = self.host_mut();
        if changed {
            let v: String = chars.into_iter().collect();
            h.set_control_value(n, v);
        }
        let e = h.form.entry(n).or_default();
        e.caret = caret;
        e.anchor = anchor;
        h.invalidate();
        if changed {
            let data = text.map(Value::str).unwrap_or(Value::Null);
            events::fire(&mut self.interp, Target::Node(n), "input", "InputEvent", EventInit { bubbles: true, props: vec![("data", data), ("inputType", Value::str("insertText"))], ..Default::default() });
        }
    }

    /// Type a string into the focused element, key by key (automation).
    pub fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            let k = if c == '\n' { "Enter".to_string() } else { c.to_string() };
            let t = if c == '\n' { None } else { Some(c.to_string()) };
            self.key_down(&k, t.as_deref(), Mods::default());
        }
    }

    /// Apply pending navigation for same-document changes. Returns other navigations.
    pub fn take_navigation(&mut self) -> Option<NavRequest> {
        let nav = self.host_mut().nav.take()?;
        if let NavRequest::SameDocument { url, replace } = &nav {
            let old = self.host().url.clone();
            let suppress = std::mem::replace(&mut self.host_mut().suppress_hashchange, false);
            self.host_mut().url = url.clone();
            if !suppress && url.fragment != old.fragment {
                if let Some(f) = url.fragment.clone() {
                    self.scroll_to_anchor(&f);
                }
                let init = EventInit { props: vec![("oldURL", Value::str(&old.to_string())), ("newURL", Value::str(&url.to_string()))], ..Default::default() };
                events::fire(&mut self.interp, Target::Window, "hashchange", "HashChangeEvent", init);
                self.after_task();
            }
            let _ = replace;
        }
        Some(nav)
    }

    pub fn cursor(&self) -> Cursor {
        self.host().cursor
    }

    pub fn find(&self, selector: &str) -> Result<Option<NodeId>, String> {
        crate::css::query_selector(&self.host().doc, 0, selector)
    }

    pub fn find_all(&self, selector: &str) -> Result<Vec<NodeId>, String> {
        crate::css::query_selector_all(&self.host().doc, 0, selector)
    }

    pub fn text_of(&self, n: NodeId) -> String {
        let h = self.host();
        match &h.doc.node(n).data {
            NodeData::Element(_) => crate::dom::collapse_ws(&h.doc.text_content(n)),
            NodeData::Text(t) => t.clone(),
            _ => String::new(),
        }
    }

    pub fn resolve(&self, href: &str) -> Option<Url> {
        self.host().resolve_url(href)
    }
}
