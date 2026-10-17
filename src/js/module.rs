//! ES modules: the module map, linking with live bindings, evaluation and dynamic `import()`.

use std::cell::RefCell;
use std::rc::Rc;

use super::ast::*;
use super::builtins::promise;
use super::interp::{read_binding, throw, Env, FuncScope, Interp, Job, Scope, R};
use super::value::*;
use crate::net::Url;

/// Supplies module source text to the interpreter (the page fetches over the network).
pub trait ModuleLoader {
    /// Fetch a module; returns its final URL (after redirects) and source.
    fn fetch(&self, it: &mut Interp, url: &str) -> Result<(String, String), String>;
    /// Hint that these modules will be requested soon (fetch them in parallel).
    fn prefetch(&self, _it: &mut Interp, _urls: &[String]) {}
    /// Base URL for `import()` from classic scripts.
    fn base_url(&self, it: &mut Interp) -> String;
}

#[derive(Clone)]
pub enum Status {
    Unlinked,
    Linked,
    Evaluating,
    Evaluated,
    Errored(Value),
}

pub struct Module {
    pub url: String,
    pub env: Env,
    pub info: ModuleInfo,
    pub body: Rc<Vec<Stmt>>,
    /// Requested specifier → loaded module.
    pub deps: Vec<(Name, ModRef)>,
    pub status: Status,
    pub namespace: Option<Obj>,
}

pub type ModRef = Rc<RefCell<Module>>;

enum Resolved {
    Binding(Env, Name),
    Namespace(ModRef),
}

/// Resolve a module specifier against the referencing module's URL.
pub fn resolve_specifier(base: &str, spec: &str) -> Option<String> {
    if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
        Url::parse(base)?.join(spec).map(|u| u.to_string())
    } else {
        let u = Url::parse(spec)?;
        if u.scheme.is_empty() {
            return None;
        }
        Some(u.to_string())
    }
}

fn dep(m: &ModRef, spec: &str) -> Option<ModRef> {
    m.borrow().deps.iter().find(|(s, _)| &**s == spec).map(|(_, d)| d.clone())
}

impl Interp {
    /// Fetch and parse `url` and its static dependency graph (cached in the module map).
    pub fn load_module(&mut self, url: &str) -> R<ModRef> {
        if let Some(m) = self.modules.get(url) {
            return Ok(m.clone());
        }
        let Some(loader) = self.module_loader.clone() else {
            throw!(self, "TypeError", "Failed to fetch module {url}: modules are not available here")
        };
        let (final_url, src) = match loader.fetch(self, url) {
            Ok(r) => r,
            Err(e) => throw!(self, "TypeError", "Failed to fetch dynamically imported module: {url} ({e})"),
        };
        if final_url != url {
            if let Some(m) = self.modules.get(&final_url) {
                let m = m.clone();
                self.modules.insert(url.to_string(), m.clone());
                return Ok(m);
            }
        }
        let mut keys = vec![url.to_string()];
        if final_url != url {
            keys.push(final_url.clone());
        }
        self.create_module(&final_url, &src, &keys)
    }

    /// Parse module source and load its dependencies. `keys` are the module-map entries to register.
    pub fn create_module(&mut self, url: &str, src: &str, keys: &[String]) -> R<ModRef> {
        let (stmts, var_names, info) = match super::parser::parse_module(src) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("{} ({url})", e.trim_start_matches("SyntaxError: "));
                return Err(self.make_error("SyntaxError", &msg));
            }
        };
        let env = Scope::new_func(Some(self.global_env.clone()), FuncScope { this: RefCell::new(Some(Value::Undefined)), home: None, new_target: Value::Undefined, func: None });
        for n in var_names {
            env.declare(n, Value::Undefined, true);
        }
        self.install_module_helpers(&env, url);
        self.hoist(&stmts, &env)?;
        let requests = info.requests.clone();
        let m = Rc::new(RefCell::new(Module { url: url.to_string(), env, info, body: Rc::new(stmts), deps: vec![], status: Status::Unlinked, namespace: None }));
        for k in keys {
            self.modules.insert(k.clone(), m.clone());
        }
        let r = self.load_deps(&m, url, &requests);
        if let Err(e) = r {
            for k in keys {
                self.modules.remove(k);
            }
            return Err(e);
        }
        Ok(m)
    }

    fn load_deps(&mut self, m: &ModRef, url: &str, requests: &[Name]) -> R<()> {
        let mut urls = Vec::new();
        for spec in requests {
            match resolve_specifier(url, spec) {
                Some(u) => urls.push(u),
                None => throw!(self, "TypeError", "Failed to resolve module specifier \"{spec}\". Relative references must start with either \"/\", \"./\", or \"../\"."),
            }
        }
        let missing: Vec<String> = urls.iter().filter(|u| !self.modules.contains_key(*u)).cloned().collect();
        if missing.len() > 1 {
            if let Some(l) = self.module_loader.clone() {
                l.prefetch(self, &missing);
            }
        }
        for (spec, u) in requests.iter().zip(urls) {
            let d = self.load_module(&u)?;
            m.borrow_mut().deps.push((spec.clone(), d));
        }
        Ok(())
    }

    /// Per-module `import()` and `import.meta`.
    fn install_module_helpers(&mut self, env: &Env, url: &str) {
        let base = url.to_string();
        let imp = self.native("import", 1, move |it, _, args| {
            let spec = args.first().cloned().unwrap_or(Value::Undefined);
            dynamic_import(it, base.clone(), spec)
        });
        env.declare(Rc::from("__tip_import"), imp, false);
        let meta = Obj::new(ObjKind::Ordinary, None, "Object");
        meta.borrow_mut().props.insert(PropKey::from("url"), Prop::data(Value::str(url)));
        let base = url.to_string();
        let resolve = self.native("resolve", 1, move |it, _, args| {
            let spec = it.to_str(&args.first().cloned().unwrap_or(Value::Undefined))?;
            match resolve_specifier(&base, &spec) {
                Some(u) => Ok(Value::str(&u)),
                None => throw!(it, "TypeError", "Failed to resolve module specifier \"{spec}\""),
            }
        });
        meta.borrow_mut().props.insert(PropKey::from("resolve"), Prop::data(resolve));
        env.declare(Rc::from("__tip_import_meta"), Value::Obj(meta), false);
    }

    /// Bind each module's imports to the exporting modules' bindings.
    pub fn link_module(&mut self, m: &ModRef) -> R<()> {
        if !matches!(m.borrow().status, Status::Unlinked) {
            return Ok(());
        }
        m.borrow_mut().status = Status::Linked;
        let deps: Vec<ModRef> = m.borrow().deps.iter().map(|(_, d)| d.clone()).collect();
        for d in &deps {
            self.link_module(d)?;
        }
        let (imports, env, url) = {
            let b = m.borrow();
            (b.info.imports.clone(), b.env.clone(), b.url.clone())
        };
        for imp in imports {
            let Some(target) = dep(m, &imp.specifier) else { continue };
            match &imp.import {
                None => {
                    let ns = self.module_namespace(&target);
                    env.declare(imp.local.clone(), Value::Obj(ns), false);
                }
                Some(name) => match resolve_export(&target, name, &mut vec![]) {
                    Some(Resolved::Binding(e, local)) => env.declare_link(imp.local.clone(), e, local),
                    Some(Resolved::Namespace(t)) => {
                        let ns = self.module_namespace(&t);
                        env.declare(imp.local.clone(), Value::Obj(ns), false);
                    }
                    None => {
                        let msg = format!("The requested module '{}' does not provide an export named '{}' ({url})", imp.specifier, name);
                        let e = self.make_error("SyntaxError", &msg);
                        m.borrow_mut().status = Status::Errored(e.clone());
                        return Err(e);
                    }
                },
            }
        }
        Ok(())
    }

    /// Run a linked module (dependencies first). Errors are cached on the module.
    pub fn evaluate_module(&mut self, m: &ModRef) -> R<()> {
        let status = m.borrow().status.clone();
        match status {
            Status::Unlinked => {
                self.link_module(m)?;
            }
            Status::Evaluating | Status::Evaluated => return Ok(()),
            Status::Errored(e) => return Err(e),
            Status::Linked => {}
        }
        m.borrow_mut().status = Status::Evaluating;
        let deps: Vec<ModRef> = m.borrow().deps.iter().map(|(_, d)| d.clone()).collect();
        let mut result = Ok(());
        for d in &deps {
            if let Err(e) = self.evaluate_module(d) {
                result = Err(e);
                break;
            }
        }
        if result.is_ok() {
            let (body, env, url) = {
                let b = m.borrow();
                (b.body.clone(), b.env.clone(), b.url.clone())
            };
            let saved = std::mem::replace(&mut self.script_name, url);
            result = self.exec_stmts(&body, &env).map(|_| ());
            self.script_name = saved;
        }
        m.borrow_mut().status = match &result {
            Ok(()) => Status::Evaluated,
            Err(e) => Status::Errored(e.clone()),
        };
        result
    }

    /// Load, link and evaluate the module at `url`; returns its namespace object.
    pub fn import_module(&mut self, url: &str) -> R<Value> {
        let m = self.load_module(url)?;
        self.link_module(&m)?;
        self.evaluate_module(&m)?;
        Ok(Value::Obj(self.module_namespace(&m)))
    }

    /// Run an inline `<script type="module">` whose base URL is `base`.
    pub fn run_inline_module(&mut self, base: &str, src: &str) -> R<()> {
        let m = self.create_module(base, src, &[])?;
        self.link_module(&m)?;
        self.evaluate_module(&m)
    }

    /// The module namespace object (`import * as ns`), created on first use.
    pub fn module_namespace(&mut self, m: &ModRef) -> Obj {
        if let Some(ns) = &m.borrow().namespace {
            return ns.clone();
        }
        let ns = Obj::new(ObjKind::Ordinary, None, "Module");
        m.borrow_mut().namespace = Some(ns.clone());
        let mut names = Vec::new();
        export_names(m, &mut names, &mut vec![]);
        names.sort();
        for n in names {
            let getter = match resolve_export(m, &n, &mut vec![]) {
                Some(Resolved::Binding(env, local)) => self.native(&format!("get {n}"), 0, move |it, _, _| match read_binding(&env, &local) {
                    Some(v) => Ok(v),
                    None => throw!(it, "ReferenceError", "Cannot access '{local}' before initialization"),
                }),
                Some(Resolved::Namespace(t)) => self.native(&format!("get {n}"), 0, move |it, _, _| Ok(Value::Obj(it.module_namespace(&t)))),
                None => continue,
            };
            ns.borrow_mut().props.insert(PropKey::Str(n.clone()), Prop { value: Value::Undefined, getter: Some(getter), setter: None, enumerable: true, writable: true, configurable: false });
        }
        let tag = PropKey::Sym(self.sym_to_string_tag.clone());
        ns.borrow_mut().props.insert(tag, Prop { value: Value::str("Module"), getter: None, setter: None, enumerable: false, writable: false, configurable: false });
        ns
    }
}

fn resolve_export(m: &ModRef, name: &str, visited: &mut Vec<(*const RefCell<Module>, Name)>) -> Option<Resolved> {
    let key = (Rc::as_ptr(m), Rc::from(name));
    if visited.contains(&key) {
        return None;
    }
    visited.push(key);
    let b = m.borrow();
    for e in &b.info.exports {
        match e {
            ExportEntry::Local { exported, local } if &**exported == name => return Some(Resolved::Binding(b.env.clone(), local.clone())),
            ExportEntry::Indirect { exported, specifier, import } if &**exported == name => {
                let t = dep(m, specifier)?;
                return match import {
                    None => Some(Resolved::Namespace(t)),
                    Some(i) => resolve_export(&t, i, visited),
                };
            }
            _ => {}
        }
    }
    if name == "default" {
        return None;
    }
    for spec in &b.info.star_exports {
        if let Some(t) = dep(m, spec) {
            if let Some(r) = resolve_export(&t, name, visited) {
                return Some(r);
            }
        }
    }
    None
}

fn export_names(m: &ModRef, out: &mut Vec<Name>, visited: &mut Vec<*const RefCell<Module>>) {
    if visited.contains(&Rc::as_ptr(m)) {
        return;
    }
    visited.push(Rc::as_ptr(m));
    let b = m.borrow();
    for e in &b.info.exports {
        let (ExportEntry::Local { exported, .. } | ExportEntry::Indirect { exported, .. }) = e;
        if !out.contains(exported) {
            out.push(exported.clone());
        }
    }
    for spec in &b.info.star_exports {
        if let Some(t) = dep(m, spec) {
            let mut inner = Vec::new();
            export_names(&t, &mut inner, visited);
            for n in inner {
                if &*n != "default" && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
    }
}

/// `import(spec)` relative to `base`: returns a promise for the module namespace.
pub fn dynamic_import(it: &mut Interp, base: String, spec: Value) -> R<Value> {
    let (p, resolve, reject) = promise::new_capability(it);
    it.enqueue(Job::Native(Box::new(move |it| {
        let r = (|| -> R<Value> {
            let s = it.to_str(&spec)?;
            let Some(url) = resolve_specifier(&base, &s) else {
                throw!(it, "TypeError", "Failed to resolve module specifier \"{s}\". Relative references must start with either \"/\", \"./\", or \"../\".")
            };
            it.import_module(&url)
        })();
        match r {
            Ok(ns) => it.call(&resolve, Value::Undefined, &[ns])?,
            Err(e) => it.call(&reject, Value::Undefined, &[e])?,
        };
        Ok(())
    })));
    Ok(p)
}

/// `import()` from a classic script resolves against the document URL.
pub fn global_import(it: &mut Interp, spec: Value) -> R<Value> {
    let base = match it.module_loader.clone() {
        Some(l) => l.base_url(it),
        None => String::new(),
    };
    dynamic_import(it, base, spec)
}
