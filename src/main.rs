#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::Path;

use tip::automation;
use tip::net::url::from_user_input;
use tip::page::LoadOptions;
use tip::tab::Tab;

const HELP: &str = "Tip — a web browser built from scratch in Rust

USAGE:
    tip [URL]                         Open a browser window (default: tip://home)
    tip render URL [-o out.png] [--width W] [--height H] [--scale S] [--full-page]
                                      Render a page headlessly to a PNG
    tip test PATH... [--junit FILE] [--screenshots DIR] [--verbose]
                                      Run JavaScript automation tests (*.test.js)
    tip dump dom|layout|text URL      Print the DOM tree, layout tree or page text
    tip eval URL CODE                 Load a page, evaluate JavaScript, print the result
    tip js FILE                       Run a JavaScript file in a bare engine
    tip version                       Print the version

Inside the browser window:
    Cmd/Ctrl+L focus address bar · Cmd/Ctrl+R reload · Cmd/Ctrl+[ / ] back/forward
    F12 or Cmd/Ctrl+Alt+I toggle DevTools · Cmd/Ctrl+ +/-/0 zoom";

fn attach_console() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn AttachConsole(pid: u32) -> i32;
        }
        unsafe {
            AttachConsole(0xFFFF_FFFF);
        }
    }
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") || a == "-o" {
            skip = !matches!(a.as_str(), "--full-page" | "--verbose" | "--no-js");
            continue;
        }
        out.push(a.clone());
    }
    out
}

fn load_tab(url: &str, args: &[String]) -> Option<Tab> {
    let Some(u) = from_user_input(url) else {
        eprintln!("Invalid URL: {url}");
        return None;
    };
    let w: f32 = opt(args, "--width").and_then(|v| v.parse().ok()).unwrap_or(1280.0);
    let h: f32 = opt(args, "--height").and_then(|v| v.parse().ok()).unwrap_or(800.0);
    let scripting = !args.iter().any(|a| a == "--no-js");
    let mut tab = Tab::new(&u, LoadOptions { viewport: (w, h), scale: 1.0, scripting, internal: false });
    automation::settle(&mut tab, opt(args, "--wait").and_then(|v| v.parse().ok()).unwrap_or(500));
    Some(tab)
}

fn print_console(tab: &Tab) {
    for c in &tab.page.host().console {
        eprintln!("[console.{}] {}{}", c.level, c.text, if c.source.is_empty() { String::new() } else { format!("  ({})", c.source) });
    }
}

fn run(args: Vec<String>) -> i32 {
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { vec![] } else { args[1..].to_vec() };
    match cmd {
        "help" | "--help" | "-h" => {
            println!("{HELP}");
            0
        }
        "version" | "--version" | "-V" => {
            println!("tip {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "render" => {
            let pos = positional(&rest);
            let Some(url) = pos.first() else {
                eprintln!("usage: tip render URL [-o out.png]");
                return 2;
            };
            let Some(mut tab) = load_tab(url, &rest) else { return 1 };
            let out = opt(&rest, "-o").or(opt(&rest, "--out")).unwrap_or("screenshot.png");
            let scale: f32 = opt(&rest, "--scale").and_then(|v| v.parse().ok()).unwrap_or(1.0);
            let full = rest.iter().any(|a| a == "--full-page");
            let t0 = std::time::Instant::now();
            let canvas = automation::screenshot(&mut tab.page, full, scale);
            if let Err(e) = automation::save_png(&canvas, Path::new(out)) {
                eprintln!("{e}");
                return 1;
            }
            print_console(&tab);
            eprintln!("Rendered {} → {} ({}x{}, {:?})", tab.url(), out, canvas.w, canvas.h, t0.elapsed());
            0
        }
        "test" => {
            let pos = positional(&rest);
            let files = if pos.is_empty() { vec!["tests".to_string()] } else { pos };
            automation::run_cli(&files, opt(&rest, "--junit"), opt(&rest, "--screenshots"), rest.iter().any(|a| a == "--verbose"))
        }
        "dump" => {
            let pos = positional(&rest);
            let (Some(what), Some(url)) = (pos.first(), pos.get(1)) else {
                eprintln!("usage: tip dump dom|layout|text URL");
                return 2;
            };
            let Some(mut tab) = load_tab(url, &rest) else { return 1 };
            match what.as_str() {
                "dom" => print!("{}", tab.page.host().doc.dump()),
                "layout" => {
                    let l = tab.page.host_mut().layout();
                    let mut out = String::new();
                    if let Some(r) = &l.root {
                        tip::layout::dump(r, 0, &tab.page.host().doc, &mut out);
                    }
                    print!("{out}");
                }
                _ => {
                    let body = tab.page.host().doc.body().unwrap_or(0);
                    let v = tab.page.eval("document.body ? document.body.innerText : ''", "dump");
                    match v {
                        Ok(tip::js::Value::Str(s)) => println!("{s}"),
                        _ => println!("{}", tab.page.text_of(body)),
                    }
                }
            }
            print_console(&tab);
            0
        }
        "image" => {
            // decode an image file with the built-in decoders (debugging aid)
            let pos = positional(&rest);
            let Some(input) = pos.first() else {
                eprintln!("usage: tip image FILE [-o out.png]");
                return 2;
            };
            let data = match std::fs::read(input) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("{input}: {e}");
                    return 1;
                }
            };
            let t0 = std::time::Instant::now();
            match tip::image::decode(&data) {
                Ok(img) => {
                    let ms = t0.elapsed();
                    // composite over white like a page background
                    let rgb: Vec<u8> = img.data.chunks(4).flat_map(|p| {
                        let a = p[3] as u32;
                        [0, 1, 2].map(|i| ((p[i] as u32 * a + 255 * (255 - a)) / 255) as u8)
                    }).collect();
                    let out = opt(&rest, "-o").unwrap_or("image.png");
                    if let Err(e) = std::fs::write(out, tip::image::png::encode_rgb(img.w, img.h, &rgb)) {
                        eprintln!("{out}: {e}");
                        return 1;
                    }
                    eprintln!("{input}: {}x{} decoded in {ms:?} -> {out}", img.w, img.h);
                    0
                }
                Err(e) => {
                    eprintln!("{input}: {e}");
                    1
                }
            }
        }
        "eval" => {
            let pos = positional(&rest);
            let (Some(url), Some(code)) = (pos.first(), pos.get(1)) else {
                eprintln!("usage: tip eval URL CODE");
                return 2;
            };
            let Some(mut tab) = load_tab(url, &rest) else { return 1 };
            let r = tab.page.eval(code, "eval");
            automation::settle(&mut tab, 200);
            print_console(&tab);
            match r {
                Ok(v) => {
                    let s = tab.page.interp.inspect(&v, 4);
                    println!("{s}");
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        "js" => {
            let Some(file) = rest.first() else {
                eprintln!("usage: tip js FILE");
                return 2;
            };
            let src = match std::fs::read_to_string(file) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{file}: {e}");
                    return 1;
                }
            };
            let mut it = tip::js::Interp::new();
            let r = it.eval_script(&src, file);
            it.run_microtasks();
            for m in it.console.drain(..) {
                if m.level == "error" || m.level == "warn" {
                    eprintln!("{}", m.text);
                } else {
                    println!("{}", m.text);
                }
            }
            match r {
                Ok(_) => 0,
                Err(e) => {
                    let msg = it.describe_error(&e);
                    eprintln!("Uncaught {msg} ({}:{})", file, it.cur_line + 1);
                    1
                }
            }
        }
        _ => {
            let url = if cmd.is_empty() { "tip://home".to_string() } else { cmd.to_string() };
            tip::app::run(&url)
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let windowed = args.is_empty() || !matches!(args[0].as_str(), "help" | "--help" | "-h" | "version" | "--version" | "-V" | "render" | "test" | "dump" | "eval" | "js" | "image");
    if !windowed {
        attach_console();
    }
    // Raise the JS stack budget to match the main-thread stack size set at link time (see build.rs).
    tip::js::interp::MAIN_STACK_LIMIT.store(200 * 1024 * 1024, std::sync::atomic::Ordering::Relaxed);
    let code = if windowed {
        run(args)
    } else {
        // Headless commands run on a large-stack thread (portable across platforms).
        std::thread::Builder::new().stack_size(256 << 20).spawn(move || run(args)).expect("spawn").join().unwrap_or(101)
    };
    std::process::exit(code);
}
