//! Session Rust: `<script type="text/rust">` support.
//!
//! Pages ship Rust source; Tip compiles it at load time into a closure tree (no native code,
//! no file system, no threads) and runs `fn main()`. Functions marked `#[export]` become
//! callable from JavaScript as `rust.name(...)`. The compiled program lives in the page's
//! JavaScript heap (through the callbacks and exports that reference it) and is dropped with
//! the page when the user navigates away.

pub mod ast;
pub mod compile;
pub mod format;
pub mod lexer;
pub mod parser;
pub mod runtime;
pub mod stdlib;
pub mod value;
pub mod web;

use std::rc::Rc;

use ast::Pos;
use runtime::{Ctrl, Ctx, Program};
use value::{FuncVal, Value};

/// rustc-style diagnostic with a source excerpt.
pub fn diagnostic(src: &str, file: &str, msg: &str, pos: Pos) -> String {
    let line = pos.line.max(1);
    let text = src.lines().nth(line as usize - 1).unwrap_or("");
    let num = line.to_string();
    let pad = " ".repeat(num.len());
    let caret_at = text.chars().take(pos.col.saturating_sub(1) as usize).map(|c| if c == '\t' { '\t' } else { ' ' }).collect::<String>();
    format!("error: {msg}\n{pad}--> {file}:{line}:{}\n{pad} |\n{num} | {text}\n{pad} | {caret_at}^", pos.col)
}

/// Lex, parse and compile a source file.
pub fn compile(src: &str, file: &str) -> Result<Rc<Program>, String> {
    let toks = lexer::tokenize(src).map_err(|(msg, line, col)| diagnostic(src, file, &msg, Pos { line, col }))?;
    let items = parser::parse_file(toks).map_err(|e| diagnostic(src, file, &e.msg, e.pos))?;
    let prog = Rc::new(Program::new(file));
    let mut c = compile::Compiler::new(prog.clone());
    c.compile_program(&items).map_err(|e| diagnostic(src, file, &e.msg, e.pos))?;
    Ok(prog)
}

/// Run `fn main()` if the program has one. Output goes to `ctx.output`.
pub fn run_main(ctx: &mut Ctx) -> Result<(), String> {
    let has_main = ctx.prog.fns.borrow().contains_key("main");
    if !has_main {
        return Ok(());
    }
    let pos = Pos { line: 1, col: 1 };
    match ctx.call_fn_name("main", vec![], pos) {
        Ok(v) => {
            // `fn main() -> Result<(), E>` reports an `Err`
            if let Some(Err(e)) = runtime::as_result(&v) {
                let d = format::debug(ctx, &e, false, 0, pos).unwrap_or_default();
                return Err(format!("Error: {d}"));
            }
            Ok(())
        }
        Err(Ctrl::Panic(msg, p)) => Err(web::panic_text(&ctx.prog.file, &msg, p)),
        Err(_) => Ok(()),
    }
}

/// Compile and run a program without a page (used by `tip rust FILE` and the unit tests).
/// Returns the printed output and, on failure, the error text. Runs on its own thread so the
/// recursion budget doesn't depend on the caller's stack.
pub fn run_headless(src: &str, file: &str) -> (String, Option<String>) {
    let (src, file) = (src.to_string(), file.to_string());
    let worker = std::thread::Builder::new().stack_size(64 << 20).spawn(move || {
        let prog = match compile(&src, &file) {
            Ok(p) => p,
            Err(e) => return (String::new(), Some(e)),
        };
        let mut ctx = Ctx::new(prog, std::ptr::null_mut());
        let r = run_main(&mut ctx);
        let out: String = ctx.output.iter().map(|(level, s)| if *level == "print" { s.clone() } else { format!("{s}\n") }).collect();
        (out, r.err())
    });
    match worker.map(|h| h.join()) {
        Ok(Ok(r)) => r,
        _ => (String::new(), Some("the Rust runtime crashed".into())),
    }
}

/// Run a page's Rust script: compile, run `main`, and expose `#[export]` functions on
/// `window.rust`. Console output, compile errors and panics go to the page console.
pub fn run_in_page(it: &mut crate::js::Interp, src: &str, file: &str) {
    use crate::js::interp::ConsoleMsg;
    let prog = match compile(src, file) {
        Ok(p) => p,
        Err(e) => {
            it.console.push(ConsoleMsg { level: "error", text: e });
            return;
        }
    };
    install_exports(it, &prog);
    let mut ctx = Ctx::new(prog, it as *mut crate::js::Interp);
    let r = run_main(&mut ctx);
    let output = std::mem::take(&mut ctx.output);
    drop(ctx);
    web::flush(it, output);
    if let Err(e) = r {
        it.console.push(ConsoleMsg { level: "error", text: e });
    }
}

fn install_exports(it: &mut crate::js::Interp, prog: &Rc<Program>) {
    use crate::js::value::Value as Js;
    let names = prog.exports.borrow().clone();
    if names.is_empty() {
        return;
    }
    let g = Js::Obj(it.global.clone());
    let ns = match it.get_str(&g, "rust") {
        Ok(v @ Js::Obj(_)) => v,
        _ => {
            let o = Js::Obj(it.new_object());
            let _ = it.set_str(&g, "rust", o.clone());
            o
        }
    };
    for name in names {
        let Some(code) = prog.fns.borrow().get(&name).and_then(|c| c.borrow().clone()) else { continue };
        let pos = code.pos;
        let nparams = code.nparams;
        let f = Value::Func(Rc::new(FuncVal::Code(code, vec![])));
        let p = prog.clone();
        let native = it.native(&name, nparams as u32, move |it, _this, args| {
            let mut rargs: Vec<Value> = args.iter().map(web::from_js).collect();
            rargs.resize(nparams, Value::Unit);
            web::invoke(it, &p, &f, rargs, pos)
        });
        let _ = it.set_str(&ns, &name, native);
    }
}

#[cfg(test)]
mod tests;
