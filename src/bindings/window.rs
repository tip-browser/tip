//! Window-level APIs: location, history, timers, fetch/XHR, storage, URL, observers...

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::events::{self, EventInit};
use super::{node_list, node_of, ph, wrap, wrap_opt, StyleHost};
use crate::dom::NodeId;
use crate::js::builtins::{array_like_to_vec, promise};
use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;
use crate::net::{self, Request, Url};
use crate::page::{NavRequest, Target, Timer};

static START: OnceLock<Instant> = OnceLock::new();

pub fn perf_now() -> f64 {
    let s = START.get_or_init(Instant::now);
    s.elapsed().as_secs_f64() * 1000.0
}

fn s(v: impl AsRef<str>) -> Value {
    Value::str(v.as_ref())
}

pub fn current_script(it: &mut Interp) -> Value {
    let cs = ph(it).current_script;
    wrap_opt(it, cs)
}

pub fn document_write(it: &mut Interp, html: &str) {
    let h = ph(it);
    let frag = h.doc.create_fragment();
    crate::html::parse_fragment_into(&mut h.doc, frag, html);
    match h.current_script.and_then(|s| h.doc.parent(s).map(|p| (s, p))) {
        Some((s, p)) => {
            let next = h.doc.next_sibling(s);
            h.doc.insert_before(p, frag, next);
        }
        None => {
            if let Some(b) = h.doc.body() {
                h.doc.append_child(b, frag);
            }
        }
    }
}

pub fn get_selection(it: &mut Interp) -> R<Value> {
    let o = it.new_object();
    o.set_prop("rangeCount", Value::Num(0.0));
    o.set_prop("isCollapsed", Value::Bool(true));
    o.set_prop("type", s("None"));
    for name in ["removeAllRanges", "addRange", "collapse", "selectAllChildren", "empty", "extend"] {
        let f = it.native(name, 0, |_, _, _| Ok(Value::Undefined));
        o.set_hidden(name, f);
    }
    let ts = it.native("toString", 0, |_, _, _| Ok(s("")));
    o.set_hidden("toString", ts);
    Ok(Value::Obj(o))
}

pub fn scroll_args(it: &mut Interp, args: &[Value]) -> R<(Option<f32>, Option<f32>)> {
    let a0 = arg(args, 0);
    if let Value::Obj(_) = a0 {
        let l = it.get_str(&a0, "left")?;
        let t = it.get_str(&a0, "top")?;
        let x = if l.is_undefined() { None } else { Some(it.to_number(&l)? as f32) };
        let y = if t.is_undefined() { None } else { Some(it.to_number(&t)? as f32) };
        return Ok((x, y));
    }
    let x = if a0.is_undefined() { None } else { Some(it.to_number(&a0)? as f32) };
    let a1 = arg(args, 1);
    let y = if a1.is_undefined() { None } else { Some(it.to_number(&a1)? as f32) };
    Ok((x, y))
}

/// Element methods that need input/default-action logic.
pub fn element_action(it: &mut Interp, n: NodeId, name: &str, args: &[Value]) -> R<Value> {
    match name {
        "focus" => crate::input::focus(it, Some(n)),
        "blur" => {
            if ph(it).focus == Some(n) {
                crate::input::focus(it, None);
            }
        }
        "click" => crate::input::click_element(it, n, events::Mods::default()),
        "submit" => crate::input::submit_form(it, n, None, false),
        "requestSubmit" => crate::input::submit_form(it, n, None, true),
        "reset" => {
            let h = ph(it);
            let ctrls: Vec<NodeId> = h.doc.element_descendants(n);
            for c in ctrls {
                h.form.remove(&c);
            }
            h.invalidate();
        }
        "select" => {
            let h = ph(it);
            h.focus = Some(n);
            h.invalidate();
        }
        "showModal" | "show" => {
            ph(it).doc.set_attr(n, "open", "");
        }
        "close" => {
            ph(it).doc.remove_attr(n, "open");
            events::fire(it, Target::Node(n), "close", "Event", EventInit::default());
        }
        "checkValidity" | "reportValidity" => return Ok(Value::Bool(crate::input::validate(it, n))),
        "play" => return Ok(promise::promise_resolve(it, Value::Undefined)?),
        _ => {
            let _ = args;
        }
    }
    Ok(Value::Undefined)
}

// ---------------------------------------------------------------- storage

fn storage_file() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    let dir = std::path::Path::new(&home).join(".tip");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("localStorage.tsv"))
}

type Store = HashMap<String, Vec<(String, String)>>;
static LOCAL: OnceLock<Mutex<Store>> = OnceLock::new();
static SESSION: OnceLock<Mutex<Store>> = OnceLock::new();

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

fn unesc(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn local_store() -> &'static Mutex<Store> {
    LOCAL.get_or_init(|| {
        let mut m: Store = HashMap::new();
        if std::env::var("TIP_EPHEMERAL").is_err() {
            if let Some(text) = storage_file().and_then(|p| std::fs::read_to_string(p).ok()) {
                for line in text.lines() {
                    let parts: Vec<&str> = line.splitn(3, '\t').collect();
                    if parts.len() == 3 {
                        m.entry(unesc(parts[0])).or_default().push((unesc(parts[1]), unesc(parts[2])));
                    }
                }
            }
        }
        Mutex::new(m)
    })
}

fn persist_local() {
    if std::env::var("TIP_EPHEMERAL").is_ok() {
        return;
    }
    let Some(p) = storage_file() else { return };
    let m = local_store().lock().unwrap_or_else(|e| e.into_inner());
    let mut out = String::new();
    for (origin, items) in m.iter() {
        for (k, v) in items {
            out.push_str(&format!("{}\t{}\t{}\n", esc(origin), esc(k), esc(v)));
        }
    }
    let _ = std::fs::write(p, out);
}

pub struct StorageHost {
    pub local: bool,
    pub origin: String,
}

impl StorageHost {
    fn store(&self) -> &'static Mutex<Store> {
        if self.local { local_store() } else { SESSION.get_or_init(|| Mutex::new(HashMap::new())) }
    }
    fn items(&self) -> Vec<(String, String)> {
        self.store().lock().unwrap_or_else(|e| e.into_inner()).get(&self.origin).cloned().unwrap_or_default()
    }
    fn set_item(&self, k: &str, v: &str) {
        {
            let mut m = self.store().lock().unwrap_or_else(|e| e.into_inner());
            let list = m.entry(self.origin.clone()).or_default();
            match list.iter_mut().find(|(kk, _)| kk == k) {
                Some(e) => e.1 = v.to_string(),
                None => list.push((k.to_string(), v.to_string())),
            }
        }
        if self.local {
            persist_local();
        }
    }
    fn remove(&self, k: Option<&str>) {
        {
            let mut m = self.store().lock().unwrap_or_else(|e| e.into_inner());
            if let Some(list) = m.get_mut(&self.origin) {
                match k {
                    Some(k) => list.retain(|(kk, _)| kk != k),
                    None => list.clear(),
                }
            }
        }
        if self.local {
            persist_local();
        }
    }
}

const STORAGE_METHODS: &[&str] = &["getItem", "setItem", "removeItem", "clear", "key", "length", "constructor", "toString", "hasOwnProperty", "valueOf"];

impl HostObject for StorageHost {
    fn class_name(&self) -> &'static str {
        "Storage"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn get(&self, _it: &mut Interp, key: &PropKey) -> Option<R<Value>> {
        let k = key.as_str()?;
        if k == "length" {
            return Some(Ok(Value::Num(self.items().len() as f64)));
        }
        if STORAGE_METHODS.contains(&k) {
            return None;
        }
        self.items().into_iter().find(|(kk, _)| kk == k).map(|(_, v)| Ok(s(v)))
    }
    fn set(&self, it: &mut Interp, key: &PropKey, v: &Value) -> Option<R<()>> {
        let k = key.as_str()?;
        if STORAGE_METHODS.contains(&k) {
            return None;
        }
        let val = it.to_string_lossy(v);
        self.set_item(k, &val);
        Some(Ok(()))
    }
    fn keys(&self, _it: &mut Interp) -> Vec<PropKey> {
        self.items().into_iter().map(|(k, _)| PropKey::Str(Rc::from(k.as_str()))).collect()
    }
}

fn storage_of(it: &mut Interp, this: &Value) -> R<Rc<dyn HostObject>> {
    match this.as_obj().and_then(|o| o.host()) {
        Some(h) if h.as_any().downcast_ref::<StorageHost>().is_some() => Ok(h),
        _ => throw!(it, "TypeError", "Illegal invocation"),
    }
}

// ---------------------------------------------------------------- location

pub struct LocationHost;

impl HostObject for LocationHost {
    fn class_name(&self) -> &'static str {
        "Location"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn get(&self, it: &mut Interp, key: &PropKey) -> Option<R<Value>> {
        let u = ph(it).url.clone();
        Some(Ok(match key.as_str()? {
            "href" => s(u.to_string()),
            "protocol" => s(format!("{}:", u.scheme)),
            "host" => s(u.host_port()),
            "hostname" => s(&u.host),
            "port" => s(u.port.map(|p| p.to_string()).unwrap_or_default()),
            "pathname" => s(if u.opaque { u.path.clone() } else { u.path.clone() }),
            "search" => s(u.search()),
            "hash" => s(u.hash()),
            "origin" => s(u.origin()),
            "ancestorOrigins" => it.new_array(vec![]),
            _ => return None,
        }))
    }
    fn set(&self, it: &mut Interp, key: &PropKey, v: &Value) -> Option<R<()>> {
        let k = key.as_str()?;
        let val = it.to_string_lossy(v);
        let h = ph(it);
        let mut u = h.url.clone();
        match k {
            "href" => {
                if let Some(n) = h.resolve_url(&val) {
                    navigate(h, n, false);
                }
            }
            "hash" => {
                u.fragment = Some(val.trim_start_matches('#').to_string());
                h.nav = Some(NavRequest::SameDocument { url: u, replace: false });
            }
            "search" => {
                u.query = Some(val.trim_start_matches('?').to_string());
                navigate(h, u, false);
            }
            "pathname" => {
                if let Some(n) = u.join(&val) {
                    navigate(h, n, false);
                }
            }
            _ => return None,
        }
        Some(Ok(()))
    }
}

fn navigate(h: &mut crate::page::PageHost, url: Url, replace: bool) {
    // same document except for the fragment?
    if url.without_fragment() == h.url.without_fragment() && url.fragment.is_some() {
        h.nav = Some(NavRequest::SameDocument { url, replace });
        return;
    }
    if url.scheme == "javascript" {
        return;
    }
    let headers = h.request_headers("GET", &url);
    h.nav = Some(NavRequest::Load { url, method: "GET".into(), body: None, replace, headers });
}

// ---------------------------------------------------------------- fetch / XHR

/// A live EventSource connection; dropping it stops the network thread.
pub struct EventSourceConn {
    pub obj: Value,
    rx: std::sync::mpsc::Receiver<SseEvent>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for EventSourceConn {
    fn drop(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

enum SseEvent {
    Open,
    Message { ty: String, data: String, id: String },
    Error { fatal: bool },
}

/// Incremental `text/event-stream` parser.
#[derive(Default)]
struct SseParser {
    line: Vec<u8>,
    data: String,
    has_data: bool,
    event: String,
    last_id: String,
    retry: Option<u64>,
    skip_lf: bool,
}

impl SseParser {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<SseEvent>) {
        for &b in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if b == b'\n' {
                    continue;
                }
            }
            if b == b'\r' || b == b'\n' {
                self.skip_lf = b == b'\r';
                let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
                self.line_done(&line, out);
            } else {
                self.line.push(b);
            }
        }
    }

    fn line_done(&mut self, line: &str, out: &mut Vec<SseEvent>) {
        if line.is_empty() {
            if self.has_data {
                let mut data = std::mem::take(&mut self.data);
                if data.ends_with('\n') {
                    data.pop();
                }
                let ty = if self.event.is_empty() { "message".to_string() } else { self.event.clone() };
                out.push(SseEvent::Message { ty, data, id: self.last_id.clone() });
            }
            self.data.clear();
            self.has_data = false;
            self.event.clear();
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
                self.has_data = true;
            }
            "event" => self.event = value.to_string(),
            "id" if !value.contains('\0') => self.last_id = value.to_string(),
            "retry" => {
                if let Ok(n) = value.parse() {
                    self.retry = Some(n);
                }
            }
            _ => {}
        }
    }
}

fn start_event_source(url: Url, origin: String, obj: Value) -> EventSourceConn {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (tx, rx) = std::sync::mpsc::channel();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    std::thread::spawn(move || {
        let mut parser = SseParser::default();
        let mut retry = 3000u64;
        loop {
            if c2.load(Ordering::Relaxed) {
                return;
            }
            let mut headers = vec![("Accept".to_string(), "text/event-stream".to_string()), ("Cache-Control".to_string(), "no-cache".to_string())];
            if !parser.last_id.is_empty() {
                headers.push(("Last-Event-ID".to_string(), parser.last_id.clone()));
            }
            if url.origin() != origin {
                headers.push(("Origin".to_string(), origin.clone()));
            }
            let req = Request { method: "GET".into(), url: url.clone(), headers, body: None };
            let mut fatal = false;
            let r = if url.scheme == "http" {
                net::http::stream(&req, &url, &c2, &mut |m| match m {
                    net::http::StreamMsg::Head { status, headers } => {
                        let ct = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.to_ascii_lowercase()).unwrap_or_default();
                        if status != 200 || !ct.contains("text/event-stream") {
                            fatal = status != 200 && status != 204 || !ct.contains("text/event-stream");
                            return false;
                        }
                        parser = SseParser { last_id: std::mem::take(&mut parser.last_id), ..Default::default() };
                        tx.send(SseEvent::Open).is_ok()
                    }
                    net::http::StreamMsg::Data(bytes) => {
                        let mut evs = Vec::new();
                        parser.feed(bytes, &mut evs);
                        if let Some(r) = parser.retry.take() {
                            retry = r;
                        }
                        evs.into_iter().all(|e| tx.send(e).is_ok())
                    }
                })
            } else {
                // other schemes: fetch the whole response and replay it
                match net::fetch(&req) {
                    Ok(resp) if resp.status == 200 && resp.mime().0.contains("text/event-stream") => {
                        let _ = tx.send(SseEvent::Open);
                        let mut evs = Vec::new();
                        parser.feed(&resp.body, &mut evs);
                        parser.feed(b"\n\n", &mut evs);
                        for e in evs {
                            let _ = tx.send(e);
                        }
                        Ok(())
                    }
                    Ok(_) => {
                        fatal = true;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            };
            let _ = r;
            if c2.load(Ordering::Relaxed) {
                return;
            }
            if tx.send(SseEvent::Error { fatal }).is_err() || fatal {
                return;
            }
            let until = Instant::now() + Duration::from_millis(retry);
            while Instant::now() < until {
                if c2.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    });
    EventSourceConn { obj, rx, cancel }
}

/// Deliver EventSource events to script. Returns true if any were dispatched.
pub fn poll_event_sources(it: &mut Interp) -> bool {
    let mut evs: Vec<(Value, SseEvent)> = Vec::new();
    for c in &ph(it).event_sources {
        while let Ok(e) = c.rx.try_recv() {
            evs.push((c.obj.clone(), e));
            if evs.len() > 500 {
                break;
            }
        }
    }
    if evs.is_empty() {
        return false;
    }
    let origin = ph(it).url.origin();
    for (obj, e) in evs {
        let Value::Obj(o) = &obj else { continue };
        if o.get_own("readyState").is_some_and(|v| matches!(v, Value::Num(n) if n == 2.0)) {
            continue;
        }
        let target = events::target_of(it, &obj);
        match e {
            SseEvent::Open => {
                o.set_prop("readyState", Value::Num(1.0));
                events::fire(it, target, "open", "Event", EventInit::default());
            }
            SseEvent::Message { ty, data, id } => {
                let init = EventInit { props: vec![("data", s(data)), ("lastEventId", s(id)), ("origin", s(origin.clone()))], ..Default::default() };
                events::fire(it, target, &ty, "MessageEvent", init);
            }
            SseEvent::Error { fatal } => {
                o.set_prop("readyState", Value::Num(if fatal { 2.0 } else { 0.0 }));
                if fatal {
                    ph(it).event_sources.retain(|c| !matches!(&c.obj, Value::Obj(x) if x.ptr_eq(o)));
                }
                events::fire(it, target, "error", "Event", EventInit::default());
            }
        }
        it.run_microtasks();
    }
    true
}

pub struct PendingFetch {
    pub rx: std::sync::mpsc::Receiver<Result<net::Response, String>>,
    pub on_done: Value,
}

fn spawn_fetch(it: &mut Interp, req: Request, on_done: Value) {
    let (tx, rx) = std::sync::mpsc::channel();
    let req2 = req.clone();
    std::thread::spawn(move || {
        let _ = tx.send(net::fetch(&req2));
    });
    let h = ph(it);
    h.pending_fetches.push(PendingFetch { rx, on_done });
    h.net_log_pending.push((req.url.to_string(), req.method.clone(), Instant::now()));
}

fn headers_obj(it: &mut Interp, headers: &[(String, String)]) -> Value {
    let o = it.new_object();
    let list: Vec<(String, String)> = headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.clone())).collect();
    let l1 = list.clone();
    let get = it.native("get", 1, move |it, _, args| {
        let k = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        let vals: Vec<&str> = l1.iter().filter(|(kk, _)| *kk == k).map(|(_, v)| v.as_str()).collect();
        Ok(if vals.is_empty() { Value::Null } else { s(vals.join(", ")) })
    });
    let l2 = list.clone();
    let has = it.native("has", 1, move |it, _, args| {
        let k = it.to_str(&arg(args, 0))?.to_ascii_lowercase();
        Ok(Value::Bool(l2.iter().any(|(kk, _)| *kk == k)))
    });
    let l3 = list.clone();
    let for_each = it.native("forEach", 1, move |it, _, args| {
        let f = arg(args, 0);
        for (k, v) in &l3 {
            it.call(&f, Value::Undefined, &[s(v), s(k)])?;
        }
        Ok(Value::Undefined)
    });
    let l4 = list;
    let entries = it.native("entries", 0, move |it, _, _| {
        let items: Vec<Value> = l4.iter().map(|(k, v)| it.new_array(vec![s(k), s(v)])).collect();
        Ok(crate::js::builtins::make_list_iterator(it, items))
    });
    o.set_hidden("get", get);
    o.set_hidden("has", has);
    o.set_hidden("forEach", for_each);
    o.set_hidden("entries", entries.clone());
    let si = it.sym_iterator.clone();
    o.borrow_mut().props.insert(PropKey::Sym(si), Prop::hidden(entries));
    Value::Obj(o)
}

fn response_obj(it: &mut Interp, r: &net::Response) -> Value {
    let o = it.new_object();
    o.set_prop("status", Value::Num(r.status as f64));
    o.set_prop("statusText", s(&r.status_text));
    o.set_prop("ok", Value::Bool(r.ok()));
    o.set_prop("url", s(&r.url));
    o.set_prop("redirected", Value::Bool(false));
    o.set_prop("type", s("basic"));
    let h = headers_obj(it, &r.headers);
    o.set_prop("headers", h);
    o.set_prop("bodyUsed", Value::Bool(false));
    let body = Rc::new(r.body.clone());
    let text = r.text();
    let t1 = text.clone();
    let f_text = it.native("text", 0, move |it, _, _| promise::promise_resolve(it, s(&t1)));
    let t2 = text;
    let f_json = it.native("json", 0, move |it, _, _| {
        let (p, res, rej) = promise::new_capability(it);
        match crate::js::builtins::json::parse(it, &t2) {
            Ok(v) => it.call(&res, Value::Undefined, &[v])?,
            Err(e) => it.call(&rej, Value::Undefined, &[e])?,
        };
        Ok(p)
    });
    let b1 = body.clone();
    let f_ab = it.native("arrayBuffer", 0, move |it, _, _| {
        let buf = Obj::new(ObjKind::ArrayBuffer(Rc::new(std::cell::RefCell::new((*b1).clone()))), Some(it.p.arraybuffer.clone()), "ArrayBuffer");
        promise::promise_resolve(it, Value::Obj(buf))
    });
    let b2 = body;
    let f_bytes = it.native("bytes", 0, move |it, _, _| {
        let t = crate::js::builtins::typed::new_typed(it, TypedTy::U8, (*b2).clone());
        promise::promise_resolve(it, t)
    });
    let me = o.clone();
    let f_clone = it.native("clone", 0, move |_, _, _| Ok(Value::Obj(me.clone())));
    o.set_hidden("text", f_text);
    o.set_hidden("json", f_json);
    o.set_hidden("arrayBuffer", f_ab.clone());
    o.set_hidden("blob", f_ab);
    o.set_hidden("bytes", f_bytes);
    o.set_hidden("clone", f_clone);
    Value::Obj(o)
}

fn body_bytes(it: &mut Interp, body: &Value, headers: &mut Vec<(String, String)>) -> R<Option<Vec<u8>>> {
    Ok(match body {
        Value::Undefined | Value::Null => None,
        Value::Str(st) => {
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                headers.push(("Content-Type".into(), "text/plain;charset=UTF-8".into()));
            }
            Some(st.as_bytes().to_vec())
        }
        v => {
            if let Some(b) = crate::js::builtins::typed::bytes_of(v) {
                return Ok(Some(b));
            }
            // Blob (prelude) exposes its bytes and type
            let bb = it.get_str(v, "__tipBytes")?;
            if bb.is_callable() {
                let bytes = it.call(&bb, v.clone(), &[])?;
                let ty = it.get_str(v, "type")?;
                if let Value::Str(t) = &ty {
                    if !t.is_empty() && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                        headers.push(("Content-Type".into(), t.to_string()));
                    }
                }
                return Ok(crate::js::builtins::typed::bytes_of(&bytes));
            }
            // URLSearchParams / FormData expose a urlencoded serialization
            let ser = it.get_str(v, "__tipSerialize")?;
            if ser.is_callable() {
                let st = it.call(&ser, v.clone(), &[])?;
                if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                    headers.push(("Content-Type".into(), "application/x-www-form-urlencoded;charset=UTF-8".into()));
                }
                return Ok(Some(it.to_str(&st)?.as_bytes().to_vec()));
            }
            Some(it.to_str(v)?.as_bytes().to_vec())
        }
    })
}

fn do_fetch(it: &mut Interp, args: &[Value]) -> R<Value> {
    let input = arg(args, 0);
    let init = arg(args, 1);
    let url_str = match &input {
        Value::Obj(_) => {
            let u = it.get_str(&input, "url")?;
            if u.is_undefined() { it.to_str(&input)?.to_string() } else { it.to_str(&u)?.to_string() }
        }
        v => it.to_str(v)?.to_string(),
    };
    let (p, res, rej) = promise::new_capability(it);
    let Some(url) = ph(it).resolve_url(&url_str) else {
        let e = it.make_error("TypeError", &format!("Failed to fetch: invalid URL {url_str}"));
        it.call(&rej, Value::Undefined, &[e])?;
        return Ok(p);
    };
    let mut method = "GET".to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut body = None;
    if let Value::Obj(_) = &init {
        let m = it.get_str(&init, "method")?;
        if !m.is_undefined() {
            method = it.to_str(&m)?.to_ascii_uppercase();
        }
        let hv = it.get_str(&init, "headers")?;
        if let Value::Obj(ho) = &hv {
            let ent = it.get_str(&hv, "entries")?;
            if ent.is_callable() && ho.get_own("get").is_some() {
                let iter = it.call(&ent, hv.clone(), &[])?;
                for pair in it.iterate_to_vec(&iter)? {
                    let k = it.get_str(&pair, "0")?;
                    let v = it.get_str(&pair, "1")?;
                    headers.push((it.to_str(&k)?.to_string(), it.to_str(&v)?.to_string()));
                }
            } else if ho.is_array() {
                for pair in it.iterate_to_vec(&hv)? {
                    let k = it.get_str(&pair, "0")?;
                    let v = it.get_str(&pair, "1")?;
                    headers.push((it.to_str(&k)?.to_string(), it.to_str(&v)?.to_string()));
                }
            } else {
                for k in it.own_keys(ho, false) {
                    let v = it.get(&hv, &k)?;
                    headers.push((k.as_str().unwrap_or("").to_string(), it.to_str(&v)?.to_string()));
                }
            }
        }
        let b = it.get_str(&init, "body")?;
        body = body_bytes(it, &b, &mut headers)?;
    }
    for h in ph(it).request_headers(&method, &url) {
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(&h.0)) {
            headers.push(h);
        }
    }
    let req = Request { method, url, headers, body };
    let on_done = it.native("", 1, move |it, _, args| {
        let r = arg(args, 0);
        if let Value::Str(e) = &r {
            let err = it.make_error("TypeError", &format!("Failed to fetch ({e})"));
            it.call(&rej, Value::Undefined, &[err])?;
        } else {
            it.call(&res, Value::Undefined, &[r])?;
        }
        Ok(Value::Undefined)
    });
    spawn_fetch(it, req, on_done);
    Ok(p)
}

/// Deliver completed fetches (called from the page event loop).
pub fn poll_fetches(it: &mut Interp) -> bool {
    let mut done = Vec::new();
    {
        let h = ph(it);
        let mut i = 0;
        while i < h.pending_fetches.len() {
            match h.pending_fetches[i].rx.try_recv() {
                Ok(r) => {
                    let pf = h.pending_fetches.remove(i);
                    let (url, method, t0) = if i < h.net_log_pending.len() { h.net_log_pending.remove(i) } else { (String::new(), "GET".into(), Instant::now()) };
                    let entry = match &r {
                        Ok(resp) => net::NetLogEntry { url, method, kind: "fetch", status: resp.status, size: resp.body.len(), mime: resp.mime().0, ms: t0.elapsed().as_millis(), error: None },
                        Err(e) => net::NetLogEntry { url, method, kind: "fetch", status: 0, size: 0, mime: String::new(), ms: t0.elapsed().as_millis(), error: Some(e.clone()) },
                    };
                    h.net_log.push(entry);
                    done.push((pf.on_done, r));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => i += 1,
                Err(_) => {
                    let pf = h.pending_fetches.remove(i);
                    if i < h.net_log_pending.len() {
                        h.net_log_pending.remove(i);
                    }
                    done.push((pf.on_done, Err("network thread failed".into())));
                }
            }
        }
    }
    let any = !done.is_empty();
    for (cb, r) in done {
        let v = match r {
            Ok(resp) => response_obj(it, &resp),
            Err(e) => s(e),
        };
        if let Err(e) = it.call(&cb, Value::Undefined, &[v]) {
            let msg = it.describe_error(&e);
            it.console.push(crate::js::interp::ConsoleMsg { level: "error", text: format!("Uncaught {msg}") });
        }
        it.run_microtasks();
    }
    any
}

fn xhr_setup(it: &mut Interp) {
    let proto = Obj::new(ObjKind::Ordinary, Some(it.p.object.clone()), "XMLHttpRequest");
    let et = it.global.get_own("EventTarget").and_then(|c| c.as_obj().and_then(|c| c.get_own("prototype")));
    if let Some(Value::Obj(e)) = et {
        proto.borrow_mut().proto = Some(e);
    }
    let ctor = it.define_ctor("XMLHttpRequest", 0, &proto, |it, this, _| {
        it.constructing = false;
        if let Value::Obj(o) = &this {
            o.set_prop("readyState", Value::Num(0.0));
            o.set_prop("status", Value::Num(0.0));
            o.set_prop("statusText", s(""));
            o.set_prop("responseText", s(""));
            o.set_prop("response", s(""));
            o.set_prop("responseType", s(""));
            o.set_prop("responseURL", s(""));
            o.set_prop("timeout", Value::Num(0.0));
            o.set_prop("withCredentials", Value::Bool(false));
            o.set_hidden("__headers", Value::Obj(it.new_object()));
            let up = it.new_object();
            o.set_prop("upload", Value::Obj(up));
        }
        Ok(this)
    });
    for (k, v) in [("UNSENT", 0), ("OPENED", 1), ("HEADERS_RECEIVED", 2), ("LOADING", 3), ("DONE", 4)] {
        ctor.set_hidden(k, Value::Num(v as f64));
        proto.set_hidden(k, Value::Num(v as f64));
    }
    it.define_method(&proto, "open", 3, |it, this, args| {
        let Value::Obj(o) = &this else { return Ok(Value::Undefined) };
        let m = it.to_str(&arg(args, 0))?.to_ascii_uppercase();
        let u = it.to_str(&arg(args, 1))?;
        o.set_hidden("__method", s(&m));
        o.set_hidden("__url", Value::Str(u));
        o.set_hidden("__async", Value::Bool(args.len() < 3 || arg(args, 2).truthy()));
        o.set_prop("readyState", Value::Num(1.0));
        Ok(Value::Undefined)
    });
    it.define_method(&proto, "setRequestHeader", 2, |it, this, args| {
        let hv = it.get_str(&this, "__headers")?;
        let k = it.to_str(&arg(args, 0))?;
        let v = it.to_str(&arg(args, 1))?;
        it.set(&hv, PropKey::Str(k), Value::Str(v))?;
        Ok(Value::Undefined)
    });
    it.define_method(&proto, "overrideMimeType", 1, |_, _, _| Ok(Value::Undefined));
    it.define_method(&proto, "abort", 0, |_, this, _| {
        if let Value::Obj(o) = &this {
            o.set_hidden("__aborted", Value::Bool(true));
        }
        Ok(Value::Undefined)
    });
    it.define_method(&proto, "getResponseHeader", 1, |it, this, args| {
        let hv = it.get_str(&this, "__respHeaders")?;
        if hv.is_undefined() {
            return Ok(Value::Null);
        }
        let get = it.get_str(&hv, "get")?;
        it.call(&get, hv, args)
    });
    it.define_method(&proto, "getAllResponseHeaders", 0, |it, this, _| {
        let raw = it.get_str(&this, "__rawHeaders")?;
        Ok(if raw.is_undefined() { s("") } else { raw })
    });
    it.define_method(&proto, "send", 1, |it, this, args| {
        let method = it.get_str(&this, "__method")?;
        let method = it.to_str(&method)?.to_string();
        let url_v = it.get_str(&this, "__url")?;
        let url_s = it.to_str(&url_v)?.to_string();
        let Some(url) = ph(it).resolve_url(&url_s) else { throw!(it, "SyntaxError", "Invalid URL") };
        let hv = it.get_str(&this, "__headers")?;
        let mut headers = Vec::new();
        if let Value::Obj(ho) = &hv {
            for k in it.own_keys(ho, false) {
                let v = it.get(&hv, &k)?;
                headers.push((k.as_str().unwrap_or("").to_string(), it.to_str(&v)?.to_string()));
            }
        }
        let body = body_bytes(it, &arg(args, 0), &mut headers)?;
        for h in ph(it).request_headers(&method, &url) {
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(&h.0)) {
                headers.push(h);
            }
        }
        let req = Request { method, url, headers, body };
        let is_async = it.get_str(&this, "__async")?.truthy();
        let xhr = this.clone();
        let complete = it.native("", 1, move |it, _, args| {
            let r = arg(args, 0);
            xhr_complete(it, &xhr, &r)
        });
        let tgt = events::target_of(it, &this);
        events::fire(it, tgt, "loadstart", "ProgressEvent", EventInit::default());
        if is_async {
            spawn_fetch(it, req, complete);
        } else {
            let r = net::fetch(&req);
            let v = match r {
                Ok(resp) => response_obj(it, &resp),
                Err(e) => s(e),
            };
            it.call(&complete, Value::Undefined, &[v])?;
        }
        Ok(Value::Undefined)
    });
}

fn xhr_complete(it: &mut Interp, xhr: &Value, r: &Value) -> R<Value> {
    let Value::Obj(o) = xhr else { return Ok(Value::Undefined) };
    if o.get_own("__aborted").is_some_and(|v| v.truthy()) {
        return Ok(Value::Undefined);
    }
    let target = events::target_of(it, xhr);
    ph(it).listener_objects.insert(o.id(), xhr.clone());
    let fire = |it: &mut Interp, ty: &str| {
        events::fire(it, target, ty, "ProgressEvent", EventInit::default());
    };
    if let Value::Str(_) = r {
        o.set_prop("readyState", Value::Num(4.0));
        o.set_prop("status", Value::Num(0.0));
        fire(it, "readystatechange");
        fire(it, "error");
        fire(it, "loadend");
        return Ok(Value::Undefined);
    }
    let status = it.get_str(r, "status")?;
    let st_text = it.get_str(r, "statusText")?;
    let url = it.get_str(r, "url")?;
    let hdrs = it.get_str(r, "headers")?;
    let text_fn = it.get_str(r, "text")?;
    let text_p = it.call(&text_fn, r.clone(), &[])?;
    let text = promise::state_of(&text_p).map(|(_, v)| v).unwrap_or(s(""));
    o.set_prop("status", status);
    o.set_prop("statusText", st_text);
    o.set_prop("responseURL", url);
    o.set_hidden("__respHeaders", hdrs.clone());
    // raw header string
    let mut raw = String::new();
    let ent = it.get_str(&hdrs, "entries")?;
    let iter = it.call(&ent, hdrs, &[])?;
    for pair in it.iterate_to_vec(&iter)? {
        let k = it.get_str(&pair, "0")?;
        let v = it.get_str(&pair, "1")?;
        raw.push_str(&format!("{}: {}\r\n", it.to_string_lossy(&k), it.to_string_lossy(&v)));
    }
    o.set_hidden("__rawHeaders", s(raw));
    o.set_prop("responseText", text.clone());
    let rt = it.get_str(xhr, "responseType")?;
    let rt = it.to_string_lossy(&rt);
    let response = if rt == "json" {
        let t = it.to_str(&text)?;
        crate::js::builtins::json::parse(it, &t).unwrap_or(Value::Null)
    } else {
        text
    };
    o.set_prop("response", response);
    o.set_prop("readyState", Value::Num(4.0));
    fire(it, "readystatechange");
    fire(it, "load");
    fire(it, "loadend");
    Ok(Value::Undefined)
}

// ---------------------------------------------------------------- URL / URLSearchParams / FormData

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.trim_start_matches('?')
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            let d = |x: &str| net::url::percent_decode(&x.replace('+', " "));
            (d(k), d(v))
        })
        .collect()
}

fn serialize_query(list: &[(String, String)]) -> String {
    list.iter().map(|(k, v)| format!("{}={}", net::url::form_encode(k), net::url::form_encode(v))).collect::<Vec<_>>().join("&")
}

fn params_list(it: &mut Interp, this: &Value) -> R<Vec<(String, String)>> {
    let l = it.get_str(this, "__list")?;
    let mut out = Vec::new();
    for pair in it.iterate_to_vec(&l)? {
        let k = it.get_str(&pair, "0")?;
        let v = it.get_str(&pair, "1")?;
        out.push((it.to_str(&k)?.to_string(), it.to_str(&v)?.to_string()));
    }
    Ok(out)
}

fn set_params_list(it: &mut Interp, this: &Value, list: &[(String, String)]) -> R<()> {
    let items: Vec<Value> = list.iter().map(|(k, v)| it.new_array(vec![s(k), s(v)])).collect();
    let arr = it.new_array(items);
    if let Value::Obj(o) = this {
        o.set_hidden("__list", arr);
        // propagate to the owning URL
        if let Some(Value::Obj(owner)) = o.get_own("__owner") {
            let q = serialize_query(list);
            let href = owner.get_own("href").map(|h| it.to_string_lossy(&h)).unwrap_or_default();
            if let Some(mut u) = Url::parse(&href) {
                u.query = if q.is_empty() { None } else { Some(q) };
                fill_url(it, &owner, &u);
            }
        }
    }
    Ok(())
}

fn fill_url(it: &mut Interp, o: &Obj, u: &Url) {
    o.set_prop("href", s(u.to_string()));
    o.set_prop("protocol", s(format!("{}:", u.scheme)));
    o.set_prop("host", s(u.host_port()));
    o.set_prop("hostname", s(&u.host));
    o.set_prop("port", s(u.port.map(|p| p.to_string()).unwrap_or_default()));
    o.set_prop("pathname", s(&u.path));
    o.set_prop("search", s(u.search()));
    o.set_prop("hash", s(u.hash()));
    o.set_prop("origin", s(u.origin()));
    o.set_prop("username", s(&u.username));
    o.set_prop("password", s(&u.password));
    let _ = it;
}

fn url_setup(it: &mut Interp) {
    // URLSearchParams
    let spp = it.new_object();
    let sp_ctor = it.define_ctor("URLSearchParams", 1, &spp, |it, this, args| {
        it.constructing = false;
        let init = arg(args, 0);
        let list = match &init {
            Value::Undefined | Value::Null => vec![],
            Value::Str(q) => parse_query(q),
            Value::Obj(o) => {
                if o.get_own("__list").is_some() {
                    params_list(it, &init)?
                } else if o.is_array() || !it.get(&init, &PropKey::Sym(it.sym_iterator.clone()))?.is_undefined() {
                    let mut out = Vec::new();
                    for pair in it.iterate_to_vec(&init)? {
                        let k = it.get_str(&pair, "0")?;
                        let v = it.get_str(&pair, "1")?;
                        out.push((it.to_str(&k)?.to_string(), it.to_str(&v)?.to_string()));
                    }
                    out
                } else {
                    let mut out = Vec::new();
                    for k in it.own_keys(o, false) {
                        let v = it.get(&init, &k)?;
                        out.push((k.as_str().unwrap_or("").to_string(), it.to_str(&v)?.to_string()));
                    }
                    out
                }
            }
            v => parse_query(&it.to_str(v)?),
        };
        set_params_list(it, &this, &list)?;
        Ok(this)
    });
    let _ = sp_ctor;
    it.define_method(&spp, "get", 1, |it, this, args| {
        let k = it.to_str(&arg(args, 0))?.to_string();
        Ok(params_list(it, &this)?.into_iter().find(|(kk, _)| *kk == k).map(|(_, v)| s(v)).unwrap_or(Value::Null))
    });
    it.define_method(&spp, "getAll", 1, |it, this, args| {
        let k = it.to_str(&arg(args, 0))?.to_string();
        let v: Vec<Value> = params_list(it, &this)?.into_iter().filter(|(kk, _)| *kk == k).map(|(_, v)| s(v)).collect();
        Ok(it.new_array(v))
    });
    it.define_method(&spp, "has", 1, |it, this, args| {
        let k = it.to_str(&arg(args, 0))?.to_string();
        Ok(Value::Bool(params_list(it, &this)?.iter().any(|(kk, _)| *kk == k)))
    });
    it.define_method(&spp, "set", 2, |it, this, args| {
        let k = it.to_str(&arg(args, 0))?.to_string();
        let v = it.to_str(&arg(args, 1))?.to_string();
        let mut l = params_list(it, &this)?;
        let mut done = false;
        l.retain_mut(|(kk, vv)| {
            if *kk == k {
                if done {
                    return false;
                }
                *vv = v.clone();
                done = true;
            }
            true
        });
        if !done {
            l.push((k, v));
        }
        set_params_list(it, &this, &l)?;
        Ok(Value::Undefined)
    });
    it.define_method(&spp, "append", 2, |it, this, args| {
        let k = it.to_str(&arg(args, 0))?.to_string();
        let v = it.to_str(&arg(args, 1))?.to_string();
        let mut l = params_list(it, &this)?;
        l.push((k, v));
        set_params_list(it, &this, &l)?;
        Ok(Value::Undefined)
    });
    it.define_method(&spp, "delete", 1, |it, this, args| {
        let k = it.to_str(&arg(args, 0))?.to_string();
        let mut l = params_list(it, &this)?;
        l.retain(|(kk, _)| *kk != k);
        set_params_list(it, &this, &l)?;
        Ok(Value::Undefined)
    });
    it.define_method(&spp, "sort", 0, |it, this, _| {
        let mut l = params_list(it, &this)?;
        l.sort_by(|a, b| a.0.cmp(&b.0));
        set_params_list(it, &this, &l)?;
        Ok(Value::Undefined)
    });
    it.define_method(&spp, "toString", 0, |it, this, _| Ok(s(serialize_query(&params_list(it, &this)?))));
    let ts = spp.get_own("toString").unwrap();
    spp.set_hidden("__tipSerialize", ts);
    it.define_getter(&spp, "size", |it, this, _| Ok(Value::Num(params_list(it, &this)?.len() as f64)));
    it.define_method(&spp, "forEach", 1, |it, this, args| {
        let f = arg(args, 0);
        for (k, v) in params_list(it, &this)? {
            it.call(&f, Value::Undefined, &[s(v), s(k), this.clone()])?;
        }
        Ok(Value::Undefined)
    });
    for (name, kind) in [("entries", 2), ("keys", 0), ("values", 1)] {
        it.define_method(&spp, name, 0, move |it, this, _| {
            let items: Vec<Value> = params_list(it, &this)?
                .into_iter()
                .map(|(k, v)| match kind {
                    0 => s(k),
                    1 => s(v),
                    _ => it.new_array(vec![s(k), s(v)]),
                })
                .collect();
            Ok(crate::js::builtins::make_list_iterator(it, items))
        });
    }
    let ent = spp.get_own("entries").unwrap();
    let si = it.sym_iterator.clone();
    spp.borrow_mut().props.insert(PropKey::Sym(si), Prop::hidden(ent));

    // URL
    let up = it.new_object();
    let uctor = it.define_ctor("URL", 2, &up, |it, this, args| {
        it.constructing = false;
        let input = it.to_str(&arg(args, 0))?.to_string();
        let base = arg(args, 1);
        let u = if base.is_undefined() {
            Url::parse(&input)
        } else {
            let b = it.to_str(&base)?.to_string();
            Url::parse(&b).and_then(|bu| bu.join(&input))
        };
        let Some(u) = u else { throw!(it, "TypeError", "Failed to construct 'URL': Invalid URL '{input}'") };
        let Value::Obj(o) = &this else { unreachable!() };
        fill_url(it, o, &u);
        let spc = it.global.get_own("URLSearchParams").unwrap();
        let sp = it.construct(&spc, &[s(u.query.clone().unwrap_or_default())], None)?;
        if let Value::Obj(spo) = &sp {
            spo.set_hidden("__owner", this.clone());
        }
        o.set_prop("searchParams", sp);
        Ok(this)
    });
    it.define_method(&uctor, "canParse", 2, |it, _, args| {
        let input = it.to_str(&arg(args, 0))?.to_string();
        Ok(Value::Bool(Url::parse(&input).is_some()))
    });
    it.define_method(&uctor, "createObjectURL", 1, |_, _, _| Ok(s("blob:tip/unsupported")));
    it.define_method(&uctor, "revokeObjectURL", 1, |_, _, _| Ok(Value::Undefined));
    it.define_method(&up, "toString", 0, |it, this, _| it.get_str(&this, "href"));
    it.define_method(&up, "toJSON", 0, |it, this, _| it.get_str(&this, "href"));

    // FormData
    let fdp = it.new_object();
    it.define_ctor("FormData", 1, &fdp, |it, this, args| {
        it.constructing = false;
        let mut list = Vec::new();
        if let Some(f) = node_of(&arg(args, 0)) {
            list = crate::input::form_entries(it, f, None);
        }
        set_params_list(it, &this, &list)?;
        Ok(this)
    });
    for name in ["get", "getAll", "has", "set", "append", "delete", "entries", "keys", "values", "forEach", "toString", "__tipSerialize"] {
        if let Some(f) = spp.get_own(name) {
            fdp.set_hidden(name, f);
        }
    }
    let ent = spp.get_own("entries").unwrap();
    let si = it.sym_iterator.clone();
    fdp.borrow_mut().props.insert(PropKey::Sym(si), Prop::hidden(ent));
}

// ---------------------------------------------------------------- observers

fn observers_setup(it: &mut Interp) {
    // MutationObserver: notified (coalesced) after tasks that changed the observed subtree.
    let mop = it.new_object();
    it.define_ctor("MutationObserver", 1, &mop, |it, this, args| {
        it.constructing = false;
        if let Value::Obj(o) = &this {
            o.set_hidden("__cb", arg(args, 0));
        }
        Ok(this)
    });
    it.define_method(&mop, "observe", 2, |it, this, args| {
        let Some(n) = node_of(&arg(args, 0)) else { return Ok(Value::Undefined) };
        let cb = it.get_str(&this, "__cb")?;
        let h = ph(it);
        let v = h.doc.version;
        h.mutation_observers.push(crate::page::MutationObs { observer: this.clone(), callback: cb, target: n, version: v });
        Ok(Value::Undefined)
    });
    it.define_method(&mop, "disconnect", 0, |it, this, _| {
        ph(it).mutation_observers.retain(|m| !crate::js::interp::strict_equals(&m.observer, &this));
        Ok(Value::Undefined)
    });
    it.define_method(&mop, "takeRecords", 0, |it, _, _| Ok(it.new_array(vec![])));

    for name in ["IntersectionObserver", "ResizeObserver"] {
        let pr = it.new_object();
        it.define_ctor(name, 1, &pr, |it, this, args| {
            it.constructing = false;
            if let Value::Obj(o) = &this {
                o.set_hidden("__cb", arg(args, 0));
            }
            Ok(this)
        });
        let is_io = name == "IntersectionObserver";
        it.define_method(&pr, "observe", 1, move |it, this, args| {
            let target = arg(args, 0);
            let Some(n) = node_of(&target) else { return Ok(Value::Undefined) };
            let cb = it.get_str(&this, "__cb")?;
            // Report the element as visible/sized on the next tick.
            let rect = super::client_rect(it, n);
            let entry = it.new_object();
            entry.set_prop("target", target.clone());
            entry.set_prop("boundingClientRect", rect.clone());
            entry.set_prop("contentRect", rect.clone());
            entry.set_prop("intersectionRect", rect);
            entry.set_prop("isIntersecting", Value::Bool(true));
            entry.set_prop("intersectionRatio", Value::Num(1.0));
            entry.set_prop("time", Value::Num(perf_now()));
            let _ = is_io;
            let entries = it.new_array(vec![Value::Obj(entry)]);
            let obs = this.clone();
            let job = it.native("", 0, move |it, _, _| it.call(&cb, obs.clone(), &[entries.clone(), obs.clone()]));
            add_timer(it, job, vec![], 0.0, false);
            Ok(Value::Undefined)
        });
        for m in ["unobserve", "disconnect"] {
            it.define_method(&pr, m, 0, |_, _, _| Ok(Value::Undefined));
        }
        it.define_method(&pr, "takeRecords", 0, |it, _, _| Ok(it.new_array(vec![])));
    }
}

// ---------------------------------------------------------------- timers

pub fn add_timer(it: &mut Interp, cb: Value, args: Vec<Value>, ms: f64, repeat: bool) -> u32 {
    let h = ph(it);
    let id = h.next_timer;
    h.next_timer += 1;
    let d = Duration::from_micros((ms.max(0.0) * 1000.0) as u64);
    h.timers.push(Timer { id, due: Instant::now() + d, interval: if repeat { Some(d) } else { None }, callback: cb, args });
    id
}

// ---------------------------------------------------------------- install

pub fn install(it: &mut Interp) {
    let g = it.global.clone();
    let gv = Value::Obj(g.clone());
    for name in ["window", "self", "top", "parent", "frames"] {
        g.set_hidden(name, gv.clone());
    }
    for (k, v) in [("opener", Value::Null), ("frameElement", Value::Null), ("closed", Value::Bool(false)), ("name", s("")), ("status", s("")), ("isSecureContext", Value::Bool(true)), ("length", Value::Num(0.0))] {
        g.set_hidden(k, v);
    }
    let origin = ph(it).url.origin();
    g.set_hidden("origin", s(&origin));

    // geometry
    it.define_getter(&g, "innerWidth", |it, _, _| Ok(Value::Num(ph(it).viewport.0 as f64)));
    it.define_getter(&g, "innerHeight", |it, _, _| Ok(Value::Num(ph(it).viewport.1 as f64)));
    it.define_getter(&g, "outerWidth", |it, _, _| Ok(Value::Num(ph(it).viewport.0 as f64)));
    it.define_getter(&g, "outerHeight", |it, _, _| Ok(Value::Num(ph(it).viewport.1 as f64 + 80.0)));
    it.define_getter(&g, "devicePixelRatio", |it, _, _| Ok(Value::Num(ph(it).scale as f64)));
    for name in ["scrollX", "pageXOffset"] {
        it.define_getter(&g, name, |it, _, _| Ok(Value::Num(ph(it).scroll.0 as f64)));
    }
    for name in ["scrollY", "pageYOffset"] {
        it.define_getter(&g, name, |it, _, _| Ok(Value::Num(ph(it).scroll.1 as f64)));
    }
    for name in ["scrollTo", "scroll", "scrollBy"] {
        it.define_method(&g, name, 2, move |it, _, args| {
            let (x, y) = scroll_args(it, args)?;
            let h = ph(it);
            if name == "scrollBy" {
                h.scroll.0 += x.unwrap_or(0.0);
                h.scroll.1 += y.unwrap_or(0.0);
            } else {
                if let Some(x) = x {
                    h.scroll.0 = x;
                }
                if let Some(y) = y {
                    h.scroll.1 = y;
                }
            }
            h.clamp_scroll();
            Ok(Value::Undefined)
        });
    }

    // timers
    for (name, repeat) in [("setTimeout", false), ("setInterval", true)] {
        it.define_method(&g, name, 2, move |it, _, args| {
            let cb = arg(args, 0);
            let ms = if args.len() > 1 { it.to_number(&args[1])?.max(0.0) } else { 0.0 };
            let ms = if ms.is_nan() { 0.0 } else { ms };
            let extra = if args.len() > 2 { args[2..].to_vec() } else { vec![] };
            let cb = if cb.is_callable() { cb } else { Value::Str(it.to_str(&cb)?) };
            Ok(Value::Num(add_timer(it, cb, extra, ms, repeat) as f64))
        });
    }
    for name in ["clearTimeout", "clearInterval"] {
        it.define_method(&g, name, 1, |it, _, args| {
            let id = it.to_number(&arg(args, 0))? as u32;
            ph(it).timers.retain(|t| t.id != id);
            Ok(Value::Undefined)
        });
    }
    it.define_method(&g, "requestAnimationFrame", 1, |it, _, args| {
        let h = ph(it);
        let id = h.next_timer;
        h.next_timer += 1;
        h.raf.push((id, arg(args, 0)));
        Ok(Value::Num(id as f64))
    });
    it.define_method(&g, "cancelAnimationFrame", 1, |it, _, args| {
        let id = it.to_number(&arg(args, 0))? as u32;
        ph(it).raf.retain(|(i, _)| *i != id);
        Ok(Value::Undefined)
    });
    it.define_method(&g, "requestIdleCallback", 1, |it, _, args| {
        let cb = arg(args, 0);
        let deadline = it.new_object();
        let tr = it.native("timeRemaining", 0, |_, _, _| Ok(Value::Num(16.0)));
        deadline.set_hidden("timeRemaining", tr);
        deadline.set_prop("didTimeout", Value::Bool(false));
        Ok(Value::Num(add_timer(it, cb, vec![Value::Obj(deadline)], 1.0, false) as f64))
    });
    it.define_method(&g, "cancelIdleCallback", 1, |it, _, args| {
        let id = it.to_number(&arg(args, 0))? as u32;
        ph(it).timers.retain(|t| t.id != id);
        Ok(Value::Undefined)
    });

    // dialogs
    it.define_method(&g, "alert", 1, |it, _, args| {
        let m = it.to_string_lossy(&arg(args, 0));
        let h = ph(it);
        h.alerts.push(m.clone());
        h.log("info", format!("[alert] {m}"));
        Ok(Value::Undefined)
    });
    it.define_method(&g, "confirm", 1, |it, _, args| {
        let m = it.to_string_lossy(&arg(args, 0));
        let h = ph(it);
        h.alerts.push(m.clone());
        h.log("info", format!("[confirm] {m} → OK"));
        Ok(Value::Bool(h.dialog_answer.unwrap_or(true)))
    });
    it.define_method(&g, "prompt", 2, |it, _, args| {
        let m = it.to_string_lossy(&arg(args, 0));
        let d = arg(args, 1);
        let h = ph(it);
        h.alerts.push(m.clone());
        h.log("info", format!("[prompt] {m}"));
        Ok(if d.is_undefined() { s("") } else { d })
    });
    for name in ["print", "focus", "blur", "stop", "moveTo", "resizeTo", "close"] {
        it.define_method(&g, name, 0, |_, _, _| Ok(Value::Undefined));
    }
    it.define_method(&g, "open", 1, |it, _, args| {
        let u = it.to_string_lossy(&arg(args, 0));
        let h = ph(it);
        if let Some(url) = h.resolve_url(&u) {
            h.nav = Some(NavRequest::Open(url));
        }
        Ok(Value::Null)
    });
    it.define_method(&g, "postMessage", 2, |it, _, args| {
        let data = arg(args, 0);
        let job = it.native("", 0, move |it, _, _| {
            let init = EventInit { bubbles: false, cancelable: false, props: vec![("data", data.clone()), ("origin", s(ph(it).url.origin())), ("source", Value::Obj(it.global.clone()))] };
            events::fire(it, Target::Window, "message", "MessageEvent", init);
            Ok(Value::Undefined)
        });
        add_timer(it, job, vec![], 0.0, false);
        Ok(Value::Undefined)
    });

    // style queries
    it.define_method(&g, "getComputedStyle", 1, |it, _, args| {
        let Some(n) = node_of(&arg(args, 0)) else { throw!(it, "TypeError", "Failed to execute 'getComputedStyle': parameter 1 is not of type 'Element'.") };
        let proto = it.global.get_own("CSSStyleDeclaration").and_then(|c| c.as_obj().and_then(|c| c.get_own("prototype"))).and_then(|p| p.as_obj().cloned());
        Ok(Value::Obj(Obj::new(ObjKind::Host(Rc::new(StyleHost(n, true))), proto, "CSSStyleDeclaration")))
    });
    it.define_method(&g, "matchMedia", 1, |it, _, args| {
        let q = it.to_str(&arg(args, 0))?.to_string();
        let (vw, vh) = ph(it).viewport;
        let sheet = crate::css::parse_stylesheet(&format!("@media {q} {{ a {{ b: c }} }}"));
        let matches = sheet.rules.first().and_then(|r| r.media.as_ref()).map(|m| m.matches(vw, vh)).unwrap_or(false);
        let o = it.new_object();
        o.set_prop("matches", Value::Bool(matches));
        o.set_prop("media", s(&q));
        o.set_prop("onchange", Value::Null);
        for m in ["addListener", "removeListener", "addEventListener", "removeEventListener"] {
            let f = it.native(m, 1, |_, _, _| Ok(Value::Undefined));
            o.set_hidden(m, f);
        }
        Ok(Value::Obj(o))
    });

    // location & history
    let loc = Obj::new(ObjKind::Host(Rc::new(LocationHost)), Some(it.p.object.clone()), "Location");
    it.define_method(&loc, "assign", 1, |it, _, args| {
        let u = it.to_string_lossy(&arg(args, 0));
        let h = ph(it);
        if let Some(url) = h.resolve_url(&u) {
            navigate(h, url, false);
        }
        Ok(Value::Undefined)
    });
    it.define_method(&loc, "replace", 1, |it, _, args| {
        let u = it.to_string_lossy(&arg(args, 0));
        let h = ph(it);
        if let Some(url) = h.resolve_url(&u) {
            navigate(h, url, true);
        }
        Ok(Value::Undefined)
    });
    it.define_method(&loc, "reload", 0, |it, _, _| {
        ph(it).nav = Some(NavRequest::Reload);
        Ok(Value::Undefined)
    });
    it.define_method(&loc, "toString", 0, |it, _, _| Ok(s(ph(it).url.to_string())));
    g.set_hidden("location", Value::Obj(loc));

    let hist = it.new_object();
    it.define_getter(&hist, "length", |it, _, _| Ok(Value::Num(ph(it).history_len as f64)));
    it.define_getter(&hist, "state", |it, _, _| Ok(ph(it).history_state.clone()));
    hist.set_prop("scrollRestoration", s("auto"));
    it.define_method(&hist, "back", 0, |it, _, _| {
        ph(it).nav = Some(NavRequest::History(-1));
        Ok(Value::Undefined)
    });
    it.define_method(&hist, "forward", 0, |it, _, _| {
        ph(it).nav = Some(NavRequest::History(1));
        Ok(Value::Undefined)
    });
    it.define_method(&hist, "go", 1, |it, _, args| {
        let n = it.to_integer(&arg(args, 0))? as i32;
        ph(it).nav = Some(if n == 0 { NavRequest::Reload } else { NavRequest::History(n) });
        Ok(Value::Undefined)
    });
    for (name, replace) in [("pushState", false), ("replaceState", true)] {
        it.define_method(&hist, name, 3, move |it, _, args| {
            let state = arg(args, 0);
            let u = arg(args, 2);
            let h = ph(it);
            h.history_state = state;
            if !u.is_nullish() {
                let us = match &u {
                    Value::Str(x) => x.to_string(),
                    _ => String::new(),
                };
                let us = if us.is_empty() { it.to_string_lossy(&u) } else { us };
                let h = ph(it);
                if let Some(url) = h.resolve_url(&us) {
                    h.url = url.clone();
                    h.nav = Some(NavRequest::SameDocument { url, replace });
                    h.suppress_hashchange = true;
                }
            }
            Ok(Value::Undefined)
        });
    }
    g.set_hidden("history", Value::Obj(hist));

    // navigator & screen
    let nav = it.new_object();
    nav.set_prop("userAgent", s(net::USER_AGENT));
    nav.set_prop("appName", s("Netscape"));
    nav.set_prop("appCodeName", s("Mozilla"));
    nav.set_prop("appVersion", s("5.0 (Tip)"));
    nav.set_prop("product", s("Gecko"));
    nav.set_prop("vendor", s("Tip"));
    nav.set_prop("platform", s(if cfg!(target_os = "macos") { "MacIntel" } else if cfg!(windows) { "Win32" } else { "Linux x86_64" }));
    nav.set_prop("language", s("en-US"));
    let langs = it.new_array(vec![s("en-US"), s("en")]);
    nav.set_prop("languages", langs);
    nav.set_prop("onLine", Value::Bool(true));
    nav.set_prop("cookieEnabled", Value::Bool(true));
    nav.set_prop("doNotTrack", Value::Null);
    nav.set_prop("hardwareConcurrency", Value::Num(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as f64));
    nav.set_prop("maxTouchPoints", Value::Num(0.0));
    nav.set_prop("webdriver", Value::Bool(std::env::var("TIP_AUTOMATION").is_ok()));
    nav.set_prop("pdfViewerEnabled", Value::Bool(false));
    let clip = it.new_object();
    it.define_method(&clip, "writeText", 1, |it, _, args| {
        let t = it.to_string_lossy(&arg(args, 0));
        ph(it).clipboard = Some(t);
        promise::promise_resolve(it, Value::Undefined)
    });
    it.define_method(&clip, "readText", 0, |it, _, _| {
        let t = ph(it).clipboard.clone().unwrap_or_default();
        promise::promise_resolve(it, s(t))
    });
    nav.set_prop("clipboard", Value::Obj(clip));
    it.define_method(&nav, "sendBeacon", 2, |it, _, args| {
        let u = it.to_string_lossy(&arg(args, 0));
        let Some(url) = ph(it).resolve_url(&u) else { throw!(it, "TypeError", "Failed to execute 'sendBeacon' on 'Navigator': Invalid URL '{u}'") };
        if !matches!(url.scheme.as_str(), "http" | "https") {
            return Ok(Value::Bool(false));
        }
        let mut headers = ph(it).request_headers("POST", &url);
        let body = body_bytes(it, &arg(args, 1), &mut headers)?;
        if body.as_ref().is_some_and(|b| b.len() > 64 * 1024) {
            return Ok(Value::Bool(false));
        }
        let req = Request { method: "POST".into(), url, headers, body: Some(body.unwrap_or_default()) };
        // fire and forget; the page may be unloading
        std::thread::spawn(move || {
            let _ = net::fetch(&req);
        });
        Ok(Value::Bool(true))
    });
    it.define_method(&nav, "vibrate", 1, |_, _, _| Ok(Value::Bool(false)));
    g.set_hidden("navigator", Value::Obj(nav));
    let screen = it.new_object();
    for (k, v) in [("width", 1920.0), ("height", 1080.0), ("availWidth", 1920.0), ("availHeight", 1050.0), ("colorDepth", 24.0), ("pixelDepth", 24.0)] {
        screen.set_prop(k, Value::Num(v));
    }
    g.set_hidden("screen", Value::Obj(screen));

    // storage
    let sproto = it.new_object();
    it.define_method(&sproto, "getItem", 1, |it, this, args| {
        let h = storage_of(it, &this)?;
        let st = h.as_any().downcast_ref::<StorageHost>().unwrap();
        let k = it.to_string_lossy(&arg(args, 0));
        Ok(st.items().into_iter().find(|(kk, _)| *kk == k).map(|(_, v)| s(v)).unwrap_or(Value::Null))
    });
    it.define_method(&sproto, "setItem", 2, |it, this, args| {
        let h = storage_of(it, &this)?;
        let st = h.as_any().downcast_ref::<StorageHost>().unwrap();
        let k = it.to_string_lossy(&arg(args, 0));
        let v = it.to_string_lossy(&arg(args, 1));
        st.set_item(&k, &v);
        Ok(Value::Undefined)
    });
    it.define_method(&sproto, "removeItem", 1, |it, this, args| {
        let h = storage_of(it, &this)?;
        let st = h.as_any().downcast_ref::<StorageHost>().unwrap();
        let k = it.to_string_lossy(&arg(args, 0));
        st.remove(Some(&k));
        Ok(Value::Undefined)
    });
    it.define_method(&sproto, "clear", 0, |it, this, _| {
        let h = storage_of(it, &this)?;
        h.as_any().downcast_ref::<StorageHost>().unwrap().remove(None);
        Ok(Value::Undefined)
    });
    it.define_method(&sproto, "key", 1, |it, this, args| {
        let h = storage_of(it, &this)?;
        let st = h.as_any().downcast_ref::<StorageHost>().unwrap();
        let i = it.to_integer(&arg(args, 0))? as usize;
        Ok(st.items().get(i).map(|(k, _)| s(k)).unwrap_or(Value::Null))
    });
    for (name, local) in [("localStorage", true), ("sessionStorage", false)] {
        let o = Obj::new(ObjKind::Host(Rc::new(StorageHost { local, origin: origin.clone() })), Some(sproto.clone()), "Storage");
        g.set_hidden(name, Value::Obj(o));
    }

    // performance
    let perf = it.new_object();
    it.define_method(&perf, "now", 0, |_, _, _| Ok(Value::Num(perf_now())));
    let origin_ms = crate::js::builtins::date::now_ms() - perf_now();
    perf.set_prop("timeOrigin", Value::Num(origin_ms));
    for name in ["mark", "measure", "clearMarks", "clearMeasures"] {
        it.define_method(&perf, name, 0, |_, _, _| Ok(Value::Undefined));
    }
    for name in ["getEntries", "getEntriesByType", "getEntriesByName"] {
        it.define_method(&perf, name, 0, |it, _, _| Ok(it.new_array(vec![])));
    }
    let timing = it.new_object();
    timing.set_prop("navigationStart", Value::Num(origin_ms));
    perf.set_prop("timing", Value::Obj(timing));
    g.set_hidden("performance", Value::Obj(perf));

    // base64 & crypto
    it.define_method(&g, "btoa", 1, |it, _, args| {
        let st = it.to_str(&arg(args, 0))?;
        let mut bytes = Vec::new();
        for c in st.chars() {
            if c as u32 > 255 {
                throw!(it, "InvalidCharacterError", "Failed to execute 'btoa': The string to be encoded contains characters outside of the Latin1 range.");
            }
            bytes.push(c as u8);
        }
        Ok(s(net::base64_encode(&bytes)))
    });
    it.define_method(&g, "atob", 1, |it, _, args| {
        let st = it.to_str(&arg(args, 0))?;
        match net::base64_decode(&st) {
            Some(b) => Ok(s(b.iter().map(|&x| x as char).collect::<String>())),
            None => throw!(it, "InvalidCharacterError", "Failed to execute 'atob': The string to be decoded is not correctly encoded."),
        }
    });
    let crypto = it.new_object();
    it.define_method(&crypto, "getRandomValues", 1, |it, _, args| {
        let a = arg(args, 0);
        let len = it.get_str(&a, "length")?;
        let n = it.to_length(&len)?;
        for i in 0..n.min(65536) {
            let v = (crate::js::builtins::number::random() * 4294967296.0).floor();
            it.set(&a, PropKey::Str(Rc::from(i.to_string().as_str())), Value::Num(v))?;
        }
        Ok(a)
    });
    it.define_method(&crypto, "randomUUID", 0, |_, _, _| {
        let mut b = [0u8; 16];
        for x in b.iter_mut() {
            *x = (crate::js::builtins::number::random() * 256.0) as u8;
        }
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
        Ok(s(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])))
    });
    g.set_hidden("crypto", Value::Obj(crypto));

    // networking
    it.define_method(&g, "fetch", 2, |it, _, args| do_fetch(it, args));
    xhr_setup(it);
    url_setup(it);
    observers_setup(it);

    // AbortController
    let acp = it.new_object();
    it.define_ctor("AbortController", 0, &acp, |it, this, _| {
        it.constructing = false;
        let sig = it.new_object();
        sig.set_prop("aborted", Value::Bool(false));
        sig.set_prop("reason", Value::Undefined);
        sig.set_prop("onabort", Value::Null);
        let et = it.global.get_own("EventTarget").and_then(|c| c.as_obj().and_then(|c| c.get_own("prototype")));
        if let Some(Value::Obj(e)) = et {
            sig.borrow_mut().proto = Some(e);
        }
        if let Value::Obj(o) = &this {
            o.set_prop("signal", Value::Obj(sig));
        }
        Ok(this)
    });
    it.define_method(&acp, "abort", 1, |it, this, args| {
        let sig = it.get_str(&this, "signal")?;
        if let Value::Obj(so) = &sig {
            so.set_prop("aborted", Value::Bool(true));
            so.set_prop("reason", arg(args, 0));
            let t = events::target_of(it, &sig);
            ph(it).listener_objects.insert(so.id(), sig.clone());
            events::fire(it, t, "abort", "Event", EventInit::default());
        }
        Ok(Value::Undefined)
    });

    // element constructors
    let img_ctor = it.native("Image", 2, |it, _, args| {
        let n = ph(it).doc.create_element("img");
        if !arg(args, 0).is_undefined() {
            let w = it.to_string_lossy(&arg(args, 0));
            ph(it).doc.set_attr(n, "width", &w);
        }
        if !arg(args, 1).is_undefined() {
            let hgt = it.to_string_lossy(&arg(args, 1));
            ph(it).doc.set_attr(n, "height", &hgt);
        }
        Ok(wrap(it, n))
    });
    if let Value::Obj(io) = &img_ctor {
        if let ObjKind::Function(Func::Native { ctor, .. }) = &mut io.borrow_mut().kind {
            *ctor = true;
        }
        if let Some(p) = it.global.get_own("HTMLImageElement").and_then(|c| c.as_obj().and_then(|c| c.get_own("prototype"))) {
            io.set_hidden("prototype", p);
        }
    }
    g.set_hidden("Image", img_ctor);
    let opt_ctor = it.native("Option", 4, |it, _, args| {
        let n = ph(it).doc.create_element("option");
        let text = if arg(args, 0).is_undefined() { String::new() } else { it.to_string_lossy(&arg(args, 0)) };
        let h = ph(it);
        h.doc.set_text_content(n, &text);
        if !arg(args, 1).is_undefined() {
            let v = it.to_string_lossy(&arg(args, 1));
            ph(it).doc.set_attr(n, "value", &v);
        }
        Ok(wrap(it, n))
    });
    if let Value::Obj(oo) = &opt_ctor {
        if let ObjKind::Function(Func::Native { ctor, .. }) = &mut oo.borrow_mut().kind {
            *ctor = true;
        }
    }
    g.set_hidden("Option", opt_ctor);

    // EventSource: server-sent events streamed on a background thread
    let esp = it.new_object();
    if let Some(Value::Obj(e)) = it.global.get_own("EventTarget").and_then(|c| c.as_obj().and_then(|c| c.get_own("prototype"))) {
        esp.borrow_mut().proto = Some(e);
    }
    let es_ctor = it.define_ctor("EventSource", 1, &esp, |it, this, args| {
        it.constructing = false;
        let u = it.to_string_lossy(&arg(args, 0));
        let Some(url) = ph(it).resolve_url(&u) else { throw!(it, "SyntaxError", "Failed to construct 'EventSource': The URL '{u}' is invalid.") };
        let with_credentials = match arg(args, 1) {
            v @ Value::Obj(_) => it.get_str(&v, "withCredentials")?.truthy(),
            _ => false,
        };
        if let Value::Obj(o) = &this {
            o.set_prop("url", s(url.to_string()));
            o.set_prop("readyState", Value::Num(0.0));
            o.set_prop("withCredentials", Value::Bool(with_credentials));
            o.set_prop("onmessage", Value::Null);
            o.set_prop("onerror", Value::Null);
            o.set_prop("onopen", Value::Null);
            ph(it).listener_objects.insert(o.id(), this.clone());
            let conn = start_event_source(url, ph(it).url.origin(), this.clone());
            ph(it).event_sources.push(conn);
        }
        Ok(this)
    });
    for (k, v) in [("CONNECTING", 0.0), ("OPEN", 1.0), ("CLOSED", 2.0)] {
        es_ctor.set_hidden(k, Value::Num(v));
        esp.set_hidden(k, Value::Num(v));
    }
    it.define_method(&esp, "close", 0, |it, this, _| {
        if let Value::Obj(o) = &this {
            o.set_prop("readyState", Value::Num(2.0));
            ph(it).event_sources.retain(|c| !matches!(&c.obj, Value::Obj(x) if x.ptr_eq(o)));
        }
        Ok(Value::Undefined)
    });

    // DOMParser
    let dpp = it.new_object();
    it.define_ctor("DOMParser", 0, &dpp, |it, this, _| {
        it.constructing = false;
        Ok(this)
    });
    it.define_method(&dpp, "parseFromString", 2, |it, _, args| {
        let src = it.to_str(&arg(args, 0))?.to_string();
        let parsed = crate::html::parse_document(&src);
        let h = ph(it);
        let frag = h.doc.create_fragment();
        fn import(dst: &mut crate::dom::Document, src: &crate::dom::Document, n: NodeId, parent: NodeId) {
            let id = match &src.node(n).data {
                crate::dom::NodeData::Element(e) => dst.create_element_with_attrs(&e.tag, e.attrs.clone()),
                crate::dom::NodeData::Text(t) => dst.create_text(t),
                crate::dom::NodeData::Comment(t) => dst.create_comment(t),
                _ => return,
            };
            dst.append_child(parent, id);
            for &c in src.children(n) {
                import(dst, src, c, id);
            }
        }
        for &c in parsed.children(0) {
            import(&mut h.doc, &parsed, c, frag);
        }
        let html = h.doc.element_children(frag).first().copied();
        let body = html.and_then(|x| h.doc.find_child_tag(x, "body"));
        let head = html.and_then(|x| h.doc.find_child_tag(x, "head"));
        let title = parsed.title();
        let o = it.new_object();
        let htmlv = wrap_opt(it, html);
        let bodyv = wrap_opt(it, body);
        let headv = wrap_opt(it, head);
        o.set_prop("documentElement", htmlv);
        o.set_prop("body", bodyv);
        o.set_prop("head", headv);
        o.set_prop("title", s(title));
        let fragv = wrap(it, frag);
        for name in ["querySelector", "querySelectorAll", "getElementsByTagName", "getElementsByClassName"] {
            let f = fragv.clone();
            let m = it.native(name, 1, move |it, _, args| {
                let meth = it.get_str(&f, name)?;
                it.call(&meth, f.clone(), args)
            });
            o.set_hidden(name, m);
        }
        let f2 = fragv;
        let gid = it.native("getElementById", 1, move |it, _, args| {
            let id = it.to_str(&arg(args, 0))?;
            let q = it.get_str(&f2, "querySelector")?;
            let sel = format!("[id=\"{}\"]", id.replace('"', "\\\""));
            it.call(&q, f2.clone(), &[s(sel)])
        });
        o.set_hidden("getElementById", gid);
        Ok(Value::Obj(o))
    });

    // CSS namespace
    let cssns = it.new_object();
    it.define_method(&cssns, "supports", 2, |it, _, args| {
        let a = it.to_string_lossy(&arg(args, 0));
        let ok = if args.len() > 1 {
            let v = it.to_string_lossy(&arg(args, 1));
            !crate::css::parse_declarations(&format!("{a}: {v}")).is_empty()
        } else {
            a.contains(':')
        };
        Ok(Value::Bool(ok))
    });
    it.define_method(&cssns, "escape", 1, |it, _, args| {
        let st = it.to_string_lossy(&arg(args, 0));
        Ok(s(st.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c.to_string() } else { format!("\\{c}") }).collect::<String>()))
    });
    g.set_hidden("CSS", Value::Obj(cssns));

    // custom elements (registry only; no upgrades)
    let ce = it.new_object();
    let defs = it.new_object();
    let d1 = defs.clone();
    let define = it.native("define", 2, move |it, _, args| {
        let n = it.to_str(&arg(args, 0))?;
        d1.set_prop(&n, arg(args, 1));
        Ok(Value::Undefined)
    });
    let d2 = defs.clone();
    let get = it.native("get", 1, move |it, _, args| {
        let n = it.to_str(&arg(args, 0))?;
        Ok(d2.get_own(&n).unwrap_or(Value::Undefined))
    });
    let when = it.native("whenDefined", 1, |it, _, _| promise::promise_resolve(it, Value::Undefined));
    ce.set_hidden("define", define);
    ce.set_hidden("get", get);
    ce.set_hidden("whenDefined", when);
    g.set_hidden("customElements", Value::Obj(ce));

    it.define_method(&g, "getSelection", 0, |it, _, _| get_selection(it));
    let _ = node_list;
    let _ = array_like_to_vec;
}
