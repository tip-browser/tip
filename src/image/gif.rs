//! GIF decoder (first frame): LZW, global/local palettes, transparency and interlacing.

use crate::raster::Image;

fn lzw_decode(data: &[u8], min_code: u8, max_out: usize) -> Result<Vec<u8>, String> {
    if !(1..=11).contains(&min_code) {
        return Err("bad LZW code size".into());
    }
    let clear = 1usize << min_code;
    let end = clear + 1;
    let mut size = min_code as u32 + 1;
    let mut next = clear + 2;
    // dictionary as (prefix, suffix, first byte, length)
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut first = vec![0u8; 4096];
    let mut length = vec![0u16; 4096];
    for i in 0..clear {
        suffix[i] = i as u8;
        first[i] = i as u8;
        length[i] = 1;
    }
    let mut out: Vec<u8> = Vec::with_capacity(max_out);
    let mut prev: Option<usize> = None;
    let (mut acc, mut nbits, mut pos) = (0u32, 0u32, 0usize);
    let mut stack = Vec::with_capacity(4096);
    loop {
        while nbits < size {
            let Some(&b) = data.get(pos) else { return Ok(out) };
            acc |= (b as u32) << nbits;
            nbits += 8;
            pos += 1;
        }
        let code = (acc & ((1 << size) - 1)) as usize;
        acc >>= size;
        nbits -= size;
        if code == clear {
            size = min_code as u32 + 1;
            next = clear + 2;
            prev = None;
            continue;
        }
        if code == end {
            break;
        }
        let emit_code;
        match prev {
            None => {
                if code >= clear {
                    return Err("bad LZW stream".into());
                }
                out.push(code as u8);
                prev = Some(code);
                continue;
            }
            Some(p) => {
                if code < next {
                    emit_code = code;
                    if next < 4096 {
                        prefix[next] = p as u16;
                        suffix[next] = first[code];
                        first[next] = first[p];
                        length[next] = length[p] + 1;
                        next += 1;
                    }
                } else if code == next && next < 4096 {
                    prefix[next] = p as u16;
                    suffix[next] = first[p];
                    first[next] = first[p];
                    length[next] = length[p] + 1;
                    next += 1;
                    emit_code = code;
                } else {
                    return Err("bad LZW code".into());
                }
            }
        }
        // expand the code
        stack.clear();
        let mut c = emit_code;
        loop {
            stack.push(suffix[c]);
            if length[c] <= 1 {
                break;
            }
            c = prefix[c] as usize;
        }
        out.extend(stack.iter().rev());
        prev = Some(emit_code);
        if next >= (1 << size) && size < 12 {
            size += 1;
        }
        if out.len() >= max_out {
            break;
        }
    }
    Ok(out)
}

fn sub_blocks(d: &[u8], pos: &mut usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let n = *d.get(*pos).ok_or("truncated GIF")? as usize;
        *pos += 1;
        if n == 0 {
            return Ok(out);
        }
        out.extend_from_slice(d.get(*pos..*pos + n).ok_or("truncated GIF")?);
        *pos += n;
    }
}

pub fn decode(d: &[u8]) -> Result<Image, String> {
    if d.len() < 13 || !(d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a")) {
        return Err("not a GIF".into());
    }
    let sw = u16::from_le_bytes([d[6], d[7]]) as usize;
    let sh = u16::from_le_bytes([d[8], d[9]]) as usize;
    let flags = d[10];
    let bg_index = d[11] as usize;
    let mut pos = 13;
    let mut global: Vec<[u8; 3]> = Vec::new();
    if flags & 0x80 != 0 {
        let n = 2usize << (flags & 7);
        for i in 0..n {
            let c = d.get(pos + i * 3..pos + i * 3 + 3).ok_or("truncated GIF palette")?;
            global.push([c[0], c[1], c[2]]);
        }
        pos += n * 3;
    }
    let _ = bg_index;
    let mut transparent: Option<usize> = None;
    while pos < d.len() {
        match d[pos] {
            0x21 => {
                let label = *d.get(pos + 1).ok_or("truncated GIF")?;
                pos += 2;
                let data = sub_blocks(d, &mut pos)?;
                if label == 0xf9 && data.len() >= 4 && data[0] & 1 != 0 {
                    transparent = Some(data[3] as usize);
                }
            }
            0x2c => {
                let b = d.get(pos + 1..pos + 10).ok_or("truncated GIF")?;
                let ix = u16::from_le_bytes([b[0], b[1]]) as usize;
                let iy = u16::from_le_bytes([b[2], b[3]]) as usize;
                let iw = u16::from_le_bytes([b[4], b[5]]) as usize;
                let ih = u16::from_le_bytes([b[6], b[7]]) as usize;
                let iflags = b[8];
                pos += 10;
                let mut palette = global.clone();
                if iflags & 0x80 != 0 {
                    let n = 2usize << (iflags & 7);
                    palette.clear();
                    for i in 0..n {
                        let c = d.get(pos + i * 3..pos + i * 3 + 3).ok_or("truncated GIF palette")?;
                        palette.push([c[0], c[1], c[2]]);
                    }
                    pos += n * 3;
                }
                let min_code = *d.get(pos).ok_or("truncated GIF")?;
                pos += 1;
                let data = sub_blocks(d, &mut pos)?;
                let (w, h) = (sw.max(ix + iw).max(1), sh.max(iy + ih).max(1));
                if w * h > 50_000_000 {
                    return Err("GIF too large".into());
                }
                let indices = lzw_decode(&data, min_code, iw * ih)?;
                let mut out = vec![0u8; w * h * 4];
                // interlaced rows come in four passes
                let rows: Vec<usize> = if iflags & 0x40 != 0 {
                    let mut r = Vec::with_capacity(ih);
                    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
                        r.extend((start..ih).step_by(step));
                    }
                    r
                } else {
                    (0..ih).collect()
                };
                for (i, &ry) in rows.iter().enumerate() {
                    for x in 0..iw {
                        let Some(&idx) = indices.get(i * iw + x) else { break };
                        let idx = idx as usize;
                        if Some(idx) == transparent {
                            continue;
                        }
                        let Some(c) = palette.get(idx) else { continue };
                        let (px, py) = (ix + x, iy + ry);
                        if px < w && py < h {
                            let o = (py * w + px) * 4;
                            out[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
                        }
                    }
                }
                return Ok(Image { w: w as u32, h: h as u32, data: out });
            }
            0x3b => break,
            _ => return Err("bad GIF block".into()),
        }
    }
    Err("GIF has no image".into())
}
