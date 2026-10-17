//! Resource loading: http(s), file, data and internal URLs.

pub mod http;
pub mod os_tls;
pub mod url;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub use url::Url;

pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Tip) TipEngine/0.1 (KHTML, like Gecko)";
pub const MAX_BODY: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub fn get(url: Url) -> Request {
        Request { method: "GET".into(), url, headers: vec![], body: None }
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub url: String,
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub from_os: bool,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// (mime type, charset)
    pub fn mime(&self) -> (String, Option<String>) {
        let ct = self.header("content-type").unwrap_or("").to_ascii_lowercase();
        let mut parts = ct.split(';');
        let mime = parts.next().unwrap_or("").trim().to_string();
        let charset = parts.find_map(|p| p.trim().strip_prefix("charset=").map(|c| c.trim_matches('"').to_string()));
        (mime, charset)
    }

    pub fn text(&self) -> String {
        decode_text(&self.body, self.mime().1.as_deref())
    }
}

/// Decode bytes using a declared charset (UTF-8, Latin-1/Windows-1252 supported).
pub fn decode_text(body: &[u8], charset: Option<&str>) -> String {
    let body = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
    match charset.map(|c| c.to_ascii_lowercase()) {
        Some(c) if c == "iso-8859-1" || c == "latin1" || c == "windows-1252" || c == "us-ascii" => {
            if let Ok(s) = std::str::from_utf8(body) {
                return s.to_string();
            }
            body.iter().map(|&b| b as char).collect()
        }
        _ => match std::str::from_utf8(body) {
            Ok(s) => s.to_string(),
            Err(_) => {
                // Heuristic: treat invalid UTF-8 as Windows-1252.
                if String::from_utf8_lossy(body).matches('\u{fffd}').count() > 4 {
                    body.iter().map(|&b| b as char).collect()
                } else {
                    String::from_utf8_lossy(body).into_owned()
                }
            }
        },
    }
}

// ---------------------------------------------------------------- cookies

type Jar = HashMap<String, Vec<(String, String)>>;
static COOKIES: OnceLock<Mutex<Jar>> = OnceLock::new();

fn jar() -> std::sync::MutexGuard<'static, Jar> {
    COOKIES.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner())
}

fn host_matches(host: &str, domain: &str) -> bool {
    let d = domain.trim_start_matches('.');
    host == d || host.ends_with(&format!(".{d}"))
}

pub fn store_cookie(url: &Url, header: &str) {
    let mut parts = header.split(';');
    let Some((name, value)) = parts.next().and_then(|nv| nv.split_once('=')) else { return };
    let mut domain = url.host.clone();
    let mut expired = false;
    for attr in parts {
        let (k, v) = attr.split_once('=').unwrap_or((attr, ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "domain" => {
                let d = v.trim().trim_start_matches('.').to_ascii_lowercase();
                if host_matches(&url.host, &d) {
                    domain = d;
                }
            }
            "max-age" => expired = v.trim().parse::<i64>().is_ok_and(|n| n <= 0),
            _ => {}
        }
    }
    let mut j = jar();
    let list = j.entry(domain).or_default();
    list.retain(|(n, _)| n != name.trim());
    if !expired {
        list.push((name.trim().to_string(), value.trim().to_string()));
    }
}

pub fn cookies_for(url: &Url) -> Option<String> {
    let j = jar();
    let mut out = Vec::new();
    for (domain, list) in j.iter() {
        if host_matches(&url.host, domain) {
            for (n, v) in list {
                out.push(format!("{n}={v}"));
            }
        }
    }
    if out.is_empty() { None } else { Some(out.join("; ")) }
}

/// document.cookie for a URL.
pub fn document_cookie(url: &Url) -> String {
    cookies_for(url).unwrap_or_default()
}

pub fn set_document_cookie(url: &Url, s: &str) {
    store_cookie(url, s);
}

// ---------------------------------------------------------------- base64

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

// ---------------------------------------------------------------- dispatch

fn ok(url: &Url, mime: &str, body: Vec<u8>) -> Response {
    Response { url: url.to_string(), status: 200, status_text: "OK".into(), headers: vec![("Content-Type".into(), mime.into())], body, from_os: false }
}

pub fn mime_for_path(p: &str) -> &'static str {
    let ext = p.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "txt" | "md" | "rs" | "toml" => "text/plain; charset=utf-8",
        "xml" => "text/xml",
        _ => "application/octet-stream",
    }
}

fn data_url(url: &Url) -> Result<Response, String> {
    let body = &url.path;
    let (meta, data) = body.split_once(',').ok_or("malformed data: URL")?;
    let is_b64 = meta.ends_with(";base64");
    let mime = meta.trim_end_matches(";base64");
    let mime = if mime.is_empty() { "text/plain" } else { mime };
    let bytes = if is_b64 {
        base64_decode(&url::percent_decode(data)).ok_or("invalid base64 in data: URL")?
    } else {
        url::percent_decode(data).into_bytes()
    };
    Ok(ok(url, mime, bytes))
}

fn file_url(url: &Url) -> Result<Response, String> {
    let path = url.file_path();
    let meta = std::fs::metadata(&path).map_err(|e| format!("{path}: {e}"))?;
    if meta.is_dir() {
        let mut entries: Vec<String> = std::fs::read_dir(&path)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() { format!("{name}/") } else { name }
            })
            .collect();
        entries.sort();
        let mut html = format!("<!doctype html><title>Index of {p}</title><style>body{{font-family:sans-serif;margin:24px}}a{{text-decoration:none}}li{{margin:3px 0}}</style><h1>Index of {p}</h1><ul>", p = crate::dom::escape_text(&path));
        if path != "/" {
            html.push_str("<li><a href=\"../\">../</a></li>");
        }
        for e in entries {
            html.push_str(&format!("<li><a href=\"{}\">{}</a></li>", crate::dom::escape_attr(&e), crate::dom::escape_text(&e)));
        }
        html.push_str("</ul>");
        return Ok(ok(url, "text/html; charset=utf-8", html.into_bytes()));
    }
    let body = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    Ok(ok(url, mime_for_path(&path), body))
}

// ---------------------------------------------------------------- prefetch cache

static PREFETCH: OnceLock<Mutex<HashMap<String, Result<Response, String>>>> = OnceLock::new();

fn prefetch_map() -> std::sync::MutexGuard<'static, HashMap<String, Result<Response, String>>> {
    PREFETCH.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap_or_else(|e| e.into_inner())
}

/// Store a response so the next GET of `url` is served from memory.
pub fn put_prefetched(url: &str, r: Result<Response, String>) {
    let mut m = prefetch_map();
    if m.len() > 512 {
        m.clear();
    }
    m.insert(url.to_string(), r);
}

/// Fetch subresources in parallel ahead of page construction.
pub fn prefetch_all(urls: Vec<Url>) {
    let handles: Vec<_> = urls
        .into_iter()
        .take(64)
        .map(|u| {
            std::thread::spawn(move || {
                let r = fetch_uncached(&Request::get(u.clone()));
                put_prefetched(&u.to_string(), r);
            })
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
}

/// Fetch a resource, following redirects (consults the prefetch cache for GETs).
pub fn fetch(req: &Request) -> Result<Response, String> {
    if req.method == "GET" && req.body.is_none() {
        if let Some(r) = prefetch_map().remove(&req.url.to_string()) {
            return r;
        }
    }
    fetch_uncached(req)
}

fn fetch_uncached(req: &Request) -> Result<Response, String> {
    let mut req = req.clone();
    for _ in 0..10 {
        let url = req.url.clone();
        let resp = match url.scheme.as_str() {
            "data" => return data_url(&url),
            "file" => return file_url(&url),
            "about" => return Ok(ok(&url, "text/html; charset=utf-8", crate::pages::about(&url.path).into_bytes())),
            "tip" => {
                let key = format!("{}{}", url.host, url.path);
                return match crate::pages::internal(&key) {
                    Some((mime, body)) => Ok(ok(&url, mime, body)),
                    None => Err(format!("Unknown internal page tip://{key}")),
                };
            }
            "http" => http::send(&req, &url)?,
            "https" => {
                let r = os_tls::fetch(&req, &url)?;
                return Ok(r);
            }
            s => return Err(format!("Unsupported URL scheme '{s}:'")),
        };
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(loc) = resp.header("location") {
                let next = url.join(loc).ok_or("bad redirect location")?;
                if resp.status == 303 || ((resp.status == 301 || resp.status == 302) && req.method == "POST") {
                    req.method = "GET".into();
                    req.body = None;
                }
                req.url = next;
                continue;
            }
        }
        return Ok(resp);
    }
    Err("Too many redirects".into())
}

/// A record of a completed request, for DevTools' Network panel.
#[derive(Debug, Clone)]
pub struct NetLogEntry {
    pub url: String,
    pub method: String,
    pub kind: &'static str,
    pub status: u16,
    pub size: usize,
    pub mime: String,
    pub ms: u128,
    pub error: Option<String>,
}

pub fn fetch_logged(req: &Request, kind: &'static str, log: &mut Vec<NetLogEntry>) -> Result<Response, String> {
    let t0 = Instant::now();
    let r = fetch(req);
    let ms = t0.elapsed().as_millis();
    match &r {
        Ok(resp) => log.push(NetLogEntry { url: req.url.to_string(), method: req.method.clone(), kind, status: resp.status, size: resp.body.len(), mime: resp.mime().0, ms, error: None }),
        Err(e) => log.push(NetLogEntry { url: req.url.to_string(), method: req.method.clone(), kind, status: 0, size: 0, mime: String::new(), ms, error: Some(e.clone()) }),
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        let data = b"Hello, Tip! \x00\xff";
        assert_eq!(base64_decode(&base64_encode(data)).unwrap(), data);
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }

    #[test]
    fn local_http_server() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for (i, stream) in listener.incoming().take(2).enumerate() {
                let mut s = stream.unwrap();
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                if i == 0 {
                    s.write_all(b"HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\n\r\n").unwrap();
                } else {
                    s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nSet-Cookie: a=1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n").unwrap();
                }
            }
        });
        let u = Url::parse(&format!("http://127.0.0.1:{port}/start")).unwrap();
        let r = fetch(&Request::get(u)).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.text(), "hello world");
        assert!(r.url.ends_with("/final"));
    }
}
