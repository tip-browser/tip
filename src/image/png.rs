//! PNG decoder and encoder.

use super::inflate::{zlib_compress, zlib_decompress};
use crate::raster::Image;

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (n, slot) in t.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb88320 ^ (c >> 1) } else { c >> 1 };
        }
        *slot = c;
    }
    t
}

pub fn crc32(data: &[u8]) -> u32 {
    let t = crc_table();
    let mut c = 0xffffffffu32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    c ^ 0xffffffff
}

pub fn decode(data: &[u8]) -> Result<Image, String> {
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("not a PNG".into());
    }
    let mut p = 8;
    let (mut w, mut h, mut depth, mut ctype, mut interlace) = (0u32, 0u32, 0u8, 0u8, 0u8);
    let mut idat = Vec::new();
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut trns: Option<Vec<u8>> = None;
    while p + 8 <= data.len() {
        let len = u32::from_be_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
        let ty = &data[p + 4..p + 8];
        let body = data.get(p + 8..p + 8 + len).ok_or("truncated chunk")?;
        match ty {
            b"IHDR" => {
                w = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                h = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                depth = body[8];
                ctype = body[9];
                interlace = body[12];
            }
            b"PLTE" => {
                palette = body.chunks(3).map(|c| [c[0], c[1], c[2], 255]).collect();
            }
            b"tRNS" => trns = Some(body.to_vec()),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        p += 12 + len;
    }
    if w == 0 || h == 0 || (w as u64 * h as u64) > 64_000_000 {
        return Err("bad PNG dimensions".into());
    }
    if let Some(t) = &trns {
        if ctype == 3 {
            for (i, a) in t.iter().enumerate() {
                if let Some(pe) = palette.get_mut(i) {
                    pe[3] = *a;
                }
            }
        }
    }
    let raw = zlib_decompress(&idat)?;
    let channels = match ctype {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return Err("unsupported color type".into()),
    };
    let bpp_bits = channels * depth as usize;
    let bpp = bpp_bits.div_ceil(8).max(1);
    let mut out = vec![0u8; (w * h * 4) as usize];

    let trns_key: Option<[u16; 3]> = trns.as_ref().and_then(|t| match ctype {
        0 if t.len() >= 2 => Some([u16::from_be_bytes([t[0], t[1]]); 3]),
        2 if t.len() >= 6 => Some([u16::from_be_bytes([t[0], t[1]]), u16::from_be_bytes([t[2], t[3]]), u16::from_be_bytes([t[4], t[5]])]),
        _ => None,
    });

    let passes: Vec<(usize, usize, usize, usize)> = if interlace == 1 {
        vec![(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
    } else {
        vec![(0, 0, 1, 1)]
    };
    let mut pos = 0;
    for (x0, y0, dx, dy) in passes {
        let pw = (w as usize).saturating_sub(x0).div_ceil(dx);
        let ph = (h as usize).saturating_sub(y0).div_ceil(dy);
        if pw == 0 || ph == 0 {
            continue;
        }
        let stride = (pw * bpp_bits).div_ceil(8);
        let mut prev = vec![0u8; stride];
        let mut cur = vec![0u8; stride];
        for row in 0..ph {
            let filter = *raw.get(pos).ok_or("truncated image data")?;
            let line = raw.get(pos + 1..pos + 1 + stride).ok_or("truncated image data")?;
            pos += 1 + stride;
            for i in 0..stride {
                let a = if i >= bpp { cur[i - bpp] as i16 } else { 0 };
                let b = prev[i] as i16;
                let c = if i >= bpp { prev[i - bpp] as i16 } else { 0 };
                let x = line[i];
                cur[i] = match filter {
                    0 => x,
                    1 => x.wrapping_add(a as u8),
                    2 => x.wrapping_add(b as u8),
                    3 => x.wrapping_add(((a + b) / 2) as u8),
                    4 => {
                        let p = a + b - c;
                        let pa = (p - a).abs();
                        let pb = (p - b).abs();
                        let pc = (p - c).abs();
                        let pr = if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c };
                        x.wrapping_add(pr as u8)
                    }
                    _ => return Err("bad filter".into()),
                };
            }
            let y = y0 + row * dy;
            for col in 0..pw {
                let x = x0 + col * dx;
                let sample = |ch: usize| -> u16 {
                    let bit = (col * channels + ch) * depth as usize;
                    match depth {
                        16 => u16::from_be_bytes([cur[bit / 8], cur[bit / 8 + 1]]),
                        8 => cur[bit / 8] as u16,
                        _ => {
                            let byte = cur[bit / 8];
                            let shift = 8 - depth as usize - (bit % 8);
                            ((byte >> shift) & ((1 << depth) - 1)) as u16
                        }
                    }
                };
                let to8 = |v: u16| -> u8 {
                    match depth {
                        16 => (v >> 8) as u8,
                        8 => v as u8,
                        d => (v * 255 / ((1 << d) - 1)) as u8,
                    }
                };
                let px = match ctype {
                    0 => {
                        let v = sample(0);
                        let a = if trns_key.is_some_and(|k| k[0] == v) { 0 } else { 255 };
                        let g = to8(v);
                        [g, g, g, a]
                    }
                    2 => {
                        let (r, g, b) = (sample(0), sample(1), sample(2));
                        let a = if trns_key.is_some_and(|k| k == [r, g, b]) { 0 } else { 255 };
                        [to8(r), to8(g), to8(b), a]
                    }
                    3 => *palette.get(sample(0) as usize).unwrap_or(&[0, 0, 0, 255]),
                    4 => {
                        let g = to8(sample(0));
                        [g, g, g, to8(sample(1))]
                    }
                    _ => [to8(sample(0)), to8(sample(1)), to8(sample(2)), to8(sample(3))],
                };
                let o = (y * w as usize + x) * 4;
                out[o..o + 4].copy_from_slice(&px);
            }
            std::mem::swap(&mut prev, &mut cur);
        }
    }
    Ok(Image { w, h, data: out })
}

fn chunk(out: &mut Vec<u8>, ty: &[u8], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let mut c = ty.to_vec();
    c.extend_from_slice(body);
    out.extend_from_slice(&c);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

/// Encode 8-bit RGB pixels as PNG.
pub fn encode_rgb(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    let stride = w as usize * 3;
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    // "Sub" filter improves compression for UI screenshots.
    for y in 0..h as usize {
        raw.push(1);
        let line = &rgb[y * stride..(y + 1) * stride];
        for i in 0..stride {
            let left = if i >= 3 { line[i - 3] } else { 0 };
            raw.push(line[i].wrapping_sub(left));
        }
    }
    chunk(&mut out, b"IDAT", &zlib_compress(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let (w, h) = (7u32, 5u32);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 37 % 256) as u8).collect();
        let png = encode_rgb(w, h, &rgb);
        let img = decode(&png).unwrap();
        assert_eq!((img.w, img.h), (w, h));
        for i in 0..(w * h) as usize {
            assert_eq!(&img.data[i * 4..i * 4 + 3], &rgb[i * 3..i * 3 + 3]);
        }
    }
}
