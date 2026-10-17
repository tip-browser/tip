//! Scripts inserted after parsing: `document.createElement('script')` + `appendChild`,
//! `document.write`, and moves of never-run scripts into the document.
//!
//! Inline scripts run during the insertion; external ones are fetched in the background and
//! run when they arrive (dynamically inserted scripts are async by default), followed by
//! their `load` or `error` event. A script runs at most once. Scripts parsed by `innerHTML`
//! never run, as in other browsers.

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use super::ph;
use crate::dom::{NodeData, NodeId};
use crate::js::interp::Interp;
use crate::net::{self, Request, Response, Url};
use crate::page::{ConsoleEntry, Page, Target};

/// An external script being fetched.
pub struct PendingScript {
    pub node: NodeId,
    pub url: Url,
    pub module: bool,
    rx: Receiver<(Result<Response, String>, Vec<net::NetLogEntry>)>,
}

#[derive(PartialEq)]
enum Kind {
    Classic,
    Module,
    Rust,
}

fn kind_of(it: &mut Interp, s: NodeId) -> Option<Kind> {
    let h = ph(it);
    if h.doc.attr(s, "nomodule").is_some() {
        return None;
    }
    let ty = h.doc.attr(s, "type").unwrap_or("").trim().to_ascii_lowercase();
    Some(match ty.as_str() {
        "" | "text/javascript" | "application/javascript" | "application/x-javascript" | "text/ecmascript" | "application/ecmascript" | "text/jscript" => Kind::Classic,
        "module" => Kind::Module,
        "text/rust" | "application/rust" | "rust" => Kind::Rust,
        t if t.contains("javascript") => Kind::Classic,
        _ => return None,
    })
}

/// The top-level nodes an insertion added (a fragment contributes its children).
pub fn roots_of(it: &mut Interp, c: NodeId) -> Vec<NodeId> {
    let h = ph(it);
    if matches!(h.doc.node(c).data, NodeData::Fragment) {
        h.doc.children(c).to_vec()
    } else {
        vec![c]
    }
}

/// Mark scripts under `root` as already started (`innerHTML` and friends never run them).
pub fn mark_started(it: &mut Interp, root: NodeId) {
    let h = ph(it);
    let mut s = h.doc.elements_by_tag(root, "script");
    if h.doc.tag_is(root, "script") {
        s.push(root);
    }
    h.started_scripts.extend(s);
}

/// Run scripts that just became connected under `roots`.
pub fn run_inserted(it: &mut Interp, roots: &[NodeId]) {
    if !ph(it).scripting {
        return;
    }
    let scripts: Vec<NodeId> = {
        let h = ph(it);
        let mut out = Vec::new();
        for &r in roots {
            if !h.doc.is_connected(r) {
                continue;
            }
            if h.doc.tag_is(r, "script") {
                out.push(r);
            }
            out.extend(h.doc.elements_by_tag(r, "script"));
        }
        out.retain(|s| !h.started_scripts.contains(s));
        out
    };
    for s in scripts {
        prepare(it, s);
    }
}

/// The HTML "prepare the script element" steps for a dynamically inserted script.
fn prepare(it: &mut Interp, s: NodeId) {
    let (src, text) = {
        let h = ph(it);
        (h.doc.attr(s, "src").map(|v| v.trim().to_string()), h.doc.text_content(s))
    };
    // a script with neither src nor text stays unstarted, so setting it later can run it
    if src.is_none() && text.trim().is_empty() {
        return;
    }
    ph(it).started_scripts.insert(s);
    let Some(kind) = kind_of(it, s) else { return };
    match src {
        Some(src) => {
            let Some(url) = ph(it).resolve_url(&src) else {
                fire(it, s, "error");
                return;
            };
            if kind == Kind::Rust {
                // fetched synchronously like the page-load path; Rust scripts are rare
                let mut log = Vec::new();
                let r = net::fetch_logged(&Request::get(url.clone()), "script", &mut log);
                ph(it).net_log.extend(log);
                match r {
                    Ok(resp) if resp.ok() => {
                        let name = url.path.rsplit('/').next().unwrap_or("main.rs").to_string();
                        crate::rust::run_in_page(it, &resp.text(), &name);
                        fire(it, s, "load");
                    }
                    _ => fire(it, s, "error"),
                }
                return;
            }
            let (tx, rx) = channel();
            let req = Request::get(url.clone());
            std::thread::spawn(move || {
                let mut log = Vec::new();
                let r = net::fetch_logged(&req, "script", &mut log);
                let _ = tx.send((r, log));
            });
            ph(it).pending_scripts.push(PendingScript { node: s, url, module: kind == Kind::Module, rx });
        }
        None => {
            let prev = ph(it).current_script.replace(s);
            let base = ph(it).resolve_url("").map(|u| u.to_string()).unwrap_or_default();
            let name = format!("{} (inline)", ph(it).url);
            let r = match kind {
                Kind::Classic => it.eval_script(&text, &name).map(|_| ()),
                Kind::Module => it.run_inline_module(&base, &text),
                Kind::Rust => {
                    crate::rust::run_in_page(it, &text, "inline.rs");
                    Ok(())
                }
            };
            ph(it).current_script = prev;
            if let Err(e) = r {
                report(it, &e, &name);
            }
        }
    }
}

fn report(it: &mut Interp, e: &crate::js::value::Value, source: &str) {
    let msg = it.describe_error(e);
    let line = it.cur_line + 1;
    ph(it).console.push(ConsoleEntry { level: "error", text: format!("Uncaught {msg}"), source: format!("{source}:{line}") });
}

fn fire(it: &mut Interp, s: NodeId, ty: &str) {
    super::events::fire(it, Target::Node(s), ty, "Event", super::events::EventInit::default());
}

impl Page {
    /// Run external scripts whose download finished. Returns true if any ran.
    pub fn poll_scripts(&mut self) -> bool {
        if self.host().pending_scripts.is_empty() {
            return false;
        }
        let mut ready = Vec::new();
        {
            let h = self.host_mut();
            let mut i = 0;
            while i < h.pending_scripts.len() {
                match h.pending_scripts[i].rx.try_recv() {
                    Ok(r) => {
                        let p = h.pending_scripts.remove(i);
                        ready.push((p, Some(r)));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => i += 1,
                    Err(_) => {
                        let p = h.pending_scripts.remove(i);
                        ready.push((p, None));
                    }
                }
            }
        }
        let any = !ready.is_empty();
        for (p, r) in ready {
            let Some((result, log)) = r else {
                self.fire_simple(Target::Node(p.node), "error", false);
                continue;
            };
            self.host_mut().net_log.extend(log);
            match result {
                Ok(resp) if resp.ok() => {
                    // network time does not count against the script's run time
                    self.interp.deadline = Some(Instant::now() + Duration::from_secs(10));
                    let prev = self.host_mut().current_script.replace(p.node);
                    let ok = if p.module {
                        match self.interp.import_module(&p.url.to_string()) {
                            Ok(_) => true,
                            Err(e) => {
                                report(&mut self.interp, &e, &p.url.to_string());
                                false
                            }
                        }
                    } else {
                        let _ = self.interp.eval_script(&resp.text(), &p.url.to_string()).map_err(|e| report(&mut self.interp, &e, &p.url.to_string()));
                        true
                    };
                    self.host_mut().current_script = prev;
                    self.interp.deadline = None;
                    self.fire_simple(Target::Node(p.node), if ok { "load" } else { "error" }, false);
                }
                Ok(resp) => {
                    self.host_mut().log("error", format!("Failed to load script {}: HTTP {}", p.url, resp.status));
                    self.fire_simple(Target::Node(p.node), "error", false);
                }
                Err(e) => {
                    self.host_mut().log("error", format!("Failed to load script {}: {e}", p.url));
                    self.fire_simple(Target::Node(p.node), "error", false);
                }
            }
            self.after_task();
        }
        any
    }
}
