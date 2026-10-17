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
