//! HTTP/1.1 client over plain TCP.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::url::Url;
use super::{Request, Response};

fn read_line(r: &mut impl BufRead) -> Result<String, String> {
    let mut buf = Vec::new();
    r.read_until(b'\n', &mut buf).map_err(|e| e.to_string())?;
    if buf.len() > 64 * 1024 {
        return Err("header line too long".into());
    }
    Ok(String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string())
}

/// Perform a single request (no redirect handling).
pub fn send(req: &Request, url: &Url) -> Result<Response, String> {
    let host = url.host.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = url.port_or_default();
    let addrs: Vec<_> = (host.as_str(), port).to_socket_addrs().map_err(|e| format!("DNS lookup failed for {host}: {e}"))?.collect();
    let mut last_err = String::from("no addresses");
    let mut stream = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, Duration::from_secs(15)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    let mut stream = stream.ok_or_else(|| format!("Could not connect to {host}:{port}: {last_err}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(30))).ok();

    let mut head = format!("{} {} HTTP/1.1\r\nHost: {}\r\n", req.method, url.path_and_query(), url.host_port());
    let has = |name: &str| req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if !has("user-agent") {
        head.push_str(&format!("User-Agent: {}\r\n", super::USER_AGENT));
    }
    if !has("accept") {
        head.push_str("Accept: */*\r\n");
    }
    if !has("accept-encoding") {
        head.push_str("Accept-Encoding: gzip, deflate\r\n");
    }
    head.push_str("Connection: close\r\n");
    for (k, v) in &req.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(c) = super::cookies_for(url) {
        if !has("cookie") {
            head.push_str(&format!("Cookie: {c}\r\n"));
        }
    }
    if let Some(b) = &req.body {
        head.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    if let Some(b) = &req.body {
        stream.write_all(b).map_err(|e| e.to_string())?;
    }

    let mut r = BufReader::new(stream);
    let status_line = read_line(&mut r)?;
    let mut parts = status_line.splitn(3, ' ');
    let proto = parts.next().unwrap_or("");
    if !proto.starts_with("HTTP/") {
        return Err(format!("Invalid HTTP response: {status_line:?}"));
    }
    let status: u16 = parts.next().and_then(|s| s.parse().ok()).ok_or("bad status code")?;
    let status_text = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    loop {
        let l = read_line(&mut r)?;
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        if headers.len() > 500 {
            return Err("too many headers".into());
        }
    }
    let header = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
    let mut body = Vec::new();
    let no_body = req.method == "HEAD" || status == 204 || status == 304 || (100..200).contains(&status);
    if !no_body {
        if header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
            loop {
                let size_line = read_line(&mut r)?;
                let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("0").trim(), 16).map_err(|_| "bad chunk size")?;
                if size == 0 {
                    // trailers
                    loop {
                        let l = read_line(&mut r)?;
                        if l.is_empty() {
                            break;
                        }
                    }
                    break;
                }
                let mut chunk = vec![0u8; size];
                r.read_exact(&mut chunk).map_err(|e| e.to_string())?;
                body.extend_from_slice(&chunk);
                let _ = read_line(&mut r)?;
                if body.len() > super::MAX_BODY {
                    return Err("response too large".into());
                }
            }
        } else if let Some(len) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
            if len > super::MAX_BODY {
                return Err("response too large".into());
            }
            body = vec![0u8; len];
            r.read_exact(&mut body).map_err(|e| e.to_string())?;
        } else {
            r.take(super::MAX_BODY as u64).read_to_end(&mut body).map_err(|e| e.to_string())?;
        }
    }
    if let Some(enc) = header("content-encoding") {
        let enc = enc.to_ascii_lowercase();
        if enc.contains("gzip") {
            body = crate::image::inflate::gzip_decompress(&body)?;
        } else if enc.contains("deflate") {
            body = crate::image::inflate::zlib_decompress(&body).or_else(|_| crate::image::inflate::inflate(&body))?;
        } else if enc.contains("br") {
            return Err("brotli content-encoding is not supported".into());
        }
    }
    for (k, v) in &headers {
        if k.eq_ignore_ascii_case("set-cookie") {
            super::store_cookie(url, v);
        }
    }
    Ok(Response { url: url.to_string(), status, status_text, headers, body, from_os: false })
}

/// Events from a streaming response.
pub enum StreamMsg<'a> {
    Head { status: u16, headers: &'a [(String, String)] },
    Data(&'a [u8]),
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Perform a request and deliver the body incrementally (no decompression, no redirects).
/// The callback returns `false` to stop. Returns when the body ends, on error, or when
/// `cancel` is set.
pub fn stream(req: &Request, url: &Url, cancel: &std::sync::atomic::AtomicBool, on: &mut dyn FnMut(StreamMsg) -> bool) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let host = url.host.trim_start_matches('[').trim_end_matches(']').to_string();
    let port = url.port_or_default();
    let addrs: Vec<_> = (host.as_str(), port).to_socket_addrs().map_err(|e| format!("DNS lookup failed for {host}: {e}"))?.collect();
    let mut stream = None;
    for a in addrs {
        if let Ok(s) = TcpStream::connect_timeout(&a, Duration::from_secs(15)) {
            stream = Some(s);
            break;
        }
    }
    let mut stream = stream.ok_or_else(|| format!("Could not connect to {host}:{port}"))?;
    stream.set_read_timeout(Some(Duration::from_millis(250))).ok();
    let mut head = format!("{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept-Encoding: identity\r\nConnection: close\r\n", req.method, url.path_and_query(), url.host_port(), super::USER_AGENT);
    for (k, v) in &req.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(c) = super::cookies_for(url) {
        head.push_str(&format!("Cookie: {c}\r\n"));
    }
    if let Some(b) = &req.body {
        head.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    if let Some(b) = &req.body {
        stream.write_all(b).map_err(|e| e.to_string())?;
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 16 * 1024];
    let mut headers: Option<Vec<(String, String)>> = None;
    let mut chunked = false;
    let mut remaining: Option<usize> = None; // content-length
    let mut chunk_left = 0usize; // bytes left in the current chunk
    let mut eof = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        if !eof {
            match stream.read(&mut tmp) {
                Ok(0) => eof = true,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
        if headers.is_none() {
            let Some(end) = find(&buf, b"\r\n\r\n") else {
                if eof {
                    return Err("connection closed before response headers".into());
                }
                continue;
            };
            let text = String::from_utf8_lossy(&buf[..end]).to_string();
            buf.drain(..end + 4);
            let mut lines = text.split("\r\n");
            let status_line = lines.next().unwrap_or("");
            let status: u16 = status_line.split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or("bad status line")?;
            let hs: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
            for (k, v) in &hs {
                if k.eq_ignore_ascii_case("set-cookie") {
                    super::store_cookie(url, v);
                }
                if k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked") {
                    chunked = true;
                }
                if k.eq_ignore_ascii_case("content-length") {
                    remaining = v.parse().ok();
                }
            }
            if !on(StreamMsg::Head { status, headers: &hs }) {
                return Ok(());
            }
            headers = Some(hs);
        }
        // body
        if chunked {
            loop {
                if chunk_left == 0 {
                    let Some(nl) = find(&buf, b"\r\n") else { break };
                    let line = String::from_utf8_lossy(&buf[..nl]).to_string();
                    buf.drain(..nl + 2);
                    if line.is_empty() {
                        continue; // CRLF after a chunk
                    }
                    let size = usize::from_str_radix(line.split(';').next().unwrap_or("0").trim(), 16).map_err(|_| "bad chunk size")?;
                    if size == 0 {
                        return Ok(());
                    }
                    chunk_left = size;
                }
                if buf.is_empty() {
                    break;
                }
                let n = chunk_left.min(buf.len());
                let piece: Vec<u8> = buf.drain(..n).collect();
                chunk_left -= n;
                if !on(StreamMsg::Data(&piece)) {
                    return Ok(());
                }
            }
        } else if !buf.is_empty() {
            let n = remaining.map_or(buf.len(), |r| r.min(buf.len()));
            let piece: Vec<u8> = buf.drain(..n).collect();
            if let Some(r) = &mut remaining {
                *r -= n;
            }
            if !on(StreamMsg::Data(&piece)) {
                return Ok(());
            }
        }
        if remaining == Some(0) || eof {
            return Ok(());
        }
    }
}
