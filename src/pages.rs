//! Built-in pages (tip:// and about:) and the HTML for the browser's own UI.

pub const HOME: &str = include_str!("pages/home.html");
pub const CHROME: &str = include_str!("pages/chrome.html");
pub const TABLIST: &str = include_str!("pages/tablist.html");
pub const DEVTOOLS: &str = include_str!("pages/devtools.html");

pub fn about(path: &str) -> String {
    match path {
        "blank" | "" => String::new(),
        "version" | "tip" => format!(
            "<!doctype html><title>About Tip</title><body style=\"font-family:sans-serif;margin:40px\"><h1>Tip {}</h1><p>A web browser engine written from scratch in Rust.</p><p>Platform: {} / {}</p></body>",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
        _ => format!("<!doctype html><title>about:{0}</title><p>Unknown page about:{0}</p>", crate::dom::escape_text(path)),
    }
}

pub fn internal(key: &str) -> Option<(&'static str, Vec<u8>)> {
    let key = key.trim_end_matches('/');
    match key {
        "home" | "newtab" | "" => Some(("text/html; charset=utf-8", HOME.as_bytes().to_vec())),
        "about" => Some(("text/html; charset=utf-8", about("version").into_bytes())),
        "examples" => Some(("text/html; charset=utf-8", examples_index().into_bytes())),
        k if k.starts_with("examples/") => {
            let name = &k["examples/".len()..];
            EXAMPLES.iter().find(|(n, _)| *n == name).map(|(_, src)| ("text/html; charset=utf-8", src.as_bytes().to_vec()))
        }
        _ => None,
    }
}

pub const EXAMPLES: &[(&str, &str)] = &[
    ("todo.html", include_str!("../examples/todo.html")),
    ("basic.html", include_str!("../examples/basic.html")),
    ("layout.html", include_str!("../examples/layout.html")),
    ("clock.html", include_str!("../examples/clock.html")),
    ("forms.html", include_str!("../examples/forms.html")),
];

fn examples_index() -> String {
    let mut s = String::from("<!doctype html><title>Examples</title><style>body{font-family:sans-serif;margin:40px;max-width:640px}li{margin:8px 0}</style><h1>Examples</h1><ul>");
    for (n, _) in EXAMPLES {
        s.push_str(&format!("<li><a href=\"tip://examples/{n}\">{n}</a></li>"));
    }
    s.push_str("</ul>");
    s
}
