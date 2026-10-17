//! A loaded document: DOM + styles + layout + scripting + resources.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::css::{self, MatchState, Stylesheet};
use crate::dom::{Document, NodeData, NodeId};
use crate::js::{Interp, Value};
use crate::layout::{self, LayoutResult};
use crate::net::{self, NetLogEntry, Request, Url};
use crate::paint::{self, ControlState, DisplayList};
use crate::raster::{Image, Rect};
use crate::style::{self, Origin, RuleSet, Styles};

#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub level: &'static str,
    pub text: String,
    pub source: String,
}

#[derive(Debug, Clone, Default)]
pub struct FormState {
    pub value: Option<String>,
    pub checked: Option<bool>,
    pub caret: usize,
    /// Selection anchor (char index); the selection spans anchor..caret.
    pub anchor: Option<usize>,
}

impl FormState {
    /// Ordered, non-empty selection range.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.caret {
            None
        } else {
            Some((a.min(self.caret), a.max(self.caret)))
        }
    }
}

/// An open `<select>` dropdown.
#[derive(Debug, Clone)]
pub struct SelectPopup {
    pub node: NodeId,
    pub hover: usize,
    pub first: usize,
    /// Geometry in document coordinates, filled in when painted: (rect, row height, visible rows).
    pub geom: Option<(Rect, f32, usize)>,
}

const POPUP_MAX_ROWS: usize = 12;

#[derive(Debug, Clone)]
pub enum NavRequest {
    Load { url: Url, method: String, body: Option<Vec<u8>>, replace: bool, headers: Vec<(String, String)> },
    Reload,
    History(i32),
    /// pushState/replaceState or hash change: URL changes without a load
    SameDocument { url: Url, replace: bool },
    Open(Url),
}

/// Messages from internal UI pages (browser chrome, DevTools) to the app.
#[derive(Debug, Clone)]
pub enum HostMsg {
    Action { action: String, arg: String, node: NodeId },
    Submit { action: String, value: String, node: NodeId },
}

pub enum ImageEntry {
    Loading,
    Loaded(Rc<Image>),
    Failed,
}

#[derive(Clone)]
pub struct Listener {
    pub ty: String,
    pub callback: Value,
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Target {
    Node(NodeId),
    Window,
    /// Any other EventTarget (XHR, AbortSignal, user subclasses), keyed by object identity.
    Object(usize),
}

pub struct MutationObs {
    pub observer: Value,
    pub callback: Value,
    pub target: NodeId,
    pub version: u64,
}

pub struct Timer {
    pub id: u32,
    pub due: Instant,
    pub interval: Option<Duration>,
    pub callback: Value,
    pub args: Vec<Value>,
}

struct SheetCacheEntry {
    text_len: usize,
    text_hash: u64,
    sheet: Rc<Stylesheet>,
}

pub struct PageHost {
    pub doc: Document,
    pub url: Url,
    pub viewport: (f32, f32),
    pub scale: f32,
    pub scroll: (f32, f32),
    pub elem_scroll: HashMap<NodeId, (f32, f32)>,
    pub hover: Option<NodeId>,
    pub focus: Option<NodeId>,
    pub active: Option<NodeId>,
    pub highlight: Option<NodeId>,
    pub form: HashMap<NodeId, FormState>,
    pub images: HashMap<String, ImageEntry>,
    pub listeners: HashMap<Target, Vec<Listener>>,
    pub wrappers: HashMap<NodeId, crate::js::Obj>,
    pub timers: Vec<Timer>,
    pub next_timer: u32,
    pub raf: Vec<(u32, Value)>,
    pub console: Vec<ConsoleEntry>,
    pub net_log: Vec<NetLogEntry>,
    pub nav: Option<NavRequest>,
    pub outbox: Vec<HostMsg>,
    pub alerts: Vec<String>,
    pub internal: bool,
    pub scripting: bool,
    pub ready_state: &'static str,
    pub render_gen: u64,
    pub listener_objects: HashMap<usize, Value>,
    pub current_script: Option<NodeId>,
    pub pending_fetches: Vec<crate::bindings::window::PendingFetch>,
    pub net_log_pending: Vec<(String, String, Instant)>,
    pub mutation_observers: Vec<MutationObs>,
    pub history_len: usize,
    pub history_state: Value,
    pub suppress_hashchange: bool,
    pub clipboard: Option<String>,
    pub dialog_answer: Option<bool>,
    pub focus_value: Option<String>,
    pub mouse_down_target: Option<NodeId>,
    pub cursor: crate::style::Cursor,
    pub select_popup: Option<SelectPopup>,
    /// Text control being drag-selected while the mouse button is held.
    pub drag_select: Option<NodeId>,
    /// Range input whose thumb is being dragged.
    pub drag_range: Option<NodeId>,
    // caches
    linked_sheets: HashMap<String, Option<Rc<Stylesheet>>>,
    inline_sheets: HashMap<NodeId, SheetCacheEntry>,
    ua_sheet: Rc<Stylesheet>,
    rules: Option<(u64, Rc<RuleSet>, Vec<SheetInfo>)>,
    styles: Option<((u64, u64, (u32, u32)), Rc<Styles>)>,
    layout: Option<((u64, u64, (u32, u32)), Rc<LayoutResult>)>,
    display: Option<((u64, u64, (u32, u32), Option<NodeId>), Rc<DisplayList>)>,
    img_tx: Sender<(String, Result<Image, String>, NetLogEntry)>,
    img_rx: Receiver<(String, Result<Image, String>, NetLogEntry)>,
    img_inflight: usize,
    img_queue: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SheetInfo {
    pub source: String,
    pub origin: Origin,
}

pub fn hash_str(s: &str) -> u64 {
    // FNV-1a
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

thread_local! {
    static UA_SHEET: Rc<Stylesheet> = Rc::new(css::parse_stylesheet(style::UA_CSS));
}

impl PageHost {
    pub fn new(doc: Document, url: Url, viewport: (f32, f32)) -> PageHost {
        let (img_tx, img_rx) = channel();
        PageHost {
            doc,
            url,
            viewport,
            scale: 1.0,
            scroll: (0.0, 0.0),
            elem_scroll: HashMap::new(),
            hover: None,
            focus: None,
            active: None,
            highlight: None,
            form: HashMap::new(),
            images: HashMap::new(),
            listeners: HashMap::new(),
            wrappers: HashMap::new(),
            timers: vec![],
            next_timer: 1,
            raf: vec![],
            console: vec![],
            net_log: vec![],
            nav: None,
            outbox: vec![],
            alerts: vec![],
            internal: false,
            scripting: true,
            ready_state: "loading",
            render_gen: 0,
            listener_objects: HashMap::new(),
            current_script: None,
            pending_fetches: vec![],
            net_log_pending: vec![],
            mutation_observers: vec![],
            history_len: 1,
            history_state: Value::Null,
            suppress_hashchange: false,
            clipboard: None,
            dialog_answer: None,
            focus_value: None,
            mouse_down_target: None,
            cursor: crate::style::Cursor::Default,
            select_popup: None,
            drag_select: None,
            drag_range: None,
            linked_sheets: HashMap::new(),
            inline_sheets: HashMap::new(),
            ua_sheet: UA_SHEET.with(|s| s.clone()),
            rules: None,
            styles: None,
            layout: None,
            display: None,
            img_tx,
            img_rx,
            img_inflight: 0,
            img_queue: vec![],
        }
    }

    pub fn invalidate(&mut self) {
        self.render_gen += 1;
    }

    /// Headers a browser attaches to requests initiated by this page (Origin / Referer),
    /// following the default `strict-origin-when-cross-origin` referrer policy.
    pub fn request_headers(&self, method: &str, target: &Url) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let me = &self.url;
        let web = matches!(me.scheme.as_str(), "http" | "https");
        let same_origin = me.origin() == target.origin();
        if web && !(me.scheme == "https" && target.scheme == "http") {
            let r = if same_origin { me.without_fragment().to_string() } else { format!("{}/", me.origin()) };
            out.push(("Referer".to_string(), r));
        }
        if method != "GET" && method != "HEAD" || !same_origin {
            out.push(("Origin".to_string(), if web { me.origin() } else { "null".into() }));
        }
        out
    }

    pub fn resolve_url(&self, s: &str) -> Option<Url> {
        let base = self
            .doc
            .elements_by_tag(0, "base")
            .first()
            .and_then(|&b| self.doc.attr(b, "href"))
            .and_then(|h| self.url.join(h))
            .unwrap_or_else(|| self.url.clone());
        base.join(s)
    }

    // ------------------------------------------------------------ stylesheets

    fn fetch_sheet(&mut self, url: &Url) -> Option<Rc<Stylesheet>> {
        let key = url.to_string();
        if let Some(s) = self.linked_sheets.get(&key) {
            return s.clone();
        }
        let r = net::fetch_logged(&Request::get(url.clone()), "stylesheet", &mut self.net_log);
        let sheet = match r {
            Ok(resp) if resp.ok() => {
                let mut sheet = css::parse_stylesheet(&resp.text());
                self.resolve_sheet_urls(&mut sheet, url);
                self.inline_imports(&mut sheet, url, 0);
                Some(Rc::new(sheet))
            }
            _ => None,
        };
        self.linked_sheets.insert(key, sheet.clone());
        sheet
    }

    fn inline_imports(&mut self, sheet: &mut Stylesheet, base: &Url, depth: u32) {
        if sheet.imports.is_empty() || depth > 4 {
            return;
        }
        let mut pre = Vec::new();
        for imp in std::mem::take(&mut sheet.imports) {
            if let Some(u) = base.join(&imp) {
                if let Some(s) = self.fetch_sheet(&u) {
                    pre.extend(s.rules.iter().cloned());
                }
            }
        }
        pre.append(&mut sheet.rules);
        sheet.rules = pre;
    }

    /// Make url(...) values in background declarations absolute.
    fn resolve_sheet_urls(&self, sheet: &mut Stylesheet, base: &Url) {
        for r in &mut sheet.rules {
            for d in &mut r.decls {
                for t in &mut d.value {
                    match t {
                        css::Tok::Url(u) => {
                            if let Some(abs) = base.join(u) {
                                *u = abs.to_string();
                            }
                        }
                        _ => {}
                    }
                }
                // url("...") as a function with a string argument
                let n = d.value.len();
                for i in 0..n {
                    if matches!(&d.value[i], css::Tok::Function(f) if f == "url") {
                        if let Some(css::Tok::Str(s)) = d.value.get(i + 1).cloned() {
                            if let Some(abs) = base.join(&s) {
                                d.value[i + 1] = css::Tok::Str(abs.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    fn collect_sheets(&mut self) -> (Vec<(Origin, Rc<Stylesheet>)>, Vec<SheetInfo>) {
        let mut out = vec![(Origin::UserAgent, self.ua_sheet.clone())];
        let mut info = vec![SheetInfo { source: "user agent stylesheet".into(), origin: Origin::UserAgent }];
        let nodes: Vec<NodeId> = self.doc.element_descendants(0).into_iter().filter(|&n| matches!(self.doc.tag(n), Some("style" | "link"))).collect();
        for n in nodes {
            let tag = self.doc.tag(n).unwrap_or("").to_string();
            if tag == "style" {
                if self.doc.attr(n, "media").is_some_and(|m| m.contains("print")) {
                    continue;
                }
                let text = self.doc.text_content(n);
                let h = hash_str(&text);
                let cached = self.inline_sheets.get(&n).filter(|e| e.text_len == text.len() && e.text_hash == h).map(|e| e.sheet.clone());
                let sheet = match cached {
                    Some(s) => s,
                    None => {
                        let mut s = css::parse_stylesheet(&text);
                        let base = self.url.clone();
                        self.resolve_sheet_urls(&mut s, &base);
                        self.inline_imports(&mut s, &base, 0);
                        let rc = Rc::new(s);
                        self.inline_sheets.insert(n, SheetCacheEntry { text_len: text.len(), text_hash: h, sheet: rc.clone() });
                        rc
                    }
                };
                out.push((Origin::Author, sheet));
                info.push(SheetInfo { source: "<style>".into(), origin: Origin::Author });
            } else {
                let rel = self.doc.attr(n, "rel").unwrap_or("").to_ascii_lowercase();
                if !rel.split_whitespace().any(|r| r == "stylesheet") || rel.contains("alternate") {
                    continue;
                }
                if self.doc.attr(n, "media").is_some_and(|m| m.trim() == "print") {
                    continue;
                }
                let Some(href) = self.doc.attr(n, "href") else { continue };
                let Some(u) = self.resolve_url(href) else { continue };
                if let Some(s) = self.fetch_sheet(&u) {
                    out.push((Origin::Author, s));
                    info.push(SheetInfo { source: u.to_string(), origin: Origin::Author });
                }
            }
        }
        (out, info)
    }

    pub fn sheet_infos(&mut self) -> Vec<SheetInfo> {
        self.rules().2.clone()
    }

    fn rules(&mut self) -> &(u64, Rc<RuleSet>, Vec<SheetInfo>) {
        let v = self.doc.version;
        if self.rules.as_ref().is_none_or(|r| r.0 != v) {
            let (sheets, info) = self.collect_sheets();
            let rs = Rc::new(RuleSet::new(&sheets));
            self.rules = Some((v, rs, info));
        }
        self.rules.as_ref().unwrap()
    }

    fn vp_key(&self) -> (u32, u32) {
        (self.viewport.0.round() as u32, self.viewport.1.round() as u32)
    }

    pub fn matched_rules(&mut self, n: NodeId) -> Vec<style::MatchedRule> {
        let rules = self.rules().1.clone();
        let ctx = style::StyleContext { doc: &self.doc, rules: &rules, state: MatchState { hover: self.hover, focus: self.focus, active: self.active }, viewport: self.viewport };
        style::matched_rules(&ctx, n)
    }

    pub fn styles(&mut self) -> Rc<Styles> {
        let key = (self.doc.version, self.render_gen, self.vp_key());
        if let Some((k, s)) = &self.styles {
            if *k == key {
                return s.clone();
            }
        }
        let rules = self.rules().1.clone();
        let ctx = style::StyleContext { doc: &self.doc, rules: &rules, state: MatchState { hover: self.hover, focus: self.focus, active: self.active }, viewport: self.viewport };
        let s = Rc::new(style::compute_styles(&ctx));
        self.styles = Some((key, s.clone()));
        s
    }

    pub fn layout(&mut self) -> Rc<LayoutResult> {
        let key = (self.doc.version, self.render_gen, self.vp_key());
        if let Some((k, l)) = &self.layout {
            if *k == key {
                return l.clone();
            }
        }
        let styles = self.styles();
        let env = Env { host: self };
        let l = Rc::new(layout::layout_document(&self.doc, &styles, &env, self.viewport.0, self.viewport.1));
        for u in take_missing() {
            self.request_image(u);
        }
        self.layout = Some((key, l.clone()));
        l
    }

    pub fn display_list(&mut self) -> Rc<DisplayList> {
        let key = (self.doc.version, self.render_gen, self.vp_key(), self.highlight);
        if let Some((k, d)) = &self.display {
            if *k == key {
                return d.clone();
            }
        }
        let l = self.layout();
        let mut dl = paint::build_display_list(&l, &Env { host: self });
        if let Some(h) = self.highlight {
            add_highlight(&mut dl, &l, h);
        }
        if self.select_popup.is_some() {
            self.paint_select_popup(&mut dl);
            // popup geometry is part of the cached frame
        }
        for u in take_missing() {
            self.request_image(u);
        }
        let dl = Rc::new(dl);
        self.display = Some((key, dl.clone()));
        dl
    }

    pub fn doc_size(&mut self) -> (f32, f32) {
        let l = self.layout();
        (l.doc_width, l.doc_height)
    }

    pub fn max_scroll(&mut self) -> (f32, f32) {
        let (w, h) = self.doc_size();
        ((w - self.viewport.0).max(0.0), (h - self.viewport.1).max(0.0))
    }

    pub fn clamp_scroll(&mut self) {
        let (mx, my) = self.max_scroll();
        self.scroll = (self.scroll.0.clamp(0.0, mx), self.scroll.1.clamp(0.0, my));
    }

    pub fn hit_test(&mut self, x: f32, y: f32) -> Option<NodeId> {
        let dl = self.display_list();
        let sc = self.scroll;
        dl.hit_test(x + sc.0, y + sc.1, sc)
    }

    /// Bounding rect in document coordinates.
    pub fn node_rect(&mut self, n: NodeId) -> Option<Rect> {
        let l = self.layout();
        let r = l.bounding_rect(n);
        if r.is_some() {
            return r;
        }
        // text nodes and display:contents: union of children
        let kids = self.doc.children(n).to_vec();
        let mut acc: Option<Rect> = None;
        for k in kids {
            if let Some(r) = l.bounding_rect(k) {
                acc = Some(acc.map(|a| a.union(&r)).unwrap_or(r));
            }
        }
        if acc.is_none() {
            if let Some(p) = self.doc.parent(n) {
                if matches!(self.doc.node(n).data, NodeData::Text(_)) {
                    return l.bounding_rect(p);
                }
            }
        }
        acc
    }

    // ------------------------------------------------------------ images

    pub fn image_url_for(&self, node: NodeId) -> Option<String> {
        let src = self.doc.attr(node, "src").filter(|s| !s.is_empty()).or_else(|| {
            self.doc.attr(node, "srcset").and_then(|ss| ss.split(',').next()).map(|c| c.split_whitespace().next().unwrap_or(""))
        })?;
        Some(self.resolve_url(src)?.to_string())
    }

    pub fn request_image(&mut self, url: String) {
        if self.images.contains_key(&url) {
            return;
        }
        self.images.insert(url.clone(), ImageEntry::Loading);
        self.img_queue.push(url);
        self.pump_image_queue();
    }

    fn pump_image_queue(&mut self) {
        while self.img_inflight < 6 && !self.img_queue.is_empty() {
            let url = self.img_queue.remove(0);
            self.img_inflight += 1;
            let tx = self.img_tx.clone();
            std::thread::spawn(move || {
                let mut log = Vec::new();
                let res = match Url::parse(&url) {
                    Some(u) => net::fetch_logged(&Request::get(u), "image", &mut log).and_then(|r| {
                        if !r.ok() {
                            return Err(format!("HTTP {}", r.status));
                        }
                        crate::image::decode(&r.body)
                    }),
                    None => Err("bad url".into()),
                };
                let entry = log.pop().unwrap_or(NetLogEntry { url: url.clone(), method: "GET".into(), kind: "image", status: 0, size: 0, mime: String::new(), ms: 0, error: None });
                let _ = tx.send((url, res, entry));
            });
        }
    }

    /// Integrate finished image loads; returns true if anything changed.
    pub fn poll_images(&mut self) -> bool {
        let mut changed = false;
        while let Ok((url, res, entry)) = self.img_rx.try_recv() {
            self.img_inflight = self.img_inflight.saturating_sub(1);
            let entry = match &res {
                Err(e) if entry.error.is_none() && entry.status != 0 => NetLogEntry { error: Some(e.clone()), ..entry },
                _ => entry,
            };
            self.net_log.push(entry);
            self.images.insert(url, match res {
                Ok(img) => ImageEntry::Loaded(Rc::new(img)),
                Err(_) => ImageEntry::Failed,
            });
            changed = true;
        }
        if changed {
            self.pump_image_queue();
            self.invalidate();
        }
        changed
    }

    pub fn images_pending(&self) -> bool {
        self.img_inflight > 0 || !self.img_queue.is_empty()
    }

    // ------------------------------------------------------------ form controls

    pub fn control_value(&self, n: NodeId) -> String {
        if let Some(v) = self.form.get(&n).and_then(|f| f.value.clone()) {
            return v;
        }
        match self.doc.tag(n) {
            Some("textarea") => self.doc.text_content(n),
            Some("select") => {
                let opts = self.doc.elements_by_tag(n, "option");
                let sel = opts.iter().find(|&&o| self.doc.has_attr(o, "selected")).or(opts.first()).copied();
                sel.map(|o| self.option_value(o)).unwrap_or_default()
            }
            Some("input") if self.doc.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("range")) => {
                let (min, max, _) = self.range_params(n);
                let v = self.doc.attr(n, "value").and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(min + (max - min) / 2.0);
                self.sanitize_range(n, v)
            }
            _ => self.doc.attr(n, "value").unwrap_or("").to_string(),
        }
    }

    /// (min, max, step) of a range input, with HTML defaults 0/100/1.
    pub fn range_params(&self, n: NodeId) -> (f64, f64, f64) {
        let num = |a: &str, d: f64| self.doc.attr(n, a).and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(d);
        let min = num("min", 0.0);
        let max = num("max", 100.0).max(min);
        let step = self.doc.attr(n, "step").and_then(|v| v.trim().parse::<f64>().ok()).filter(|s| *s > 0.0).unwrap_or(1.0);
        (min, max, step)
    }

    /// Clamp and snap a value to a range input's step, formatted like browsers do.
    pub fn sanitize_range(&self, n: NodeId, v: f64) -> String {
        let (min, max, step) = self.range_params(n);
        let v = if v.is_nan() { min + (max - min) / 2.0 } else { v };
        let snapped = (min + ((v - min) / step).round() * step).clamp(min, max);
        // avoid floating noise like 0.30000000000000004
        let s = format!("{:.10}", snapped);
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s == "-0" { "0".into() } else { s.to_string() }
    }

    pub fn option_value(&self, o: NodeId) -> String {
        self.doc.attr(o, "value").map(|s| s.to_string()).unwrap_or_else(|| crate::dom::collapse_ws(&self.doc.text_content(o)))
    }

    pub fn set_control_value(&mut self, n: NodeId, v: String) {
        let caret = v.chars().count();
        let e = self.form.entry(n).or_default();
        e.value = Some(v);
        e.caret = caret;
        e.anchor = None;
        self.invalidate();
    }

    /// Option elements of a select with their labels and disabled state.
    pub fn select_options(&self, sel: NodeId) -> Vec<(NodeId, String, bool)> {
        self.doc
            .elements_by_tag(sel, "option")
            .into_iter()
            .map(|o| {
                let label = self.doc.attr(o, "label").map(|s| s.to_string()).unwrap_or_else(|| crate::dom::collapse_ws(&self.doc.text_content(o)));
                let disabled = self.doc.has_attr(o, "disabled") || self.doc.parent(o).is_some_and(|p| self.doc.tag_is(p, "optgroup") && self.doc.has_attr(p, "disabled"));
                (o, label, disabled)
            })
            .collect()
    }

    pub fn open_select(&mut self, sel: NodeId) {
        let n = self.select_options(sel).len();
        if n == 0 {
            return;
        }
        let cur = self.selected_index(sel).max(0) as usize;
        let first = cur.saturating_sub(POPUP_MAX_ROWS / 2).min(n.saturating_sub(POPUP_MAX_ROWS));
        self.select_popup = Some(SelectPopup { node: sel, hover: cur, first, geom: None });
        self.invalidate();
    }

    pub fn close_select(&mut self) {
        if self.select_popup.take().is_some() {
            self.invalidate();
        }
    }

    /// Which popup row (option index) is at a document point, if inside the popup.
    pub fn popup_row_at(&self, x: f32, y: f32) -> Option<Option<usize>> {
        let p = self.select_popup.as_ref()?;
        let (r, rh, _) = p.geom?;
        if !r.contains(x, y) {
            return Some(None);
        }
        let row = ((y - r.y - 4.0) / rh).floor().max(0.0) as usize;
        Some(Some(p.first + row))
    }

    fn paint_select_popup(&mut self, dl: &mut DisplayList) {
        use crate::css::Color;
        let Some(p) = self.select_popup.clone() else { return };
        let Some(anchor) = self.node_rect(p.node) else { return };
        let styles = self.styles();
        let Some(st) = styles.get(p.node).cloned() else { return };
        let opts = self.select_options(p.node);
        let key = paint::key_of(&st);
        let fs = st.font_size.max(12.0);
        let rh = (fs * 1.6).round();
        let widest = opts.iter().map(|(_, l, _)| crate::text::measure(key, fs, l, 0.0)).fold(0.0, f32::max);
        let w = anchor.w.max(widest + 40.0);
        let rows = opts.len().min(POPUP_MAX_ROWS);
        let h = rows as f32 * rh + 8.0;
        let vp_bottom = self.scroll.1 + self.viewport.1;
        let below = anchor.bottom() + 2.0;
        let y = if below + h > vp_bottom && anchor.y - h - 2.0 >= self.scroll.1 { anchor.y - h - 2.0 } else { below };
        let rect = Rect::new(anchor.x, y, w, h);
        let current = self.selected_index(p.node).max(0) as usize;
        dl.items.push(paint::Item::SetFixed(false));
        dl.items.push(paint::Item::Shadow { rect, radii: [8.0; 4], color: Color::rgba(0, 0, 0, 60), blur: 14.0 });
        dl.items.push(paint::Item::Rect { rect, color: Color::WHITE, radii: [8.0; 4] });
        dl.items.push(paint::Item::Ring { outer: rect, outer_radii: [8.0; 4], inner: rect.inflate(-1.0), inner_radii: [7.0; 4], color: Color::rgb(0xc8, 0xcc, 0xd4) });
        for (i, (_, label, disabled)) in opts.iter().enumerate().skip(p.first).take(rows) {
            let ry = y + 4.0 + (i - p.first) as f32 * rh;
            let row = Rect::new(anchor.x + 4.0, ry, w - 8.0, rh);
            let hovered = i == p.hover && !disabled;
            if hovered {
                dl.items.push(paint::Item::Rect { rect: row, color: Color::rgb(0x0b, 0x57, 0xd0), radii: [5.0; 4] });
            }
            let color = if hovered { Color::WHITE } else if *disabled { Color::rgb(0x9c, 0xa3, 0xaf) } else { Color::rgb(0x11, 0x18, 0x27) };
            if i == current {
                dl.items.push(paint::Item::Text { x: row.x + 6.0, baseline: ry + rh * 0.68, text: "✓".into(), font: key, size: fs, color, letter_spacing: 0.0 });
            }
            dl.items.push(paint::Item::Text { x: row.x + 22.0, baseline: ry + rh * 0.68, text: label.clone(), font: key, size: fs, color, letter_spacing: 0.0 });
        }
        if opts.len() > rows {
            // scroll indicator
            let track = h - 8.0;
            let th = (track * rows as f32 / opts.len() as f32).max(16.0);
            let ty = y + 4.0 + (track - th) * (p.first as f32 / (opts.len() - rows) as f32);
            dl.items.push(paint::Item::Rect { rect: Rect::new(rect.right() - 6.0, ty, 3.0, th), color: Color::rgba(0, 0, 0, 80), radii: [1.5; 4] });
        }
        if let Some(sp) = &mut self.select_popup {
            sp.geom = Some((rect, rh, rows));
        }
    }

    pub fn is_checked(&self, n: NodeId) -> bool {
        self.form.get(&n).and_then(|f| f.checked).unwrap_or_else(|| self.doc.has_attr(n, "checked"))
    }

    pub fn set_checked(&mut self, n: NodeId, c: bool) {
        self.form.entry(n).or_default().checked = Some(c);
        if c && self.doc.attr(n, "type").is_some_and(|t| t.eq_ignore_ascii_case("radio")) {
            // uncheck others in the same group
            let name = self.doc.attr(n, "name").map(|s| s.to_string());
            if let Some(name) = name {
                let others: Vec<NodeId> = self.doc.elements_by_tag(0, "input").into_iter().filter(|&o| o != n && self.doc.attr(o, "type").is_some_and(|t| t.eq_ignore_ascii_case("radio")) && self.doc.attr(o, "name") == Some(name.as_str())).collect();
                for o in others {
                    self.form.entry(o).or_default().checked = Some(false);
                }
            }
        }
        self.invalidate();
    }

    pub fn selected_index(&self, sel: NodeId) -> i32 {
        let v = self.control_value(sel);
        let opts = self.doc.elements_by_tag(sel, "option");
        opts.iter().position(|&o| self.option_value(o) == v).map(|i| i as i32).unwrap_or(if opts.is_empty() { -1 } else { 0 })
    }

    pub fn is_text_control(&self, n: NodeId) -> bool {
        match self.doc.tag(n) {
            Some("textarea") => true,
            Some("input") => !matches!(
                self.doc.attr(n, "type").unwrap_or("text").to_ascii_lowercase().as_str(),
                "checkbox" | "radio" | "submit" | "button" | "reset" | "hidden" | "image" | "file" | "range" | "color"
            ),
            _ => false,
        }
    }

    pub fn is_focusable(&self, n: NodeId) -> bool {
        if self.doc.has_attr(n, "disabled") {
            return false;
        }
        match self.doc.tag(n) {
            Some("input") => self.doc.attr(n, "type").is_none_or(|t| !t.eq_ignore_ascii_case("hidden")),
            Some("textarea" | "select" | "button") => true,
            Some("a") => self.doc.has_attr(n, "href"),
            Some(_) => self.doc.attr(n, "tabindex").is_some_and(|t| t.trim().parse::<i32>().is_ok_and(|v| v >= 0)) || self.doc.has_attr(n, "contenteditable"),
            None => false,
        }
    }

    pub fn log(&mut self, level: &'static str, text: String) {
        self.console.push(ConsoleEntry { level, text, source: String::new() });
    }
}

fn add_highlight(dl: &mut DisplayList, l: &LayoutResult, n: NodeId) {
    use crate::css::Color;
    let Some(b) = l.find_box(n) else {
        if let Some(r) = l.bounding_rect(n) {
            dl.items.push(paint::Item::Rect { rect: r, color: Color::rgba(111, 168, 220, 110), radii: [0.0; 4] });
        }
        return;
    };
    let mb = b.margin_box();
    let bb = b.border_box();
    let pb = b.padding_box();
    let cb = b.content;
    let fixed = b.fixed;
    dl.items.push(paint::Item::SetFixed(fixed));
    dl.items.push(paint::Item::Ring { outer: mb, outer_radii: [0.0; 4], inner: bb, inner_radii: [0.0; 4], color: Color::rgba(246, 178, 107, 110) });
    dl.items.push(paint::Item::Ring { outer: bb, outer_radii: [0.0; 4], inner: pb, inner_radii: [0.0; 4], color: Color::rgba(255, 229, 153, 120) });
    dl.items.push(paint::Item::Ring { outer: pb, outer_radii: [0.0; 4], inner: cb, inner_radii: [0.0; 4], color: Color::rgba(147, 196, 125, 110) });
    dl.items.push(paint::Item::Rect { rect: cb, color: Color::rgba(111, 168, 220, 110), radii: [0.0; 4] });
    dl.items.push(paint::Item::SetFixed(false));
}

thread_local! {
    static MISSING: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn take_missing() -> Vec<String> {
    MISSING.with(|m| std::mem::take(&mut *m.borrow_mut()))
}

/// Layout/paint environment view of a page.
struct Env<'a> {
    host: &'a PageHost,
}

impl Env<'_> {
    fn image(&self, node: NodeId) -> Option<Rc<Image>> {
        let url = self.host.image_url_for(node)?;
        match self.host.images.get(&url) {
            Some(ImageEntry::Loaded(i)) => Some(i.clone()),
            Some(_) => None,
            None => {
                MISSING.with(|m| m.borrow_mut().push(url));
                None
            }
        }
    }
}

impl layout::LayoutEnv for Env<'_> {
    fn image_size(&self, node: NodeId) -> Option<(f32, f32)> {
        self.image(node).map(|i| (i.w as f32, i.h as f32))
    }
    fn scroll_offset(&self, node: NodeId) -> (f32, f32) {
        self.host.elem_scroll.get(&node).copied().unwrap_or((0.0, 0.0))
    }
}

impl paint::PaintEnv for Env<'_> {
    fn node_image(&self, node: NodeId) -> Option<Rc<Image>> {
        self.image(node)
    }
    fn url_image(&self, url: &str) -> Option<Rc<Image>> {
        match self.host.images.get(url) {
            Some(ImageEntry::Loaded(i)) => Some(i.clone()),
            Some(_) => None,
            None => {
                MISSING.with(|m| m.borrow_mut().push(url.to_string()));
                None
            }
        }
    }
    fn control(&self, node: NodeId) -> ControlState {
        let h = self.host;
        let doc = &h.doc;
        let focused = h.focus == Some(node);
        match doc.tag(node) {
            Some("img") => ControlState { text: doc.attr(node, "alt").unwrap_or("").to_string(), placeholder: false, checked: false, focused: false, caret: None, sel: None, fraction: None },
            Some("select") => {
                let v = h.control_value(node);
                let opts = doc.elements_by_tag(node, "option");
                let label = opts.iter().find(|&&o| h.option_value(o) == v).map(|&o| crate::dom::collapse_ws(&doc.text_content(o))).unwrap_or_default();
                ControlState { text: label, placeholder: false, checked: false, focused, caret: None, sel: None, fraction: None }
            }
            _ => {
                let ty = doc.attr(node, "type").unwrap_or("text").to_ascii_lowercase();
                if ty == "range" {
                    let (min, max, _) = h.range_params(node);
                    let v: f64 = h.control_value(node).parse().unwrap_or(min);
                    let f = if max > min { ((v - min) / (max - min)) as f32 } else { 0.0 };
                    return ControlState { text: String::new(), placeholder: false, checked: false, focused, caret: None, sel: None, fraction: Some(f) };
                }
                if ty == "checkbox" || ty == "radio" {
                    return ControlState { text: String::new(), placeholder: false, checked: h.is_checked(node), focused, caret: None, sel: None, fraction: None };
                }
                let v = h.control_value(node);
                let caret = h.form.get(&node).map(|f| f.caret).unwrap_or(v.chars().count());
                let sel = if focused { h.form.get(&node).and_then(|f| f.selection()) } else { None };
                if v.is_empty() {
                    let ph = doc.attr(node, "placeholder").unwrap_or("").to_string();
                    return ControlState { text: ph, placeholder: true, checked: false, focused, caret: Some(0), sel: None, fraction: None };
                }
                let shown = if ty == "password" { "•".repeat(v.chars().count()) } else { v };
                ControlState { text: shown, placeholder: false, checked: false, focused, caret: Some(caret), sel, fraction: None }
            }
        }
    }
}

// ---------------------------------------------------------------- Page

/// Fetches ES modules for a page over the network (with the page's cookies and net log).
struct PageModuleLoader;

impl crate::js::module::ModuleLoader for PageModuleLoader {
    fn fetch(&self, it: &mut Interp, url: &str) -> Result<(String, String), String> {
        let u = Url::parse(url).ok_or_else(|| format!("invalid URL {url}"))?;
        let mut log = Vec::new();
        let r = net::fetch_logged(&Request::get(u), "script", &mut log);
        if let Some(h) = it.host_mut::<PageHost>() {
            h.net_log.extend(log);
        }
        // network time does not count against the script deadline
        if it.deadline.is_some() {
            it.deadline = Some(Instant::now() + Duration::from_secs(10));
        }
        let resp = r?;
        if !resp.ok() {
            return Err(format!("HTTP {}", resp.status));
        }
        let final_url = if resp.url.is_empty() { url.to_string() } else { resp.url.clone() };
        Ok((final_url, resp.text()))
    }

    fn prefetch(&self, _it: &mut Interp, urls: &[String]) {
        let list: Vec<Url> = urls.iter().filter_map(|u| Url::parse(u)).filter(|u| matches!(u.scheme.as_str(), "http" | "https")).collect();
        if list.len() > 1 {
            net::prefetch_all(list);
        }
    }

    fn base_url(&self, it: &mut Interp) -> String {
        it.host::<PageHost>().and_then(|h| h.resolve_url("")).map(|u| u.to_string()).unwrap_or_default()
    }
}

pub struct Page {
    pub interp: Box<Interp>,
}

pub struct LoadOptions {
    pub viewport: (f32, f32),
    pub scale: f32,
    pub scripting: bool,
    pub internal: bool,
}

impl Default for LoadOptions {
    fn default() -> Self {
        LoadOptions { viewport: (1024.0, 768.0), scale: 1.0, scripting: true, internal: false }
    }
}

pub fn error_page(url: &str, err: &str) -> String {
    format!(
        "<!doctype html><html><head><title>Problem loading page</title><style>
        body {{ font-family: sans-serif; background: #f8f9fb; color: #1f2937; margin: 0 }}
        .box {{ max-width: 560px; margin: 96px auto; padding: 32px; background: white; border: 1px solid #e5e7eb; border-radius: 14px; box-shadow: 0 8px 24px rgba(0,0,0,0.06) }}
        h1 {{ font-size: 22px; margin: 0 0 12px }} code {{ background: #f1f5f9; padding: 2px 6px; border-radius: 4px; word-break: break-all }}
        p {{ line-height: 1.5; color: #4b5563 }}</style></head>
        <body><div class=box><h1>Can't reach this page</h1><p><code>{}</code></p><p>{}</p></div></body></html>",
        crate::dom::escape_text(url),
        crate::dom::escape_text(err)
    )
}

impl Page {
    pub fn host(&self) -> &PageHost {
        self.interp.host::<PageHost>().expect("page host")
    }

    pub fn host_mut(&mut self) -> &mut PageHost {
        self.interp.host_mut::<PageHost>().expect("page host")
    }

    /// Create a page from HTML source. Scripts are not run until `start`.
    pub fn from_html(html: &str, url: Url, opts: &LoadOptions) -> Page {
        let doc = crate::html::parse_document(html);
        let mut host = PageHost::new(doc, url, opts.viewport);
        host.scale = opts.scale;
        host.internal = opts.internal;
        host.scripting = opts.scripting && !opts.internal;
        let mut interp = Interp::new();
        interp.host = Some(Box::new(host));
        interp.module_loader = Some(Rc::new(PageModuleLoader));
        let mut page = Page { interp };
        crate::bindings::install(&mut page.interp);
        page
    }

    /// Fetch and construct a page (blocking).
    pub fn load(url: &Url, opts: &LoadOptions) -> Page {
        Self::load_request(Request::get(url.clone()), opts)
    }

    pub fn load_request(req: Request, opts: &LoadOptions) -> Page {
        let mut log = Vec::new();
        let url = req.url.clone();
        let result = net::fetch_logged(&req, "document", &mut log);
        let mut page = match result {
            Ok(resp) => Self::from_response(&resp, opts),
            Err(e) => {
                let mut p = Self::from_html(&error_page(&url.to_string(), &e), url.clone(), &LoadOptions { scripting: false, ..LoadOptions { viewport: opts.viewport, scale: opts.scale, scripting: false, internal: opts.internal } });
                p.host_mut().log("error", format!("Failed to load {url}: {e}"));
                p
            }
        };
        page.host_mut().net_log.splice(0..0, log);
        page.start();
        page
    }

    pub fn from_response(resp: &net::Response, opts: &LoadOptions) -> Page {
        let url = Url::parse(&resp.url).unwrap_or_else(|| Url::parse("about:blank").unwrap());
        let (mime, charset) = resp.mime();
        let html = if mime.starts_with("image/") {
            format!("<!doctype html><html><head><title>{0}</title><style>body{{margin:0;background:#0e0e10;display:flex;align-items:center;justify-content:center;min-height:100vh}}img{{max-width:100%}}</style></head><body><img src=\"{0}\"></body></html>", crate::dom::escape_attr(&resp.url))
        } else if mime.starts_with("text/plain") || mime == "text/css" || mime.contains("javascript") || mime == "application/json" {
            format!("<!doctype html><html><body><pre style=\"word-wrap:break-word;white-space:pre-wrap\">{}</pre></body></html>", crate::dom::escape_text(&net::decode_text(&resp.body, charset.as_deref())))
        } else {
            // sniff charset from <meta charset>
            let mut cs = charset.clone();
            if cs.is_none() {
                let head = String::from_utf8_lossy(&resp.body[..resp.body.len().min(2048)]).to_ascii_lowercase();
                if let Some(i) = head.find("charset=") {
                    let rest = &head[i + 8..];
                    let v: String = rest.trim_start_matches(['"', '\'']).chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
                    cs = Some(v);
                }
            }
            let text = net::decode_text(&resp.body, cs.as_deref());
            if resp.status >= 400 && text.trim().is_empty() {
                error_page(&resp.url, &format!("The server responded with HTTP {} {}", resp.status, resp.status_text))
            } else {
                text
            }
        };
        Self::from_html(&html, url, opts)
    }

    /// Load subresources and run scripts.
    pub fn start(&mut self) {
        // stylesheets first (render-blocking)
        let _ = self.host_mut().styles();
        if self.host().scripting {
            // parser-blocking classic scripts first, then deferred and module scripts in order
            let scripts: Vec<NodeId> = self.host().doc.elements_by_tag(0, "script");
            let deferred = |h: &PageHost, s: NodeId| {
                let module = h.doc.attr(s, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("module"));
                module || (h.doc.attr(s, "defer").is_some() && h.doc.attr(s, "src").is_some())
            };
            let (later, now): (Vec<NodeId>, Vec<NodeId>) = scripts.into_iter().partition(|&s| deferred(self.host(), s));
            for s in now.into_iter().chain(later) {
                self.run_script_element(s);
            }
        }
        self.host_mut().ready_state = "interactive";
        self.fire_simple(Target::Node(0), "readystatechange", false);
        self.fire_simple(Target::Node(0), "DOMContentLoaded", true);
        self.host_mut().ready_state = "complete";
        self.fire_simple(Target::Node(0), "readystatechange", false);
        self.fire_simple(Target::Window, "load", false);
        self.after_task();
        // scroll to fragment
        if let Some(f) = self.host().url.fragment.clone() {
            self.scroll_to_anchor(&f);
        }
    }

    pub fn run_script_element(&mut self, s: NodeId) {
        let (ty, src, text) = {
            let h = self.host();
            let ty = h.doc.attr(s, "type").unwrap_or("").to_ascii_lowercase();
            (ty, h.doc.attr(s, "src").map(|x| x.to_string()), h.doc.text_content(s))
        };
        if ty.trim() == "module" {
            self.run_module_script(s, src, text);
            return;
        }
        if !(ty.is_empty() || ty.contains("javascript") || ty.contains("ecmascript")) || self.host().doc.attr(s, "nomodule").is_some() {
            return;
        }
        let (code, name) = match src {
            Some(src) => {
                let Some(u) = self.host().resolve_url(&src) else { return };
                let mut log = Vec::new();
                let r = net::fetch_logged(&Request::get(u.clone()), "script", &mut log);
                self.host_mut().net_log.extend(log);
                match r {
                    Ok(resp) if resp.ok() => (resp.text(), u.to_string()),
                    Ok(resp) => {
                        self.host_mut().log("error", format!("Failed to load script {u}: HTTP {}", resp.status));
                        return;
                    }
                    Err(e) => {
                        self.host_mut().log("error", format!("Failed to load script {u}: {e}"));
                        return;
                    }
                }
            }
            None => (text, format!("{} (inline)", self.host().url)),
        };
        self.host_mut().current_script = Some(s);
        let _ = self.eval(&code, &name);
        self.host_mut().current_script = None;
    }

    fn run_module_script(&mut self, s: NodeId, src: Option<String>, text: String) {
        self.host_mut().current_script = Some(s);
        self.interp.deadline = Some(Instant::now() + Duration::from_secs(10));
        let r = match src {
            Some(src) => match self.host().resolve_url(&src) {
                Some(u) => self.interp.import_module(&u.to_string()).map(|_| ()),
                None => Ok(()),
            },
            None => {
                let base = self.host().resolve_url("").map(|u| u.to_string()).unwrap_or_default();
                self.interp.run_inline_module(&base, &text)
            }
        };
        self.interp.deadline = None;
        if let Err(e) = r {
            let msg = self.interp.describe_error(&e);
            let source = format!("{}:{}", self.interp.script_name, self.interp.cur_line + 1);
            self.host_mut().console.push(ConsoleEntry { level: "error", text: format!("Uncaught {msg}"), source });
        }
        self.host_mut().current_script = None;
        self.after_task();
    }

    /// Evaluate script source as a task; errors are reported to the console.
    pub fn eval(&mut self, code: &str, name: &str) -> Result<Value, String> {
        self.interp.deadline = Some(Instant::now() + Duration::from_secs(10));
        let r = self.interp.eval_script(code, name);
        self.interp.deadline = None;
        let out = match r {
            Ok(v) => Ok(v),
            Err(e) => {
                let msg = self.interp.describe_error(&e);
                let line = self.interp.cur_line + 1;
                self.host_mut().console.push(ConsoleEntry { level: "error", text: format!("Uncaught {msg}"), source: format!("{name}:{line}") });
                Err(msg)
            }
        };
        self.after_task();
        out
    }

    /// Run microtasks and collect console output after a task.
    pub fn after_task(&mut self) {
        self.interp.run_microtasks();
        self.notify_mutation_observers();
        let msgs = std::mem::take(&mut self.interp.console);
        let src = self.interp.script_name.clone();
        let h = self.host_mut();
        for m in msgs {
            h.console.push(ConsoleEntry { level: m.level, text: m.text, source: src.clone() });
        }
        if h.console.len() > 5000 {
            let n = h.console.len() - 5000;
            h.console.drain(0..n);
        }
    }

    pub fn call_js(&mut self, f: &Value, this: Value, args: &[Value]) {
        self.interp.deadline = Some(Instant::now() + Duration::from_secs(10));
        let r = self.interp.call(f, this, args);
        self.interp.deadline = None;
        if let Err(e) = r {
            let msg = self.interp.describe_error(&e);
            let source = self.interp.script_name.clone();
            self.host_mut().console.push(ConsoleEntry { level: "error", text: format!("Uncaught {msg}"), source });
        }
    }

    pub fn fire_simple(&mut self, target: Target, ty: &str, bubbles: bool) -> bool {
        crate::bindings::events::dispatch(self, target, ty, crate::bindings::events::EventInit { bubbles, ..Default::default() })
    }

    // ------------------------------------------------------------ timers

    pub fn next_timer_due(&self) -> Option<Instant> {
        self.host().timers.iter().map(|t| t.due).min()
    }

    pub fn has_animation_frames(&self) -> bool {
        !self.host().raf.is_empty()
    }

    /// Run due timers. Returns true if any ran.
    pub fn run_timers(&mut self) -> bool {
        let now = Instant::now();
        let mut ran = false;
        for _ in 0..1000 {
            let next = {
                let h = self.host_mut();
                let idx = h.timers.iter().enumerate().filter(|(_, t)| t.due <= now).min_by_key(|(_, t)| (t.due, t.id)).map(|(i, _)| i);
                match idx {
                    None => None,
                    Some(i) => {
                        let t = &mut h.timers[i];
                        let cb = t.callback.clone();
                        let args = t.args.clone();
                        match t.interval {
                            Some(iv) => {
                                t.due = now.max(t.due) + iv.max(Duration::from_millis(4));
                            }
                            None => {
                                h.timers.remove(i);
                            }
                        }
                        Some((cb, args))
                    }
                }
            };
            let Some((cb, args)) = next else { break };
            ran = true;
            if cb.is_callable() {
                self.call_js(&cb, Value::Undefined, &args);
            } else if let Value::Str(s) = &cb {
                let code = s.to_string();
                let _ = self.eval(&code, "timer");
            }
            self.after_task();
        }
        ran
    }

    pub fn run_animation_frames(&mut self) {
        let cbs = std::mem::take(&mut self.host_mut().raf);
        let t = Value::Num(crate::bindings::window::perf_now());
        for (_, cb) in cbs {
            self.call_js(&cb, Value::Undefined, std::slice::from_ref(&t));
        }
        self.after_task();
    }

    /// Process background work: images, fetches, timers. Returns true if a repaint is needed.
    pub fn poll(&mut self) -> bool {
        let a = self.host_mut().poll_images();
        let f = crate::bindings::window::poll_fetches(&mut self.interp);
        if f {
            self.after_task();
        }
        let b = self.run_timers();
        a || b || f
    }

    pub fn has_pending_work(&self) -> bool {
        let h = self.host();
        h.images_pending() || !h.pending_fetches.is_empty()
    }

    fn notify_mutation_observers(&mut self) {
        if self.host().mutation_observers.is_empty() {
            return;
        }
        for _ in 0..8 {
            let v = self.host().doc.version;
            let due: Vec<(Value, Value, NodeId)> = {
                let h = self.host_mut();
                let mut out = Vec::new();
                for m in h.mutation_observers.iter_mut() {
                    if m.version != v {
                        m.version = v;
                        out.push((m.observer.clone(), m.callback.clone(), m.target));
                    }
                }
                out
            };
            if due.is_empty() {
                break;
            }
            for (obs, cb, target) in due {
                let tv = crate::bindings::wrap(&mut self.interp, target);
                let rec = self.interp.new_object();
                rec.set_prop("type", Value::str("childList"));
                rec.set_prop("target", tv);
                let empty = self.interp.new_array(vec![]);
                rec.set_prop("addedNodes", empty.clone());
                rec.set_prop("removedNodes", empty);
                rec.set_prop("attributeName", Value::Null);
                let recs = self.interp.new_array(vec![Value::Obj(rec)]);
                let _ = self.interp.call(&cb, obs.clone(), &[recs, obs]);
                self.interp.run_microtasks();
            }
        }
    }

    // ------------------------------------------------------------ navigation helpers

    pub fn title(&self) -> String {
        let t = self.host().doc.title();
        if t.is_empty() { self.host().url.to_string() } else { t }
    }

    pub fn url(&self) -> Url {
        self.host().url.clone()
    }

    pub fn scroll_to_anchor(&mut self, frag: &str) {
        let frag = net::url::percent_decode(frag);
        let target = {
            let h = self.host();
            h.doc.get_element_by_id(&frag).or_else(|| h.doc.elements_by_tag(0, "a").into_iter().find(|&a| h.doc.attr(a, "name") == Some(frag.as_str())))
        };
        if let Some(t) = target {
            if let Some(r) = self.host_mut().node_rect(t) {
                self.host_mut().scroll.1 = r.y;
                self.host_mut().clamp_scroll();
            }
        } else if frag.is_empty() || frag == "top" {
            self.host_mut().scroll = (0.0, 0.0);
        }
    }

    pub fn set_viewport(&mut self, w: f32, h: f32, scale: f32) {
        let host = self.host_mut();
        if host.viewport != (w, h) || host.scale != scale {
            host.viewport = (w, h);
            host.scale = scale;
            host.clamp_scroll();
            self.fire_simple(Target::Window, "resize", false);
        }
    }
}
