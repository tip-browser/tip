//! Dedicated workers (`new Worker(url)`).
//!
//! A worker runs in its own JavaScript interpreter with a global scope that has no document:
//! `self`, `postMessage`, `importScripts`, `close`, timers and `fetch`. Messages in both
//! directions are structured-cloned. Workers share the page's thread and are driven from the
//! page's event loop, so a busy worker delays the page (our JS values can't cross threads).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};

use crate::frames::{adopt_port, export, import, transfer_ports, Cloned};

/// A message between a worker and its owner, with any transferred MessagePorts.
pub type WorkerMsg = (Cloned, Vec<(u64, u64)>);

/// `postMessage(data, transfer)` or `postMessage(data, { transfer })`.
fn transfer_arg(it: &mut Interp, args: &[Value]) -> Result<Vec<(u64, u64)>, Value> {
    let t = arg(args, 1);
    let list = match &t {
        Value::Obj(o) if !o.is_array() => it.get_str(&t, "transfer")?,
        _ => t,
    };
    transfer_ports(it, &list)
}
use crate::js::interp::{arg, throw, Interp};
use crate::js::value::Value;
use crate::net::{self, Request, Url};
use crate::page::{ConsoleEntry, LoadOptions, Page, PageHost, Target};

pub struct WorkerCtx {
    pub id: u64,
    url: Url,
    module: bool,
    name: String,
    pub page: Option<Page>,
    pending: Option<Receiver<Result<net::Response, String>>>,
    /// Messages posted before the script arrived.
    queued: Vec<WorkerMsg>,
    /// Messages the worker posted to its owner.
    outbox: Rc<RefCell<Vec<WorkerMsg>>>,
    closed: bool,
}

/// What a worker's own page knows about being a worker.
pub struct WorkerLink {
    pub outbox: Rc<RefCell<Vec<WorkerMsg>>>,
    pub closing: bool,
}

fn host(it: &mut Interp) -> &mut PageHost {
    it.host_mut::<PageHost>().expect("page host")
}

fn next_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Natives behind the prelude's `Worker` class.
pub fn install(it: &mut Interp) {
    let g = it.global.clone();
    it.define_method(&g, "__tip_worker_start", 3, |it, _, args| {
        let src = it.to_string_lossy(&arg(args, 0));
        let module = it.to_string_lossy(&arg(args, 1)) == "module";
        let name = it.to_string_lossy(&arg(args, 2));
        let Some(url) = host(it).resolve_url(&src) else { throw!(it, "SyntaxError", "Failed to construct 'Worker': The URL '{src}' is invalid.") };
        let own = host(it).url.origin();
        if !matches!(url.scheme.as_str(), "blob" | "data") && url.origin() != own && own != "null" {
            throw!(it, "SecurityError", "Failed to construct 'Worker': Script at '{url}' cannot be accessed from origin '{own}'.");
        }
        let id = next_id();
        let (tx, rx) = channel();
        let req = Request::get(url.clone());
        std::thread::spawn(move || {
            let _ = tx.send(net::fetch(&req));
        });
        host(it).workers.push(WorkerCtx { id, url, module, name, page: None, pending: Some(rx), queued: vec![], outbox: Rc::new(RefCell::new(vec![])), closed: false });
        Ok(Value::Num(id as f64))
    });
    it.define_method(&g, "__tip_worker_post", 2, |it, _, args| {
        let id = it.to_number(&arg(args, 0))? as u64;
        let data = export(it, &arg(args, 1))?;
        let ports = transfer_arg(it, &args[1..])?;
        crate::frames::trace("owner -> worker", "owner", &data, ports.len());
        if let Some(w) = host(it).workers.iter_mut().find(|w| w.id == id) {
            match &mut w.page {
                Some(p) => p.host_mut().worker_inbox.push((data, ports)),
                None => w.queued.push((data, ports)),
            }
        }
        Ok(Value::Undefined)
    });
    it.define_method(&g, "__tip_worker_terminate", 1, |it, _, args| {
        let id = it.to_number(&arg(args, 0))? as u64;
        host(it).workers.retain(|w| w.id != id);
        Ok(Value::Undefined)
    });
}

/// Globals of a worker: `self`, `postMessage`, `importScripts`, `close`, `name`; no document.
fn install_worker_globals(page: &mut Page, url: &Url, name: &str) {
    let it = &mut page.interp;
    let g = it.global.clone();
    let gv = Value::Obj(g.clone());
    for k in ["document", "window", "frames", "parent", "top", "opener", "frameElement"] {
        g.borrow_mut().props.remove(&crate::js::value::PropKey::from(k));
    }
    g.set_hidden("self", gv.clone());
    g.set_hidden("globalThis", gv);
    g.set_hidden("name", Value::str(name));
    it.define_method(&g, "postMessage", 2, |it, _, args| {
        let data = export(it, &arg(args, 0))?;
        let ports = transfer_arg(it, args)?;
        crate::frames::trace("worker -> owner", "worker", &data, ports.len());
        if let Some(l) = &host(it).worker_link {
            l.outbox.borrow_mut().push((data, ports));
        }
        Ok(Value::Undefined)
    });
    it.define_method(&g, "close", 0, |it, _, _| {
        if let Some(l) = &mut host(it).worker_link {
            l.closing = true;
        }
        Ok(Value::Undefined)
    });
    it.define_method(&g, "importScripts", 1, |it, _, args| {
        for a in args {
            let s = it.to_string_lossy(a);
            let Some(u) = host(it).resolve_url(&s) else { throw!(it, "SyntaxError", "Failed to execute 'importScripts': The URL '{s}' is invalid.") };
            let text = match net::fetch(&Request::get(u.clone())) {
                Ok(r) if r.ok() => r.text(),
                _ => throw!(it, "NetworkError", "Failed to execute 'importScripts': The script at '{u}' failed to load."),
            };
            it.eval_script(&text, &u.to_string())?;
        }
        Ok(Value::Undefined)
    });
    // `self instanceof WorkerGlobalScope` and friends
    let _ = it.eval_script(
        "(() => { const mk = (n) => { const f = function () { throw new TypeError('Illegal constructor'); }; Object.defineProperty(f, Symbol.hasInstance, { value: (o) => o === self }); Object.defineProperty(self, n, { value: f, configurable: true, writable: true }); }; mk('WorkerGlobalScope'); mk('DedicatedWorkerGlobalScope'); })();",
        "tip:worker",
    );
    host(it).url = url.clone();
}

impl Page {
    /// Start workers whose script arrived, run their event loops, and deliver messages both
    /// ways. Returns true if anything happened.
    pub fn poll_workers(&mut self) -> bool {
        if self.host().workers.is_empty() {
            return false;
        }
        let mut changed = false;
        let ids: Vec<u64> = self.host().workers.iter().map(|w| w.id).collect();
        for id in ids {
            let Some(i) = self.host().workers.iter().position(|w| w.id == id) else { continue };
            // script arrived: create the worker's global scope and run it
            let arrived = match &self.host().workers[i].pending {
                Some(rx) => rx.try_recv().ok(),
                None => None,
            };
            if let Some(res) = arrived {
                changed = true;
                self.start_worker(i, res);
            }
            let Some(i) = self.host().workers.iter().position(|w| w.id == id) else { continue };
            let outbox = self.host().workers[i].outbox.clone();
            let mut logs = Vec::new();
            let mut closing = false;
            if let Some(p) = &mut self.host_mut().workers[i].page {
                // messages from the owner
                let inbox = std::mem::take(&mut p.host_mut().worker_inbox);
                for (m, ports) in inbox {
                    let data = import(&mut p.interp, &m);
                    let port_objs: Vec<Value> = ports.iter().map(|&(pid, peer)| adopt_port(&mut p.interp, pid, peer)).collect();
                    let ports = p.interp.new_array(port_objs);
                    let init = crate::bindings::events::EventInit { bubbles: false, cancelable: false, props: vec![("data", data), ("ports", ports)] };
                    crate::bindings::events::fire(&mut p.interp, Target::Window, "message", "MessageEvent", init);
                    p.after_task();
                    changed = true;
                }
                changed |= p.poll();
                logs = std::mem::take(&mut p.host_mut().console);
                closing = p.host().worker_link.as_ref().is_some_and(|l| l.closing);
            }
            let url = self.host().workers[i].url.to_string();
            for mut l in logs {
                if l.source.is_empty() {
                    l.source = url.clone();
                }
                self.host_mut().console.push(l);
            }
            // messages to the owner
            let msgs: Vec<WorkerMsg> = outbox.borrow_mut().drain(..).collect();
            for (m, ports) in msgs {
                changed = true;
                let data = import(&mut self.interp, &m);
                let port_objs: Vec<Value> = ports.iter().map(|&(pid, peer)| adopt_port(&mut self.interp, pid, peer)).collect();
                let ports = self.interp.new_array(port_objs);
                let g = Value::Obj(self.interp.global.clone());
                if let Ok(deliver) = self.interp.get_str(&g, "__tip_worker_deliver") {
                    let _ = self.interp.call(&deliver, Value::Undefined, &[Value::Num(id as f64), data, ports]);
                }
                self.after_task();
            }
            if closing {
                if let Some(i) = self.host().workers.iter().position(|w| w.id == id) {
                    self.host_mut().workers[i].closed = true;
                    self.host_mut().workers[i].page = None;
                }
            }
        }
        self.host_mut().workers.retain(|w| !w.closed);
        changed
    }

    fn start_worker(&mut self, i: usize, res: Result<net::Response, String>) {
        let (id, url, module, name) = {
            let w = &mut self.host_mut().workers[i];
            w.pending = None;
            (w.id, w.url.clone(), w.module, w.name.clone())
        };
        let text = match res {
            Ok(r) if r.ok() => r.text(),
            other => {
                let why = match other {
                    Ok(r) => format!("HTTP {}", r.status),
                    Err(e) => e,
                };
                self.host_mut().console.push(ConsoleEntry { level: "error", text: format!("Failed to load worker script {url}: {why}"), source: String::new() });
                self.worker_error(id, &format!("Failed to load worker script {url}"));
                self.host_mut().workers[i].closed = true;
                return;
            }
        };
        let opts = LoadOptions { viewport: (0.0, 0.0), scale: self.host().scale, scripting: true, internal: false };
        let mut page = Page::from_html("", url.clone(), &opts);
        let outbox = self.host().workers[i].outbox.clone();
        page.host_mut().worker_link = Some(WorkerLink { outbox, closing: false });
        install_worker_globals(&mut page, &url, &name);
        page.interp.deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
        let r = if module { page.interp.run_inline_module(&url.to_string(), &text) } else { page.interp.eval_script(&text, &url.to_string()).map(|_| ()) };
        page.interp.deadline = None;
        if let Err(e) = r {
            let msg = page.interp.describe_error(&e);
            self.host_mut().console.push(ConsoleEntry { level: "error", text: format!("Uncaught {msg}"), source: url.to_string() });
            self.worker_error(id, &msg);
        }
        page.after_task();
        let queued = std::mem::take(&mut self.host_mut().workers[i].queued);
        page.host_mut().worker_inbox.extend(queued);
        self.host_mut().workers[i].page = Some(page);
    }

    fn worker_error(&mut self, id: u64, msg: &str) {
        let g = Value::Obj(self.interp.global.clone());
        if let Ok(f) = self.interp.get_str(&g, "__tip_worker_error") {
            let _ = self.interp.call(&f, Value::Undefined, &[Value::Num(id as f64), Value::str(msg)]);
        }
    }

    pub fn workers_pending(&self) -> bool {
        self.host().workers.iter().any(|w| w.pending.is_some() || !w.queued.is_empty() || !w.outbox.borrow().is_empty() || w.page.as_ref().is_some_and(|p| p.has_pending_work() || !p.host().worker_inbox.is_empty()))
    }

    pub fn workers_next_timer_due(&self) -> Option<std::time::Instant> {
        self.host().workers.iter().filter_map(|w| w.page.as_ref().and_then(|p| p.next_timer_due())).min()
    }
}
