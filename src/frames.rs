//! `<iframe>`: nested browsing contexts.
//!
//! Each iframe owns a child [`Page`] with its own JavaScript interpreter. The parent paints
//! the child's display list into the iframe's content box, routes input to it, and drives
//! its loading, timers and navigation from its own event loop.
//!
//! Windows in different frames never share JavaScript objects. Scripts reach other windows
//! only through `WindowProxy` objects (`iframe.contentWindow`, `window.parent`, `window.top`,
//! `window.frames[i]`) that support `postMessage`. Messages are copied with a structured
//! clone, as between cross-origin windows in other browsers. `contentDocument` is `null`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};

use crate::bindings::events::{self, EventInit};
use crate::dom::NodeId;
use crate::js::interp::{arg, Interp};
use crate::js::value::{Obj, ObjKind, PropKey, TypedTy, Value};
use crate::net::{Request, Url};
use crate::page::{LoadOptions, NavRequest, Page, PageHost, Target};
use crate::tab::{fetch_document, Loaded};

/// Frames nested deeper than this are not loaded (guards against recursive embedding).
const MAX_DEPTH: usize = 8;
/// `window[i]` is maintained for this many frames.
const MAX_INDEXED: usize = 64;

/// A value copied between windows (the structured clone algorithm's data model).
#[derive(Debug, Clone)]
pub enum Cloned {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    BigInt(i128),
    Str(String),
    Array(Vec<Cloned>),
    Object(Vec<(String, Cloned)>),
    Date(f64),
    Map(Vec<(Cloned, Cloned)>),
    Set(Vec<Cloned>),
    ArrayBuffer(Vec<u8>),
    Typed(TypedTy, Vec<u8>),
    Error(String, String),
}

/// Which window sent a message, from the receiver's point of view.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Parent,
    Top,
    Frame(NodeId),
    /// Another frame of the same parent, by its key (iframe name, or "#index").
    Sibling(String),
}

/// A message on its way to a window.
#[derive(Debug, Clone)]
pub struct Message {
    pub data: Cloned,
    pub origin: String,
    pub source: Source,
    /// Transferred MessagePorts, as (own mailbox id, peer's mailbox id).
    pub ports: Vec<(u64, u64)>,
}

/// A message posted by a child frame to its parent (or to the top window).
#[derive(Debug, Clone)]
pub struct UpMessage {
    pub data: Cloned,
    pub origin: String,
    /// `targetOrigin` given by the sender ("*" for any).
    pub target_origin: String,
    pub to_top: bool,
    pub ports: Vec<(u64, u64)>,
    /// For a sibling frame (`parent.frames[name]`): its key; the parent routes it.
    pub sibling: Option<String>,
}

/// What a child page knows about the frame it lives in.
pub struct ParentLink {
    pub outbox: Rc<RefCell<Vec<UpMessage>>>,
    pub parent_origin: String,
    pub depth: usize,
    /// Names of the parent's frames in order ("" when unnamed), for `parent.frames[...]`.
    pub siblings: Rc<RefCell<Vec<String>>>,
}

/// One `<iframe>` element's browsing context.
pub struct Frame {
    pub node: NodeId,
    /// What the frame was asked to show (src URL, srcdoc hash, ...); a change reloads it.
    pub key: String,
    pub page: Option<Page>,
    pending: Option<(Url, Receiver<Loaded>)>,
    outbox: Rc<RefCell<Vec<UpMessage>>>,
    /// Content box size the child page is laid out at.
    pub viewport: (f32, f32),
}

/// Proxy id for the parent and top windows (frames use their element's node id).
const PROXY_PARENT: i64 = -1;
const PROXY_TOP: i64 = -2;

/// The origin a page's scripts run with. `about:blank`, `about:srcdoc` and `data:` frames
/// inherit their parent's origin here (Chrome gives `data:` an opaque origin, but scripts
/// that post to such frames usually pass "*" anyway).
pub fn effective_origin(h: &PageHost) -> String {
    match (h.url.scheme.as_str(), &h.parent) {
        ("about" | "data", Some(p)) => p.parent_origin.clone(),
        _ => h.url.origin(),
    }
}

/// Size of an iframe's content box from the parent's layout.
fn content_size(h: &mut PageHost, node: NodeId) -> Option<(f32, f32)> {
    let l = h.layout();
    let b = l.find_box(node)?;
    Some((b.content.w.max(0.0).round(), b.content.h.max(0.0).round()))
}

fn frame_key(h: &PageHost, node: NodeId) -> String {
    if let Some(sd) = h.doc.attr(node, "srcdoc") {
        return format!("srcdoc:{}", crate::page::hash_str(sd));
    }
    match h.doc.attr(node, "src").map(str::trim).filter(|s| !s.is_empty()) {
        Some(src) => h.resolve_url(src).map(|u| u.to_string()).unwrap_or_else(|| "about:blank".into()),
        None => "about:blank".into(),
    }
}

/// X-Frame-Options and CSP `frame-ancestors` (only the common forms). Returns false when the
/// response forbids being framed by `parent_origin`.
fn framing_allowed(resp: &crate::net::Response, parent_origin: &str) -> bool {
    let own = Url::parse(&resp.url).map(|u| u.origin()).unwrap_or_default();
    for (k, v) in &resp.headers {
        let k = k.to_ascii_lowercase();
        let v = v.to_ascii_lowercase();
        if k == "x-frame-options" {
            let v = v.split(',').next().unwrap_or("").trim();
            if v == "deny" || (v == "sameorigin" && own != parent_origin) {
                return false;
            }
        }
        if k == "content-security-policy" {
            // several policies may be joined with commas (repeated headers merged by the OS)
            for directive in v.split([';', ',']) {
                let mut parts = directive.split_whitespace();
                if parts.next() != Some("frame-ancestors") {
                    continue;
                }
                let sources: Vec<&str> = parts.collect();
                let ok = sources.iter().any(|s| match *s {
                    "*" => true,
                    "'self'" => own == parent_origin,
                    "'none'" => false,
                    s if s.starts_with("https:") && s.len() == 6 => parent_origin.starts_with("https:"),
                    s => source_matches(s, parent_origin),
                });
                if !ok {
                    return false;
                }
            }
        }
    }
    true
}

/// A CSP host source (`https://*.example.com`, `example.com:8443`, `*.example.com`) against
/// an origin like `https://www.example.com`.
fn source_matches(src: &str, origin: &str) -> bool {
    let (o_scheme, o_rest) = origin.split_once("://").unwrap_or(("", origin));
    let (s_scheme, s_rest) = match src.split_once("://") {
        Some((a, b)) => (Some(a), b),
        None => (None, src),
    };
    if s_scheme.is_some_and(|sc| sc != o_scheme) {
        return false;
    }
    let s_rest = s_rest.trim_end_matches('/');
    let (s_host, s_port) = match s_rest.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit() || c == '*') => (h, Some(p)),
        _ => (s_rest, None),
    };
    let (o_host, o_port) = match o_rest.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h, Some(p)),
        _ => (o_rest, None),
    };
    if let Some(p) = s_port {
        if p != "*" && Some(p) != o_port {
            return false;
        }
    }
    match s_host.strip_prefix("*.") {
        Some(suffix) => o_host.ends_with(&format!(".{suffix}")),
        None => s_host == o_host,
    }
}

fn blocked_page(url: &str) -> String {
    format!(
        "<!doctype html><html><body style=\"margin:0;font:13px sans-serif;color:#6b7280;background:#f3f4f6;display:flex;align-items:center;justify-content:center;height:100vh;text-align:center\"><div><b style=\"color:#374151\">{}</b><br>refused to be shown in a frame.</div></body></html>",
        crate::dom::escape_text(&Url::parse(url).map(|u| u.host).unwrap_or_default())
    )
}

impl Page {
    /// Create, reload or drop child frames to match the document's `<iframe>` elements.
    pub fn sync_frames(&mut self) {
        let depth = self.host().parent.as_ref().map(|p| p.depth).unwrap_or(0);
        let v = self.host().doc.version;
        if self.host().frames_version == v {
            return;
        }
        self.host_mut().frames_version = v;
        if depth >= MAX_DEPTH || self.host().internal {
            return;
        }
        let wanted: Vec<(NodeId, String)> = {
            let h = self.host();
            h.doc.elements_by_tag(0, "iframe").into_iter().map(|n| (n, frame_key(h, n))).collect()
        };
        // drop frames whose element left the document
        self.host_mut().frames.retain(|f| wanted.iter().any(|(n, _)| *n == f.node));
        for (node, key) in wanted {
            let existing = self.host().frames.iter().position(|f| f.node == node);
            match existing {
                Some(i) if self.host().frames[i].key == key => {}
                Some(i) => {
                    self.host_mut().frames[i].key = key.clone();
                    self.start_frame_load(node, &key);
                }
                None => {
                    let size = content_size(self.host_mut(), node).unwrap_or((300.0, 150.0));
                    self.host_mut().frames.push(Frame { node, key: key.clone(), page: None, pending: None, outbox: Rc::new(RefCell::new(Vec::new())), viewport: size });
                    self.start_frame_load(node, &key);
                }
            }
        }
        // the frames' names, which children see through `parent.frames[...]`
        let names: Vec<String> = {
            let h = self.host();
            h.frames.iter().map(|f| h.doc.attr(f.node, "name").unwrap_or("").to_string()).collect()
        };
        *self.host().frame_names.borrow_mut() = names;
        // `window[i]` / `frames[i]`
        let g = self.interp.global.clone();
        let wins = child_windows(&mut self.interp);
        for i in 0..MAX_INDEXED {
            let k = PropKey::from(i.to_string().as_str());
            match wins.get(i) {
                Some(w) => g.set_hidden(&i.to_string(), w.clone()),
                None => {
                    if g.borrow().props.get(&k).is_none() {
                        break;
                    }
                    g.borrow_mut().props.remove(&k);
                }
            }
        }
    }

    fn frame_index(&self, node: NodeId) -> Option<usize> {
        self.host().frames.iter().position(|f| f.node == node)
    }

    /// Begin loading `key` (a URL, "about:blank" or a srcdoc marker) into the frame.
    fn start_frame_load(&mut self, node: NodeId, key: &str) {
        let Some(i) = self.frame_index(node) else { return };
        self.host_mut().frames[i].pending = None;
        if key.starts_with("srcdoc:") {
            let html = self.host().doc.attr(node, "srcdoc").unwrap_or("").to_string();
            let base = self.host().url.clone();
            let mut url = Url::parse("about:srcdoc").unwrap();
            url.fragment = None;
            let page = self.new_frame_page(node, &html, url, Some(base));
            self.install_frame(node, page);
            return;
        }
        match Url::parse(key) {
            Some(u) if matches!(u.scheme.as_str(), "http" | "https" | "file" | "data") => {
                let (tx, rx) = channel();
                let mut req = Request::get(u.clone());
                req.headers.push(("Referer".into(), self.host().url.without_fragment().to_string()));
                std::thread::spawn(move || {
                    let _ = tx.send(fetch_document(req));
                });
                self.host_mut().frames[i].pending = Some((u, rx));
            }
            _ => {
                let page = self.new_frame_page(node, "", Url::parse("about:blank").unwrap(), None);
                self.install_frame(node, page);
            }
        }
    }

    /// A child page for a frame, with the frame's `window.name`, `parent` and `top` set up.
    fn new_frame_page(&mut self, node: NodeId, html: &str, url: Url, base: Option<Url>) -> Page {
        let opts = self.frame_opts(node);
        let mut page = Page::from_html(html, url, &opts);
        if let Some(b) = base {
            page.host_mut().base_override = Some(b);
        }
        self.link_child(node, &mut page);
        page
    }

    fn frame_opts(&self, node: NodeId) -> LoadOptions {
        let h = self.host();
        let vp = h.frames.iter().find(|f| f.node == node).map(|f| f.viewport).unwrap_or((300.0, 150.0));
        LoadOptions { viewport: vp, scale: h.scale, scripting: h.scripting, internal: false }
    }

    fn link_child(&mut self, node: NodeId, page: &mut Page) {
        let depth = self.host().parent.as_ref().map(|p| p.depth).unwrap_or(0) + 1;
        let origin = effective_origin(self.host());
        let Some(i) = self.frame_index(node) else { return };
        let outbox = self.host().frames[i].outbox.clone();
        let name = self.host().doc.attr(node, "name").unwrap_or("").to_string();
        let siblings = self.host().frame_names.clone();
        page.host_mut().parent = Some(ParentLink { outbox, parent_origin: origin, depth, siblings });
        install_child_globals(&mut page.interp, &name);
    }

    /// Put a loaded page into its frame, run it, and fire `load` on the iframe element.
    fn install_frame(&mut self, node: NodeId, mut page: Page) {
        page.start();
        let Some(i) = self.frame_index(node) else { return };
        self.host_mut().frames[i].page = Some(page);
        self.host_mut().invalidate();
        self.fire_simple(Target::Node(node), "load", false);
        self.after_task();
    }

    fn finish_frame_load(&mut self, node: NodeId, url: Url, loaded: Loaded) {
        let opts = self.frame_opts(node);
        let parent_origin = effective_origin(self.host());
        let mut page = match loaded.result {
            Ok(resp) if framing_allowed(&resp, &parent_origin) => Page::from_response(&resp, &opts),
            Ok(resp) => {
                self.host_mut().log("error", format!("Refused to display '{}' in a frame because it set 'X-Frame-Options' or 'frame-ancestors'.", resp.url));
                Page::from_html(&blocked_page(&resp.url), url.clone(), &LoadOptions { scripting: false, ..opts })
            }
            Err(e) => Page::from_html(&crate::page::error_page(&url.to_string(), &e), url.clone(), &LoadOptions { scripting: false, ..opts }),
        };
        page.host_mut().net_log.splice(0..0, loaded.log);
        self.link_child(node, &mut page);
        self.install_frame(node, page);
    }

    /// Drive child frames: finish loads, run their event loops, follow their navigations and
    /// deliver messages. Returns true if anything changed.
    pub fn poll_frames(&mut self) -> bool {
        self.sync_frames();
        if self.host().frames.is_empty() && self.host().inbox.is_empty() {
            return self.deliver_port_messages();
        }
        let mut changed = false;
        // keep child viewports in step with the iframes' boxes
        let nodes: Vec<NodeId> = self.host().frames.iter().map(|f| f.node).collect();
        for &node in &nodes {
            if let Some(size) = content_size(self.host_mut(), node) {
                let scale = self.host().scale;
                let Some(i) = self.frame_index(node) else { continue };
                let f = &mut self.host_mut().frames[i];
                if f.viewport != size {
                    f.viewport = size;
                    if let Some(p) = &mut f.page {
                        p.set_viewport(size.0, size.1, scale);
                        p.after_task();
                    }
                    changed = true;
                }
            }
        }
        for node in nodes {
            let Some(i) = self.frame_index(node) else { continue };
            // finished loads
            let done = match &self.host().frames[i].pending {
                Some((_, rx)) => match rx.try_recv() {
                    Ok(l) => Some(Some(l)),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                    Err(_) => Some(None),
                },
                None => None,
            };
            if let Some(res) = done {
                let url = self.host_mut().frames[i].pending.take().map(|p| p.0);
                if let (Some(l), Some(u)) = (res, url) {
                    self.finish_frame_load(node, u, l);
                }
                changed = true;
            }
            let Some(i) = self.frame_index(node) else { continue };
            // the child's own event loop, and navigation it asked for
            let mut navs = Vec::new();
            let mut logs = Vec::new();
            if let Some(p) = &mut self.host_mut().frames[i].page {
                changed |= p.poll();
                // frames' console output shows up in the page's console, labelled with the frame
                logs = std::mem::take(&mut p.host_mut().console);
                let label = p.host().url.to_string();
                for l in &mut logs {
                    l.source = if l.source.is_empty() { label.clone() } else { l.source.clone() };
                }
                for _ in 0..4 {
                    match p.take_navigation() {
                        Some(n) => navs.push(n),
                        None => break,
                    }
                }
            }
            self.host_mut().console.extend(logs);
            for nav in navs {
                changed = true;
                match nav {
                    NavRequest::Load { url, method, body, headers, .. } => {
                        if method.eq_ignore_ascii_case("GET") && body.is_none() {
                            self.navigate_frame(node, url);
                        } else {
                            let (tx, rx) = channel();
                            let req = Request { method, url: url.clone(), headers, body };
                            std::thread::spawn(move || {
                                let _ = tx.send(fetch_document(req));
                            });
                            if let Some(i) = self.frame_index(node) {
                                self.host_mut().frames[i].pending = Some((url, rx));
                            }
                        }
                    }
                    NavRequest::Reload => {
                        let key = self.host().frames.iter().find(|f| f.node == node).map(|f| f.key.clone()).unwrap_or_default();
                        self.start_frame_load(node, &key);
                    }
                    NavRequest::Open(url) => self.host_mut().nav = Some(NavRequest::Open(url)),
                    NavRequest::Up { nav, top } => {
                        // _parent lands here; _top keeps going while there is a window above
                        self.host_mut().nav = Some(if top && self.host().parent.is_some() { NavRequest::Up { nav, top } } else { *nav });
                    }
                    NavRequest::History(_) | NavRequest::SameDocument { .. } => {}
                }
            }
            // messages the child posted upwards
            let Some(i) = self.frame_index(node) else { continue };
            let ups: Vec<UpMessage> = self.host().frames[i].outbox.borrow_mut().drain(..).collect();
            for m in ups {
                trace(&format!("drained up (to_top={}, target={}) in", m.to_top, m.target_origin), &self.host().url.to_string().chars().take(60).collect::<String>(), &m.data, m.ports.len());
                changed = true;
                let forward_up = m.to_top && self.host().parent.is_some();
                if forward_up {
                    if let Some(p) = &self.host().parent {
                        p.outbox.borrow_mut().push(m);
                    }
                    continue;
                }
                if let Some(key) = &m.sibling {
                    // to another of our frames: hand it over, from the sender's sibling proxy
                    let target = self.frame_by_key(key);
                    let from = self.frame_key_of(node);
                    if let Some(t) = target {
                        if let Some(child) = self.host_mut().frames.iter_mut().find(|f| f.node == t).and_then(|f| f.page.as_mut()) {
                            if origin_allows(&m.target_origin, &effective_origin(child.host())) {
                                child.host_mut().inbox.push(Message { data: m.data, origin: m.origin, source: Source::Sibling(from), ports: m.ports });
                            }
                        }
                    }
                    continue;
                }
                let mine = effective_origin(self.host());
                if !origin_allows(&m.target_origin, &mine) {
                    continue;
                }
                self.host_mut().inbox.push(Message { data: m.data, origin: m.origin, source: Source::Frame(node), ports: m.ports });
            }
        }
        changed |= self.deliver_messages();
        changed |= self.deliver_port_messages();
        changed
    }

    /// Our frame for a sibling key: "#3" (index) or an iframe name.
    fn frame_by_key(&self, key: &str) -> Option<NodeId> {
        let h = self.host();
        match key.strip_prefix('#').and_then(|i| i.parse::<usize>().ok()) {
            Some(i) => h.frames.get(i).map(|f| f.node),
            None => h.frames.iter().find(|f| h.doc.attr(f.node, "name") == Some(key)).map(|f| f.node),
        }
    }

    /// The key other frames use for one of ours: its name, or "#index".
    fn frame_key_of(&self, node: NodeId) -> String {
        let h = self.host();
        match h.doc.attr(node, "name").filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            None => format!("#{}", h.frames.iter().position(|f| f.node == node).unwrap_or(0)),
        }
    }

    /// Navigate a frame (from a link or script inside it, or `location` on its proxy).
    pub fn navigate_frame(&mut self, node: NodeId, url: Url) {
        let key = url.to_string();
        if let Some(i) = self.frame_index(node) {
            self.host_mut().frames[i].key = key.clone();
        }
        self.start_frame_load(node, &key);
    }

    /// Fire queued `message` events on this window.
    pub fn deliver_messages(&mut self) -> bool {
        let msgs = std::mem::take(&mut self.host_mut().inbox);
        if msgs.is_empty() {
            return false;
        }
        for m in msgs {
            trace(&format!("deliver window message ({} ports) in", m.ports.len()), &self.host().url.to_string().chars().take(60).collect::<String>(), &m.data, m.ports.len());
            let data = import(&mut self.interp, &m.data);
            let source = match m.source {
                Source::Parent => proxy(&mut self.interp, PROXY_PARENT),
                Source::Top => proxy(&mut self.interp, PROXY_TOP),
                Source::Frame(n) => proxy(&mut self.interp, n as i64),
                Source::Sibling(k) => sibling_id(&mut self.interp, &k),
            };
            let port_objs: Vec<Value> = m.ports.iter().map(|&(id, peer)| adopt_port(&mut self.interp, id, peer)).collect();
            let ports = self.interp.new_array(port_objs);
            let init = EventInit { bubbles: false, cancelable: false, props: vec![("data", data), ("origin", Value::str(&m.origin)), ("source", source), ("lastEventId", Value::str("")), ("ports", ports)] };
            events::fire(&mut self.interp, Target::Window, "message", "MessageEvent", init);
            self.after_task();
        }
        true
    }

    /// The iframe under a viewport point and the point in the frame's own coordinates.
    pub fn frame_at(&mut self, x: f32, y: f32) -> Option<(NodeId, f32, f32)> {
        if self.host().frames.is_empty() {
            return None;
        }
        let sc = self.host().scroll;
        let dl = self.host_mut().display_list();
        let hit = dl.hit_test(x + sc.0, y + sc.1, sc)?;
        let r = dl.frames.iter().rev().find(|f| f.node == hit)?;
        let (px, py) = if r.fixed { (x, y) } else { (x + sc.0, y + sc.1) };
        if !r.rect.contains(px, py) {
            return None;
        }
        self.frame_index(hit)?;
        Some((hit, px - r.rect.x, py - r.rect.y))
    }

    /// A viewport point in a frame's coordinates (also outside it, for drags).
    pub fn frame_point(&mut self, node: NodeId, x: f32, y: f32) -> Option<(f32, f32)> {
        let sc = self.host().scroll;
        let dl = self.host_mut().display_list();
        let r = dl.frames.iter().rev().find(|f| f.node == node)?;
        let (px, py) = if r.fixed { (x, y) } else { (x + sc.0, y + sc.1) };
        Some((px - r.rect.x, py - r.rect.y))
    }

    /// Route a pointer move to frames. Returns true if a frame took it.
    pub fn frame_mouse_move(&mut self, x: f32, y: f32) -> bool {
        if let Some(node) = self.host().frame_capture {
            if let Some((lx, ly)) = self.frame_point(node, x, y) {
                let cursor = self.with_frame(node, |p| {
                    p.mouse_move(lx, ly);
                    p.cursor()
                });
                if let Some(c) = cursor {
                    self.host_mut().cursor = c;
                }
                return true;
            }
        }
        let hit = self.frame_at(x, y);
        let prev = self.host().frame_hover;
        if prev.is_some() && prev != hit.map(|h| h.0) {
            // the pointer left a frame: clear its hover state
            self.with_frame(prev.unwrap(), |p| p.mouse_move(-1.0e6, -1.0e6));
            self.host_mut().frame_hover = None;
        }
        let Some((node, lx, ly)) = hit else { return false };
        self.host_mut().frame_hover = Some(node);
        let cursor = self.with_frame(node, |p| {
            p.mouse_move(lx, ly);
            p.cursor()
        });
        if let Some(c) = cursor {
            self.host_mut().cursor = c;
        }
        true
    }

    /// Route a press to the frame under the pointer (focusing its iframe element).
    pub fn frame_mouse_down(&mut self, x: f32, y: f32, button: i32, mods: crate::bindings::events::Mods, count: i32) -> bool {
        let Some((node, lx, ly)) = self.frame_at(x, y) else { return false };
        if self.host().focus != Some(node) {
            crate::input::focus(&mut self.interp, Some(node));
        }
        self.host_mut().frame_capture = Some(node);
        self.with_frame(node, |p| p.mouse_down_n(lx, ly, button, mods, count));
        true
    }

    pub fn frame_mouse_up(&mut self, x: f32, y: f32, button: i32, mods: crate::bindings::events::Mods, count: i32) -> bool {
        let Some(node) = self.host_mut().frame_capture.take() else { return false };
        let Some((lx, ly)) = self.frame_point(node, x, y) else { return true };
        self.with_frame(node, |p| p.mouse_up(lx, ly, button, mods, count));
        true
    }

    /// Wheel over a frame scrolls it; returns Some(scrolled) when a frame was under the pointer.
    pub fn frame_wheel(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> Option<bool> {
        let (node, lx, ly) = self.frame_at(x, y)?;
        self.with_frame(node, |p| p.wheel(lx, ly, dx, dy))
    }

    /// Run `f` on a frame's page, if it has loaded.
    pub fn with_frame<T>(&mut self, node: NodeId, f: impl FnOnce(&mut Page) -> T) -> Option<T> {
        let i = self.frame_index(node)?;
        let page = self.host_mut().frames[i].page.as_mut()?;
        Some(f(page))
    }

    /// The frame whose document has keyboard focus (its iframe element is focused here).
    pub fn focused_frame(&self) -> Option<NodeId> {
        let f = self.host().focus?;
        self.host().frames.iter().any(|fr| fr.node == f && fr.page.is_some()).then_some(f)
    }

    pub fn frames_have_pending_work(&self) -> bool {
        self.host().frames.iter().any(|f| f.pending.is_some() || f.page.as_ref().is_some_and(|p| p.has_pending_work()))
    }

    pub fn frames_next_timer_due(&self) -> Option<std::time::Instant> {
        self.host().frames.iter().filter_map(|f| f.page.as_ref().and_then(|p| p.next_timer_due())).min()
    }

    pub fn frames_have_animation_frames(&self) -> bool {
        self.host().frames.iter().any(|f| f.page.as_ref().is_some_and(|p| p.has_animation_frames()))
    }

    pub fn run_frame_animation_frames(&mut self) {
        let n = self.host().frames.len();
        for i in 0..n {
            if let Some(p) = &mut self.host_mut().frames[i].page {
                if p.has_animation_frames() {
                    p.run_animation_frames();
                }
            }
        }
    }
}

impl PageHost {
    /// Identity of every frame's current picture, folded into this page's display-list key.
    pub fn frames_paint_key(&mut self) -> u64 {
        let mut k = 0u64;
        for f in &mut self.frames {
            if let Some(p) = &mut f.page {
                let dl = p.host_mut().display_list();
                let sc = p.host().scroll;
                k = k.wrapping_mul(31).wrapping_add(Rc::as_ptr(&dl) as u64).wrapping_add(sc.0.to_bits() as u64 ^ ((sc.1.to_bits() as u64) << 32));
            }
        }
        k
    }

    /// Images still loading here or in any frame.
    pub fn images_pending_deep(&self) -> bool {
        self.images_pending() || self.frames.iter().any(|f| f.page.as_ref().is_some_and(|p| p.host().images_pending_deep()))
    }

    /// Integrate finished image loads here and in frames.
    pub fn poll_images_deep(&mut self) -> bool {
        let mut changed = self.poll_images();
        for f in &mut self.frames {
            if let Some(p) = &mut f.page {
                changed |= p.host_mut().poll_images_deep();
            }
        }
        changed
    }

    /// A frame's display list and scroll position, for painting it into its iframe.
    pub fn frame_picture(&self, node: NodeId) -> Option<(Rc<crate::paint::DisplayList>, (f32, f32))> {
        let f = self.frames.iter().find(|f| f.node == node)?;
        let p = f.page.as_ref()?;
        let h = p.host();
        let (_, dl) = h.display.as_ref()?;
        Some((dl.clone(), h.scroll))
    }
}

// ---------------------------------------------------------------- structured clone

/// Copy a JS value out of one interpreter (the structured clone algorithm, minus transfer).
pub fn export(it: &mut Interp, v: &Value) -> Result<Cloned, Value> {
    let mut seen = Vec::new();
    export_inner(it, v, &mut seen, 0)
}

fn export_inner(it: &mut Interp, v: &Value, seen: &mut Vec<*const ()>, depth: usize) -> Result<Cloned, Value> {
    if depth > 200 {
        return Err(it.make_error("DataCloneError", "Value is too deeply nested to be cloned."));
    }
    Ok(match v {
        Value::Undefined => Cloned::Undefined,
        Value::Null => Cloned::Null,
        Value::Bool(b) => Cloned::Bool(*b),
        Value::Num(n) => Cloned::Num(*n),
        Value::BigInt(b) => Cloned::BigInt(*b),
        Value::Str(s) => Cloned::Str(s.to_string()),
        Value::Sym(_) => return Err(it.make_error("DataCloneError", "Symbol could not be cloned.")),
        Value::Obj(o) => {
            let ptr = o.id() as *const ();
            if seen.contains(&ptr) {
                // cycles are not preserved; break them
                return Ok(Cloned::Null);
            }
            seen.push(ptr);
            let kind = {
                let b = o.borrow();
                match &b.kind {
                    ObjKind::Function(_) => Err("function"),
                    ObjKind::Array(items) => Ok(Some(Err(items.clone()))),
                    ObjKind::Date(t) => Ok(Some(Ok(Cloned::Date(*t)))),
                    ObjKind::Bool(x) => Ok(Some(Ok(Cloned::Bool(*x)))),
                    ObjKind::Num(x) => Ok(Some(Ok(Cloned::Num(*x)))),
                    ObjKind::Str(x) => Ok(Some(Ok(Cloned::Str(x.to_string())))),
                    ObjKind::ArrayBuffer(buf) => Ok(Some(Ok(Cloned::ArrayBuffer(buf.borrow().clone())))),
                    ObjKind::TypedArray(t) => {
                        let bytes = t.buf.borrow();
                        let size = t.ty.size();
                        let end = (t.offset + t.len * size).min(bytes.len());
                        Ok(Some(Ok(Cloned::Typed(t.ty, bytes[t.offset.min(end)..end].to_vec()))))
                    }
                    ObjKind::Map(_) | ObjKind::Set(_) => Ok(None),
                    ObjKind::Error => Ok(None),
                    ObjKind::Host(_) | ObjKind::Promise(_) | ObjKind::Generator(_) | ObjKind::Proxy(_) | ObjKind::Symbol(_) => Err("object"),
                    _ => Ok(None),
                }
            };
            let out = match kind {
                Err(what) => {
                    seen.pop();
                    return Err(it.make_error("DataCloneError", &format!("{} could not be cloned.", if what == "function" { "A function" } else { "This object" })));
                }
                Ok(Some(Ok(c))) => c,
                Ok(Some(Err(items))) => {
                    let mut out = Vec::with_capacity(items.len());
                    for x in &items {
                        out.push(export_inner(it, x, seen, depth + 1)?);
                    }
                    Cloned::Array(out)
                }
                Ok(None) => {
                    let (is_map, is_set, is_err) = {
                        let b = o.borrow();
                        (matches!(b.kind, ObjKind::Map(_)), matches!(b.kind, ObjKind::Set(_)), matches!(b.kind, ObjKind::Error))
                    };
                    if is_map || is_set {
                        let entries: Vec<(Value, Value)> = {
                            let b = o.borrow();
                            match &b.kind {
                                ObjKind::Map(m) | ObjKind::Set(m) => m.entries.iter().flatten().map(|(k, v)| (k.clone(), v.clone())).collect(),
                                _ => vec![],
                            }
                        };
                        if is_map {
                            let mut out = Vec::new();
                            for (k, v) in &entries {
                                out.push((export_inner(it, k, seen, depth + 1)?, export_inner(it, v, seen, depth + 1)?));
                            }
                            Cloned::Map(out)
                        } else {
                            let mut out = Vec::new();
                            for (k, _) in &entries {
                                out.push(export_inner(it, k, seen, depth + 1)?);
                            }
                            Cloned::Set(out)
                        }
                    } else if is_err {
                        let name = it.get_str(v, "name").map(|n| it.to_string_lossy(&n)).unwrap_or_else(|_| "Error".into());
                        let msg = it.get_str(v, "message").map(|n| it.to_string_lossy(&n)).unwrap_or_default();
                        Cloned::Error(name, msg)
                    } else {
                        let keys = it.own_keys(o, false);
                        let mut out = Vec::with_capacity(keys.len());
                        for k in keys {
                            let PropKey::Str(name) = &k else { continue };
                            let enumerable = o.borrow().props.get(&k).map(|p| p.enumerable).unwrap_or(true);
                            if !enumerable {
                                continue;
                            }
                            let val = it.get(v, &k)?;
                            out.push((name.to_string(), export_inner(it, &val, seen, depth + 1)?));
                        }
                        Cloned::Object(out)
                    }
                }
            };
            seen.pop();
            out
        }
    })
}

/// Rebuild a cloned value inside an interpreter.
pub fn import(it: &mut Interp, c: &Cloned) -> Value {
    let construct = |it: &mut Interp, ctor: &str, args: &[Value]| -> Value {
        let g = Value::Obj(it.global.clone());
        match it.get_str(&g, ctor) {
            Ok(f) => it.construct(&f, args, None).unwrap_or(Value::Undefined),
            Err(_) => Value::Undefined,
        }
    };
    match c {
        Cloned::Undefined => Value::Undefined,
        Cloned::Null => Value::Null,
        Cloned::Bool(b) => Value::Bool(*b),
        Cloned::Num(n) => Value::Num(*n),
        Cloned::BigInt(b) => Value::BigInt(*b),
        Cloned::Str(s) => Value::str(s),
        Cloned::Array(items) => {
            let vals = items.iter().map(|x| import(it, x)).collect();
            it.new_array(vals)
        }
        Cloned::Object(props) => {
            let o = it.new_object();
            for (k, v) in props {
                let val = import(it, v);
                o.set_prop(k, val);
            }
            Value::Obj(o)
        }
        Cloned::Date(t) => construct(it, "Date", &[Value::Num(*t)]),
        Cloned::Map(entries) => {
            let m = construct(it, "Map", &[]);
            if let Ok(set) = it.get_str(&m, "set") {
                for (k, v) in entries {
                    let (k, v) = (import(it, k), import(it, v));
                    let _ = it.call(&set, m.clone(), &[k, v]);
                }
            }
            m
        }
        Cloned::Set(items) => {
            let s = construct(it, "Set", &[]);
            if let Ok(add) = it.get_str(&s, "add") {
                for x in items {
                    let x = import(it, x);
                    let _ = it.call(&add, s.clone(), &[x]);
                }
            }
            s
        }
        Cloned::ArrayBuffer(bytes) => {
            let b = construct(it, "ArrayBuffer", &[Value::Num(bytes.len() as f64)]);
            if let Value::Obj(o) = &b {
                if let ObjKind::ArrayBuffer(buf) = &o.borrow().kind {
                    *buf.borrow_mut() = bytes.clone();
                }
            }
            b
        }
        Cloned::Typed(ty, bytes) => {
            let buf = import(it, &Cloned::ArrayBuffer(bytes.clone()));
            construct(it, ty.name(), &[buf])
        }
        Cloned::Error(name, msg) => it.make_error(name, msg),
    }
}

// ---------------------------------------------------------------- WindowProxy

fn host(it: &mut Interp) -> &mut PageHost {
    it.host_mut::<PageHost>().expect("page host")
}

/// Proxy ids live in a hidden property; `window.parent` etc. are cached per interpreter so
/// identity comparisons (`event.source === iframe.contentWindow`) hold.
pub fn proxy(it: &mut Interp, id: i64) -> Value {
    // the top window, seen from a direct child, is its parent
    let id = if id == PROXY_TOP && host(it).parent.as_ref().is_some_and(|p| p.depth <= 1) { PROXY_PARENT } else { id };
    if id == PROXY_TOP && host(it).parent.is_none() || id == PROXY_PARENT && host(it).parent.is_none() {
        return Value::Obj(it.global.clone());
    }
    if let Some(o) = host(it).frame_proxies.get(&id) {
        return Value::Obj(o.clone());
    }
    let o = Obj::new(ObjKind::Ordinary, Some(it.p.object.clone()), "Window");
    let ov = Value::Obj(o.clone());
    o.set_hidden("__tipWindowId", Value::Num(id as f64));
    o.set_hidden("window", ov.clone());
    o.set_hidden("self", ov.clone());
    o.set_hidden("frames", ov.clone());
    o.set_hidden("closed", Value::Bool(false));
    o.set_hidden("length", Value::Num(0.0));
    let me = Value::Obj(it.global.clone());
    if id >= 0 {
        // a child frame: its parent is this window, its top is ours
        o.set_hidden("parent", me);
        let top = proxy(it, PROXY_TOP);
        o.set_hidden("top", top);
    } else if id <= SIBLING_BASE {
        // one of our parent's other frames: same parent and top as us
        let parent = proxy(it, PROXY_PARENT);
        let top = proxy(it, PROXY_TOP);
        o.set_hidden("parent", parent);
        o.set_hidden("top", top);
    } else {
        let top = if id == PROXY_TOP || host(it).parent.as_ref().is_some_and(|p| p.depth <= 1) { ov.clone() } else { proxy(it, PROXY_TOP) };
        o.set_hidden("top", top.clone());
        o.set_hidden("parent", if id == PROXY_TOP { ov.clone() } else { top });
    }
    it.define_method(&o, "postMessage", 2, move |it, _, args| {
        post_message(it, id, args)?;
        Ok(Value::Undefined)
    });
    for name in ["focus", "blur", "close", "stop", "print"] {
        it.define_method(&o, name, 0, |_, _, _| Ok(Value::Undefined));
    }
    if id == PROXY_PARENT || id == PROXY_TOP {
        // `parent.location = url` / `top.location.href = url` navigate the window above
        let loc = it.new_object();
        let to_top = id == PROXY_TOP;
        let up = move |it: &mut Interp, v: &Value| -> Result<(), Value> {
            let s = it.to_string_lossy(v);
            if let Some(url) = host(it).resolve_url(&s) {
                let headers = host(it).request_headers("GET", &url);
                let load = NavRequest::Load { url, method: "GET".into(), body: None, replace: false, headers };
                host(it).nav = Some(NavRequest::Up { nav: Box::new(load), top: to_top });
            }
            Ok(())
        };
        it.define_accessor(&loc, "href", |_, _, _| Ok(Value::str("")), move |it, _, args| {
            up(it, &arg(args, 0))?;
            Ok(Value::Undefined)
        });
        for m in ["assign", "replace"] {
            it.define_method(&loc, m, 1, move |it, _, args| {
                up(it, &arg(args, 0))?;
                Ok(Value::Undefined)
            });
        }
        o.set_hidden("location", Value::Obj(loc));
    }
    if id >= 0 {
        // `frame.location = url` / `frame.location.href = url` / `.replace(url)` navigate it
        let loc = it.new_object();
        let nav = move |it: &mut Interp, v: &Value| -> Result<(), Value> {
            let s = it.to_string_lossy(v);
            if let Some(u) = host(it).resolve_url(&s) {
                host(it).frame_navs.push((id as NodeId, u));
            }
            Ok(())
        };
        let nav2 = nav;
        it.define_accessor(&loc, "href", |_, _, _| Ok(Value::str("")), move |it, _, args| {
            nav(it, &arg(args, 0))?;
            Ok(Value::Undefined)
        });
        for m in ["assign", "replace"] {
            it.define_method(&loc, m, 1, move |it, _, args| {
                nav2(it, &arg(args, 0))?;
                Ok(Value::Undefined)
            });
        }
        o.set_hidden("location", Value::Obj(loc));
    }
    // the parent and top windows resolve their frames by index or name (`parent.frames[n]`)
    let out = if id == PROXY_PARENT || id == PROXY_TOP {
        let g = Value::Obj(it.global.clone());
        match it.get_str(&g, "__tip_wrap_window") {
            Ok(f) if f.is_callable() => it.call(&f, Value::Undefined, &[ov.clone()]).unwrap_or(ov),
            _ => ov,
        }
    } else {
        ov
    };
    if let Value::Obj(w) = &out {
        host(it).frame_proxies.insert(id, w.clone());
    }
    out
}

/// Ids of proxies for the parent's other frames, allocated per key.
const SIBLING_BASE: i64 = -1000;

fn sibling_id(it: &mut Interp, key: &str) -> Value {
    let h = host(it);
    let idx = match h.sibling_keys.iter().position(|k| k == key) {
        Some(i) => i,
        None => {
            h.sibling_keys.push(key.to_string());
            h.sibling_keys.len() - 1
        }
    };
    proxy(it, SIBLING_BASE - idx as i64)
}

/// `parent.frames[key]` from inside a frame: a proxy for the parent's frame, or null.
pub fn frame_lookup(it: &mut Interp, window: i64, key: &str) -> Value {
    // only direct siblings are known (top, when it is not our parent, is opaque)
    let parent_is_top = host(it).parent.as_ref().is_some_and(|p| p.depth <= 1);
    if window == PROXY_TOP && !parent_is_top {
        return Value::Null;
    }
    let Some(names) = host(it).parent.as_ref().map(|p| p.siblings.borrow().clone()) else { return Value::Null };
    if key == "length" {
        return Value::Num(names.len() as f64);
    }
    let canonical = match key.parse::<usize>() {
        Ok(i) if i < names.len() => match names[i].as_str() {
            "" => format!("#{i}"),
            n => n.to_string(),
        },
        Ok(_) => return Value::Null,
        Err(_) if !key.is_empty() && names.iter().any(|n| n == key) => key.to_string(),
        Err(_) => return Value::Null,
    };
    sibling_id(it, &canonical)
}

/// Does a `postMessage` targetOrigin ("*", or a URL like "https://a.com:443/x") allow a
/// window with origin `origin`? The target is parsed, so default ports and paths don't matter.
fn origin_allows(target: &str, origin: &str) -> bool {
    if target == "*" || target == origin {
        return true;
    }
    match Url::parse(target) {
        Some(u) => u.origin() == origin,
        None => false,
    }
}

/// `TIP_TRACE_MESSAGES=1` prints cross-window messages (debugging real sites).
pub fn trace(kind: &str, from: &str, data: &Cloned, ports: usize) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var("TIP_TRACE_MESSAGES").is_ok()) {
        let s = format!("{data:?}");
        let s: String = s.chars().take(160).collect();
        static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let t = T0.get_or_init(std::time::Instant::now).elapsed().as_secs_f32();
        eprintln!("[msg {t:7.2}s] {kind} from {from} ports={ports}: {s}");
    }
}

fn post_message(it: &mut Interp, id: i64, args: &[Value]) -> Result<(), Value> {
    let data = export(it, &arg(args, 0))?;
    let opt = arg(args, 1);
    // postMessage(data, targetOrigin, transfer) or postMessage(data, { targetOrigin, transfer })
    let transfer = match &opt {
        Value::Obj(_) => it.get_str(&opt, "transfer")?,
        _ => arg(args, 2),
    };
    let target_origin = match &opt {
        Value::Str(s) => s.to_string(),
        Value::Obj(_) => {
            let t = it.get_str(&opt, "targetOrigin")?;
            if t.is_undefined() { "/".to_string() } else { it.to_string_lossy(&t) }
        }
        Value::Undefined => "/".to_string(),
        other => it.to_string_lossy(other),
    };
    let origin = effective_origin(host(it));
    let target_origin = if target_origin == "/" { origin.clone() } else { target_origin };
    trace(if id >= 0 { "to frame" } else if id == PROXY_TOP { "to top" } else { "to parent" }, &host(it).url.to_string().chars().take(60).collect::<String>(), &data, 0);
    if id >= 0 {
        let node = id as NodeId;
        let h = host(it);
        let Some(f) = h.frames.iter_mut().find(|f| f.node == node) else { return Ok(()) };
        let Some(child) = &mut f.page else { return Ok(()) };
        let child_origin = effective_origin(child.host());
        if !origin_allows(&target_origin, &child_origin) {
            return Ok(());
        }
        let ports = transfer_ports(it, &transfer)?;
        let h = host(it);
        if let Some(child) = h.frames.iter_mut().find(|f| f.node == node).and_then(|f| f.page.as_mut()) {
            child.host_mut().inbox.push(Message { data, origin, source: Source::Parent, ports });
        }
    } else {
        if host(it).parent.is_none() {
            return Ok(());
        }
        let ports = transfer_ports(it, &transfer)?;
        let sibling = if id <= SIBLING_BASE { host(it).sibling_keys.get((SIBLING_BASE - id) as usize).cloned() } else { None };
        if let Some(p) = &host(it).parent {
            p.outbox.borrow_mut().push(UpMessage { data, origin, target_origin, to_top: id == PROXY_TOP, ports, sibling });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- MessagePorts across windows

/// Mailboxes of MessagePort ends whose peer lives in another window, by port id.
fn mailboxes() -> std::sync::MutexGuard<'static, HashMap<u64, Vec<Cloned>>> {
    static M: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, Vec<Cloned>>>> = std::sync::OnceLock::new();
    M.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

fn next_port_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn num_prop(it: &mut Interp, o: &Value, k: &str) -> Option<u64> {
    match it.get_str(o, k) {
        Ok(Value::Num(n)) if n > 0.0 => Some(n as u64),
        _ => None,
    }
}

/// Detach the MessagePorts in a transfer list from this window. Each becomes an end whose
/// peer is reached through a mailbox; returns their (id, peer id) for the receiver.
pub fn transfer_ports(it: &mut Interp, transfer: &Value) -> Result<Vec<(u64, u64)>, Value> {
    let Value::Obj(_) = transfer else { return Ok(vec![]) };
    let items = it.iterate_to_vec(transfer)?;
    let mut out = Vec::new();
    for p in items {
        let tag = it.get_str(&p, "__tipPort")?;
        if !tag.truthy() {
            continue;
        }
        let (id, peer) = match (num_prop(it, &p, "_id"), num_prop(it, &p, "_remote")) {
            (Some(id), Some(peer)) => {
                // already remote: hand this end over to the receiving window
                host(it).ports.remove(&id);
                (id, peer)
            }
            _ => {
                // a local pair: the other end stays here and talks to the transferred one
                let id = next_port_id();
                let other = it.get_str(&p, "_other")?;
                let peer = next_port_id();
                if let Value::Obj(o) = &other {
                    o.set_prop("_id", Value::Num(peer as f64));
                    o.set_prop("_remote", Value::Num(id as f64));
                    o.set_prop("_other", Value::Null);
                    host(it).ports.insert(peer, other.clone());
                }
                (id, peer)
            }
        };
        if let Value::Obj(o) = &p {
            o.set_prop("_open", Value::Bool(false));
            o.set_prop("_other", Value::Null);
        }
        out.push((id, peer));
    }
    Ok(out)
}

/// A MessagePort in this window for a transferred end.
pub fn adopt_port(it: &mut Interp, id: u64, peer: u64) -> Value {
    let g = Value::Obj(it.global.clone());
    let port = match it.get_str(&g, "MessagePort").ok().filter(|c| c.is_callable()) {
        Some(c) => it.construct(&c, &[], None).unwrap_or(Value::Undefined),
        None => Value::Undefined,
    };
    if let Value::Obj(o) = &port {
        o.set_prop("_id", Value::Num(id as f64));
        o.set_prop("_remote", Value::Num(peer as f64));
        host(it).ports.insert(id, port.clone());
    }
    port
}

/// `port.postMessage` for an end whose peer is in another window (called from the prelude).
pub fn port_send(it: &mut Interp, peer: u64, data: &Value) -> Result<(), Value> {
    let c = export(it, data)?;
    trace(&format!("port -> {peer}"), &host(it).url.to_string().chars().take(60).collect::<String>(), &c, 0);
    mailboxes().entry(peer).or_default().push(c);
    Ok(())
}

impl Page {
    /// Deliver messages that arrived on this window's remote MessagePorts.
    pub fn deliver_port_messages(&mut self) -> bool {
        if self.host().ports.is_empty() {
            return false;
        }
        let ids: Vec<u64> = self.host().ports.keys().copied().collect();
        let mut any = false;
        for id in ids {
            let msgs = mailboxes().remove(&id).unwrap_or_default();
            if msgs.is_empty() {
                continue;
            }
            any = true;
            for m in &msgs {
                trace(&format!("deliver on port {id} in"), &self.host().url.to_string().chars().take(60).collect::<String>(), m, 0);
            }
            let Some(port) = self.host().ports.get(&id).cloned() else { continue };
            let g = Value::Obj(self.interp.global.clone());
            let Ok(deliver) = self.interp.get_str(&g, "__tip_port_deliver") else { continue };
            for m in msgs {
                let data = import(&mut self.interp, &m);
                let _ = self.interp.call(&deliver, Value::Undefined, &[port.clone(), data]);
                self.after_task();
            }
        }
        any
    }

    pub fn has_port_messages(&self) -> bool {
        let h = self.host();
        if h.ports.is_empty() {
            return false;
        }
        let m = mailboxes();
        h.ports.keys().any(|id| m.get(id).is_some_and(|v| !v.is_empty()))
    }
}

/// Globals of a framed window: `parent`, `top`, `name`, `frameElement`.
fn install_child_globals(it: &mut Interp, name: &str) {
    let g = it.global.clone();
    let parent = proxy(it, PROXY_PARENT);
    let top = proxy(it, PROXY_TOP);
    g.set_hidden("parent", parent);
    g.set_hidden("top", top);
    g.set_hidden("name", Value::str(name));
    g.set_hidden("frameElement", Value::Null);
}

/// `window.length` / `window.frames[i]` / `iframe.contentWindow` from the parent's side.
pub fn content_window(it: &mut Interp, node: NodeId) -> Value {
    if host(it).frames.iter().any(|f| f.node == node) {
        proxy(it, node as i64)
    } else {
        Value::Null
    }
}

/// Proxies for this window's child frames, in document order.
pub fn child_windows(it: &mut Interp) -> Vec<Value> {
    let nodes: Vec<NodeId> = host(it).frames.iter().map(|f| f.node).collect();
    nodes.into_iter().map(|n| proxy(it, n as i64)).collect()
}

/// Navigations requested through frame proxies (`frame.location = ...`).
pub fn take_frame_navs(page: &mut Page) -> bool {
    let navs = std::mem::take(&mut page.host_mut().frame_navs);
    let any = !navs.is_empty();
    for (node, url) in navs {
        page.navigate_frame(node, url);
    }
    any
}

pub type FrameProxies = HashMap<i64, Obj>;

#[cfg(test)]
mod tests {
    use super::framing_allowed;
    use crate::net::Response;

    fn resp(headers: &[(&str, &str)]) -> Response {
        Response { url: "https://widgets.example.com/w".into(), status: 200, status_text: "OK".into(), headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), body: vec![], from_os: true }
    }

    #[test]
    fn frame_ancestors_and_x_frame_options() {
        let parent = "https://www.example.com";
        assert!(framing_allowed(&resp(&[]), parent));
        assert!(!framing_allowed(&resp(&[("X-Frame-Options", "DENY")]), parent));
        assert!(!framing_allowed(&resp(&[("X-Frame-Options", "SAMEORIGIN")]), parent));
        assert!(framing_allowed(&resp(&[("x-frame-options", "SAMEORIGIN")]), "https://widgets.example.com"));
        // merged headers, as NSURLSession reports them
        let merged = "frame-ancestors https://www.example.com, script-src 'nonce-x' 'unsafe-inline';object-src 'none'";
        assert!(framing_allowed(&resp(&[("content-security-policy", merged)]), parent));
        assert!(!framing_allowed(&resp(&[("content-security-policy", merged)]), "https://evil.test"));
        assert!(framing_allowed(&resp(&[("content-security-policy", "frame-ancestors 'self' https://*.example.com")]), parent));
        assert!(!framing_allowed(&resp(&[("content-security-policy", "frame-ancestors 'none'")]), parent));
    }
}
