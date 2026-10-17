//! Developer tools: an internal page that inspects the active content page.

use std::collections::HashSet;

use crate::dom::{escape_attr, escape_text, NodeData, NodeId};
use crate::net::Url;
use crate::page::{ConsoleEntry, HostMsg, LoadOptions, Page};

pub enum DevAction {
    None,
    Close,
    Pick(bool),
}

pub struct DevTools {
    pub page: Page,
    pub panel: String,
    pub selected: Option<NodeId>,
    pub expanded: HashSet<NodeId>,
    pub picking: bool,
    last_version: u64,
    last_selected: Option<NodeId>,
    last_console: usize,
    last_net: usize,
    last_page: u64,
    history: Vec<String>,
}

pub fn set_html(page: &mut Page, id: &str, html: &str) {
    let h = page.host_mut();
    if let Some(n) = h.doc.get_element_by_id(id) {
        h.doc.remove_all_children(n);
        crate::html::parse_fragment_into(&mut h.doc, n, html);
    }
}

pub fn append_html(page: &mut Page, id: &str, html: &str) {
    let h = page.host_mut();
    if let Some(n) = h.doc.get_element_by_id(id) {
        crate::html::parse_fragment_into(&mut h.doc, n, html);
    }
}

fn set_class(page: &mut Page, id: &str, class: &str, on: bool) {
    let h = page.host_mut();
    if let Some(n) = h.doc.get_element_by_id(id) {
        let mut cls: Vec<String> = h.doc.classes(n).into_iter().map(|s| s.to_string()).filter(|c| c != class).collect();
        if on {
            cls.push(class.to_string());
        }
        h.doc.set_attr(n, "class", &cls.join(" "));
    }
}

fn scroll_to_bottom(page: &mut Page, id: &str) {
    let h = page.host_mut();
    if let Some(n) = h.doc.get_element_by_id(id) {
        h.elem_scroll.insert(n, (0.0, 1e7));
        h.invalidate();
    }
}

fn short(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n { format!("{t}…") } else { t }
}

impl DevTools {
    pub fn new(viewport: (f32, f32), scale: f32) -> DevTools {
        let opts = LoadOptions { viewport, scale, scripting: false, internal: true };
        let mut page = Page::from_html(crate::pages::DEVTOOLS, Url::parse("tip://devtools").unwrap(), &opts);
        page.start();
        DevTools { page, panel: "elements".into(), selected: None, expanded: HashSet::new(), picking: false, last_version: u64::MAX, last_selected: None, last_console: 0, last_net: usize::MAX, last_page: u64::MAX, history: vec![] }
    }

    pub fn select(&mut self, content: &Page, n: NodeId) {
        let h = content.host();
        let n = if h.doc.is_element(n) { n } else { h.doc.parent(n).unwrap_or(n) };
        self.selected = Some(n);
        let mut cur = h.doc.parent(n);
        while let Some(p) = cur {
            self.expanded.insert(p);
            cur = h.doc.parent(p);
        }
        self.last_version = u64::MAX;
        if self.panel != "elements" {
            self.show_panel("elements");
        }
    }

    fn show_panel(&mut self, name: &str) {
        for p in ["elements", "console", "network", "app"] {
            set_class(&mut self.page, p, "sel", p == name);
            set_class(&mut self.page, &format!("t-{p}"), "sel", p == name);
        }
        self.panel = name.to_string();
        self.last_net = usize::MAX;
        self.last_version = u64::MAX;
        if name == "console" {
            scroll_to_bottom(&mut self.page, "log");
            let cin = self.page.host().doc.get_element_by_id("cin");
            crate::input::focus(&mut self.page.interp, cin);
        }
    }

    /// Bring the DevTools UI up to date with the content page.
    pub fn sync(&mut self, content: &mut Page, page_gen: u64) {
        if page_gen != self.last_page {
            // new document: reset per-page state
            self.last_page = page_gen;
            self.selected = None;
            self.expanded.clear();
            self.last_console = 0;
            self.last_net = usize::MAX;
            self.last_version = u64::MAX;
            set_html(&mut self.page, "log", "");
            let doc = &content.host().doc;
            if let Some(html) = doc.document_element() {
                self.expanded.insert(0);
                self.expanded.insert(html);
                if let Some(b) = doc.body() {
                    self.expanded.insert(b);
                }
            }
        }
        // console (always kept current so messages aren't lost)
        let console_len = content.host().console.len();
        if console_len < self.last_console {
            self.last_console = 0;
            set_html(&mut self.page, "log", "");
        }
        if console_len > self.last_console {
            let mut html = String::new();
            for e in &content.host().console[self.last_console..] {
                html.push_str(&console_row(e));
            }
            self.last_console = console_len;
            append_html(&mut self.page, "log", &html);
            scroll_to_bottom(&mut self.page, "log");
            // keep the log bounded
            let h = self.page.host_mut();
            if let Some(log) = h.doc.get_element_by_id("log") {
                let kids = h.doc.children(log).to_vec();
                if kids.len() > 1500 {
                    for k in &kids[..kids.len() - 1500] {
                        h.doc.detach(*k);
                    }
                }
            }
        }
        match self.panel.as_str() {
            "elements" => {
                let v = content.host().doc.version;
                if v != self.last_version || self.selected != self.last_selected {
                    self.last_version = v;
                    self.last_selected = self.selected;
                    let tree = self.tree_html(content);
                    set_html(&mut self.page, "tree", &tree);
                    let side = self.side_html(content);
                    set_html(&mut self.page, "side", &side);
                }
            }
            "network" => {
                let n = content.host().net_log.len();
                if n != self.last_net {
                    self.last_net = n;
                    let (table, sum) = network_html(content);
                    set_html(&mut self.page, "net", &table);
                    set_html(&mut self.page, "netsum", &sum);
                }
            }
            "app" => {
                let v = content.host().doc.version;
                if v != self.last_version {
                    self.last_version = v;
                    let html = app_html(content);
                    set_html(&mut self.page, "app", &html);
                }
            }
            _ => {}
        }
    }

    /// Handle a click/submit from the DevTools UI.
    pub fn handle(&mut self, msg: HostMsg, content: &mut Page) -> DevAction {
        match msg {
            HostMsg::Action { action, arg, .. } => match action.as_str() {
                "tab" => self.show_panel(&arg),
                "select" => {
                    if let Ok(n) = arg.parse::<NodeId>() {
                        self.selected = Some(n);
                        content.host_mut().highlight = Some(n);
                    }
                }
                "toggle" => {
                    if let Ok(n) = arg.parse::<NodeId>() {
                        if !self.expanded.remove(&n) {
                            self.expanded.insert(n);
                        }
                        self.last_version = u64::MAX;
                    }
                }
                "pick" => {
                    self.picking = !self.picking;
                    set_class(&mut self.page, "pick", "on", self.picking);
                    return DevAction::Pick(self.picking);
                }
                "close-devtools" => return DevAction::Close,
                "clear-console" => {
                    content.host_mut().console.clear();
                    self.last_console = 0;
                    set_html(&mut self.page, "log", "");
                }
                _ => {}
            },
            HostMsg::Submit { action, value, node } => {
                if action == "eval" && !value.trim().is_empty() {
                    self.history.push(value.clone());
                    content.host_mut().console.push(ConsoleEntry { level: "input", text: value.clone(), source: String::new() });
                    let before = content.host().console.len();
                    let r = content.interp.eval_script(&value, "console");
                    content.after_task();
                    let text = match r {
                        Ok(v) => {
                            if let Some(n) = crate::bindings::node_of(&v) {
                                self.select(content, n);
                            }
                            Some(("result", content.interp.inspect(&v, 3)))
                        }
                        Err(e) => Some(("error", format!("Uncaught {}", content.interp.describe_error(&e)))),
                    };
                    if let Some((level, t)) = text {
                        let at = content.host().console.len().min(before.max(0));
                        let _ = at;
                        content.host_mut().console.push(ConsoleEntry { level, text: t, source: String::new() });
                    }
                    self.page.host_mut().set_control_value(node, String::new());
                }
            }
        }
        DevAction::None
    }

    fn tree_html(&self, content: &Page) -> String {
        let doc = &content.host().doc;
        let mut out = String::new();
        let mut count = 0usize;
        self.node_html(doc, 0, 0, &mut out, &mut count);
        out
    }

    fn node_html(&self, doc: &crate::dom::Document, n: NodeId, depth: usize, out: &mut String, count: &mut usize) {
        *count += 1;
        if *count > 4000 {
            return;
        }
        let pad = depth * 14;
        match &doc.node(n).data {
            NodeData::Document => {
                for &c in doc.children(n) {
                    self.node_html(doc, c, depth, out, count);
                }
            }
            NodeData::Element(e) => {
                let kids: Vec<NodeId> = doc.children(n).iter().copied().filter(|&c| !matches!(&doc.node(c).data, NodeData::Text(t) if t.trim().is_empty())).collect();
                let open = self.expanded.contains(&n);
                let sel = if self.selected == Some(n) { " sel" } else { "" };
                let mut attrs = String::new();
                for (k, v) in &e.attrs {
                    attrs.push_str(&format!(" <span class=an>{}</span>=\"<span class=av>{}</span>\"", escape_text(k), escape_text(&short(v, 80))));
                }
                let only_text = kids.len() == 1 && matches!(doc.node(kids[0]).data, NodeData::Text(_));
                let twisty = if kids.is_empty() || only_text {
                    "<span class=tw></span>".to_string()
                } else {
                    format!("<span class=tw data-action=toggle data-arg={n}>{}</span>", if open { "▾" } else { "▸" })
                };
                if only_text {
                    let t = match &doc.node(kids[0]).data {
                        NodeData::Text(t) => short(t.trim(), 120),
                        _ => String::new(),
                    };
                    out.push_str(&format!(
                        "<div class=\"row{sel}\" style=\"padding-left:{pad}px\" data-action=select data-arg={n}>{twisty}<span class=tag>&lt;{tag}</span>{attrs}<span class=tag>&gt;</span><span class=txt>{}</span><span class=tag>&lt;/{tag}&gt;</span></div>",
                        escape_text(&t),
                        tag = e.tag
                    ));
                    return;
                }
                let close = if kids.is_empty() && crate::dom::VOID_ELEMENTS.contains(&e.tag.as_str()) {
                    String::new()
                } else if !open {
                    format!("<span class=txt>…</span><span class=tag>&lt;/{}&gt;</span>", e.tag)
                } else {
                    String::new()
                };
                out.push_str(&format!("<div class=\"row{sel}\" style=\"padding-left:{pad}px\" data-action=select data-arg={n}>{twisty}<span class=tag>&lt;{}</span>{attrs}<span class=tag>&gt;</span>{close}</div>", e.tag));
                if open {
                    for c in kids {
                        self.node_html(doc, c, depth + 1, out, count);
                    }
                    if !crate::dom::VOID_ELEMENTS.contains(&e.tag.as_str()) {
                        out.push_str(&format!("<div class=row style=\"padding-left:{pad}px\" data-action=select data-arg={n}><span class=tw></span><span class=tag>&lt;/{}&gt;</span></div>", e.tag));
                    }
                }
            }
            NodeData::Text(t) => {
                let t = t.trim();
                if !t.is_empty() {
                    out.push_str(&format!("<div class=row style=\"padding-left:{pad}px\"><span class=tw></span><span class=txt>\"{}\"</span></div>", escape_text(&short(t, 200))));
                }
            }
            NodeData::Comment(c) => {
                out.push_str(&format!("<div class=row style=\"padding-left:{pad}px\"><span class=tw></span><span class=cmt>&lt;!--{}--&gt;</span></div>", escape_text(&short(c, 120))));
            }
            NodeData::Fragment => {}
        }
    }

    fn side_html(&self, content: &mut Page) -> String {
        let Some(n) = self.selected else {
            return "<div class=empty>Select an element to inspect its styles.</div>".into();
        };
        if !content.host().doc.is_element(n) {
            return String::new();
        }
        let mut out = String::new();
        // box model
        let l = content.host_mut().layout();
        if let Some(b) = l.find_box(n) {
            let f = |v: f32| if v == 0.0 { "–".to_string() } else { format!("{}", (v * 10.0).round() / 10.0) };
            out.push_str("<div class=h>Box model</div><div class=bm>");
            out.push_str(&format!("<div class=m>margin {} / {} / {} / {}", f(b.margin.top), f(b.margin.right), f(b.margin.bottom), f(b.margin.left)));
            out.push_str(&format!("<div class=b>border {} / {} / {} / {}", f(b.border.top), f(b.border.right), f(b.border.bottom), f(b.border.left)));
            out.push_str(&format!("<div class=p>padding {} / {} / {} / {}", f(b.padding.top), f(b.padding.right), f(b.padding.bottom), f(b.padding.left)));
            out.push_str(&format!("<div class=c>{} × {}</div></div></div></div></div>", f(b.content.w), f(b.content.h)));
        }
        // matched rules (most specific first)
        let rules = content.host_mut().matched_rules(n);
        let infos = content.host_mut().sheet_infos();
        out.push_str("<div class=h>Styles</div>");
        if let Some(inline) = content.host().doc.attr(n, "style") {
            out.push_str("<div class=rule><span class=sel-t>element.style</span> {");
            for d in crate::css::parse_declarations(inline) {
                out.push_str(&format!("<div class=decl><span class=pn>{}</span>: <span class=pv>{}</span>;</div>", escape_text(&d.name), escape_text(&d.value_string())));
            }
            out.push_str("}</div>");
        }
        let mut seen: HashSet<String> = HashSet::new();
        for r in rules.iter().rev() {
            let src = infos.get(r.sheet).map(|i| i.source.clone()).unwrap_or_default();
            let src = src.rsplit('/').next().unwrap_or(&src).to_string();
            out.push_str(&format!("<div class=rule><span class=src>{}</span><span class=sel-t>{}</span> {{", escape_text(&src), escape_text(&r.selector)));
            for (k, v, imp) in &r.decls {
                let over = !seen.insert(k.clone());
                out.push_str(&format!(
                    "<div class=decl><span class=\"{}\"><span class=pn>{}</span>: <span class=pv>{}{}</span>;</span></div>",
                    if over { "over" } else { "" },
                    escape_text(k),
                    escape_text(v),
                    if *imp { " !important" } else { "" }
                ));
            }
            out.push_str("}</div>");
        }
        // computed
        let styles = content.host_mut().styles();
        if let Some(s) = styles.get(n) {
            out.push_str("<div class=h>Computed</div>");
            for (k, v) in s.to_properties() {
                out.push_str(&format!("<div class=cs><span class=k>{k}</span><span>{}</span></div>", escape_text(&v)));
            }
        }
        out
    }
}

fn console_row(e: &ConsoleEntry) -> String {
    let (class, prefix) = match e.level {
        "error" => ("error", "✖ "),
        "warn" => ("warn", "⚠ "),
        "input" => ("input", "› "),
        "result" => ("result", "← "),
        _ => ("", ""),
    };
    let src = if e.source.is_empty() { String::new() } else { format!("<span class=s>{}</span>", escape_text(&short(e.source.rsplit('/').next().unwrap_or(&e.source), 40))) };
    format!("<div class=\"msg {class}\">{src}{prefix}{}</div>", escape_text(&short(&e.text, 5000)))
}

fn human_size(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    } else if n >= 1024 {
        format!("{:.1} kB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn network_html(content: &Page) -> (String, String) {
    let log = &content.host().net_log;
    if log.is_empty() {
        return ("<div class=empty>No requests recorded for this page.</div>".into(), String::new());
    }
    let mut out = String::from("<table><tr><th>Name</th><th>Status</th><th>Type</th><th>Size</th><th>Time</th></tr>");
    let mut total = 0;
    for e in log {
        total += e.size;
        let name = e.url.trim_end_matches('/').rsplit('/').next().unwrap_or(&e.url).to_string();
        let name = if name.is_empty() { e.url.clone() } else { name };
        let bad = e.error.is_some() || e.status >= 400;
        let status = match &e.error {
            Some(err) => format!("failed: {}", short(err, 40)),
            None => e.status.to_string(),
        };
        out.push_str(&format!(
            "<tr class=\"{}\"><td class=u title=\"{}\">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{} ms</td></tr>",
            if bad { "bad" } else { "" },
            escape_attr(&e.url),
            escape_text(&short(&name, 60)),
            escape_text(&status),
            e.kind,
            human_size(e.size),
            e.ms
        ));
    }
    out.push_str("</table>");
    (out, format!("{} requests · {} transferred", log.len(), human_size(total)))
}

fn app_html(content: &Page) -> String {
    let h = content.host();
    let mut out = String::from("<div class=h>Page</div>");
    out.push_str(&format!("<div class=kv><span class=k>URL</span><span class=v>{}</span></div>", escape_text(&h.url.to_string())));
    out.push_str(&format!("<div class=kv><span class=k>Title</span><span class=v>{}</span></div>", escape_text(&h.doc.title())));
    out.push_str(&format!("<div class=kv><span class=k>DOM nodes</span><span class=v>{}</span></div>", h.doc.nodes.len()));
    out.push_str(&format!("<div class=kv><span class=k>Timers</span><span class=v>{}</span></div>", h.timers.len()));
    out.push_str("<div class=h>Cookies</div>");
    let c = crate::net::document_cookie(&h.url);
    if c.is_empty() {
        out.push_str("<div class=empty>No cookies</div>");
    }
    for part in c.split("; ").filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        out.push_str(&format!("<div class=kv><span class=k>{}</span><span class=v>{}</span></div>", escape_text(k), escape_text(v)));
    }
    out
}
