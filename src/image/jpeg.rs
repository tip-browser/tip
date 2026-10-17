//! JPEG decoder: baseline and progressive Huffman-coded images, any chroma subsampling,
//! restart intervals, grayscale / YCbCr / RGB / CMYK / YCCK, and EXIF orientation.

use crate::raster::Image;

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15,
    23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

#[derive(Clone, Default)]
struct Huffman {
    /// Fast path: (symbol, code length) for the next 9 bits; length 0 = slow path.
    fast: Vec<(u8, u8)>,
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    vals: Vec<u8>,
}

const FAST_BITS: u32 = 9;

impl Huffman {
    fn new(counts: &[u8; 16], vals: Vec<u8>) -> Result<Huffman, String> {
        let mut h = Huffman { fast: vec![(0, 0); 1 << FAST_BITS], maxcode: [-1; 18], valptr: [0; 17], mincode: [0; 17], vals };
        let mut code: i32 = 0;
        let mut k: i32 = 0;
        for len in 1..=16usize {
            let n = counts[len - 1] as i32;
            h.valptr[len] = k;
            h.mincode[len] = code;
            if n > 0 {
                // fill the fast table for short codes
                if len as u32 <= FAST_BITS {
                    for i in 0..n {
                        let c = (code + i) as usize;
                        let shift = FAST_BITS - len as u32;
                        for j in 0..(1usize << shift) {
                            let idx = (c << shift) | j;
                            if idx < h.fast.len() {
                                let sym = *h.vals.get((k + i) as usize).ok_or("bad Huffman table")?;
                                h.fast[idx] = (sym, len as u8);
                            }
                        }
                    }
                }
                code += n;
                k += n;
                h.maxcode[len] = code - 1;
            }
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        if k as usize > h.vals.len() {
            return Err("bad Huffman table".into());
        }
        Ok(h)
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
    /// Hit a marker: further reads yield zeros.
    marker: bool,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], pos: usize) -> Bits<'a> {
        Bits { data, pos, acc: 0, n: 0, marker: false }
    }

    fn fill(&mut self) {
        while self.n <= 24 {
            let mut byte = 0u32;
            if !self.marker && self.pos < self.data.len() {
                let b = self.data[self.pos];
                if b == 0xff {
                    let next = self.data.get(self.pos + 1).copied().unwrap_or(0);
                    if next == 0 {
                        self.pos += 2;
                        byte = 0xff;
                    } else {
                        self.marker = true;
                    }
                } else {
                    self.pos += 1;
                    byte = b as u32;
                }
            }
            self.acc |= byte << (24 - self.n);
            self.n += 8;
        }
    }

    fn bit(&mut self) -> u32 {
        if self.n < 1 {
            self.fill();
        }
        let b = self.acc >> 31;
        self.acc <<= 1;
        self.n -= 1;
        b
    }

    fn bits(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        if self.n < count {
            self.fill();
        }
        let v = self.acc >> (32 - count);
        self.acc <<= count;
        self.n -= count;
        v
    }

    fn decode(&mut self, h: &Huffman) -> Result<u8, String> {
        if self.n < 16 {
            self.fill();
        }
        let peek = (self.acc >> (32 - FAST_BITS)) as usize;
        let (sym, len) = h.fast.get(peek).copied().unwrap_or((0, 0));
        if len > 0 {
            self.acc <<= len;
            self.n -= len as u32;
            return Ok(sym);
        }
        let mut code: i32 = 0;
        for len in 1..=16usize {
            code = (code << 1) | self.bit() as i32;
            if code <= h.maxcode[len] {
                let idx = h.valptr[len] + code - h.mincode[len];
                return h.vals.get(idx as usize).copied().ok_or_else(|| "bad Huffman code".to_string());
            }
        }
        Err("bad Huffman code".into())
    }

    /// Receive `s` bits and sign-extend as JPEG defines.
    fn receive_extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.bits(s) as i32;
        if v < (1 << (s - 1)) { v - (1 << s) + 1 } else { v }
    }

    /// Skip to the byte after the next RSTn marker.
    fn restart(&mut self) {
        self.acc = 0;
        self.n = 0;
        self.marker = false;
        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == 0xff && (0xd0..=0xd7).contains(&self.data[self.pos + 1]) {
                self.pos += 2;
                return;
            }
            if self.data[self.pos] == 0xff && self.data[self.pos + 1] != 0 && self.data[self.pos + 1] != 0xff {
                return; // some other marker: the restart is missing
            }
            self.pos += 1;
        }
    }
}

#[derive(Clone)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Blocks per line / column including MCU padding.
    bw: usize,
    bh: usize,
    coeffs: Vec<i32>,
    dc_table: usize,
    ac_table: usize,
    pred: i32,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    comps: Vec<Component>,
    hmax: usize,
    vmax: usize,
    mcux: usize,
    mcuy: usize,
}

fn be16(d: &[u8], i: usize) -> Result<usize, String> {
    Ok(((*d.get(i).ok_or("truncated JPEG")? as usize) << 8) | *d.get(i + 1).ok_or("truncated JPEG")? as usize)
}

pub fn decode(data: &[u8]) -> Result<Image, String> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return Err("not a JPEG".into());
    }
    let mut qt = [[0u16; 64]; 4];
    let mut dc_tables: Vec<Huffman> = vec![Huffman::default(); 4];
    let mut ac_tables: Vec<Huffman> = vec![Huffman::default(); 4];
    let mut frame: Option<Frame> = None;
    let mut restart_interval = 0usize;
    let mut adobe: Option<u8> = None;
    let mut orientation = 1u16;
    let mut pos = 2;
    while pos + 4 <= data.len() {
        if data[pos] != 0xff {
            pos += 1;
            continue;
        }
        let marker = data[pos + 1];
        pos += 2;
        match marker {
            0xd8 | 0x01 | 0xd0..=0xd7 | 0xff => {
                if marker == 0xff {
                    pos -= 1;
                }
                continue;
            }
            0xd9 => break,
            _ => {}
        }
        let len = be16(data, pos)?;
        let seg = data.get(pos + 2..pos + len).ok_or("truncated JPEG segment")?;
        match marker {
            0xdb => {
                let mut i = 0;
                while i < seg.len() {
                    let pq = seg[i] >> 4;
                    let tq = (seg[i] & 15) as usize;
                    i += 1;
                    if tq > 3 {
                        return Err("bad quantization table".into());
                    }
                    for k in 0..64 {
                        let v = if pq == 0 {
                            let v = *seg.get(i).ok_or("truncated DQT")? as u16;
                            i += 1;
                            v
                        } else {
                            let v = be16(seg, i)? as u16;
                            i += 2;
                            v
                        };
                        qt[tq][ZIGZAG[k]] = v;
                    }
                }
            }
            0xc4 => {
                let mut i = 0;
                while i + 17 <= seg.len() {
                    let tc = seg[i] >> 4;
                    let th = (seg[i] & 15) as usize;
                    let mut counts = [0u8; 16];
                    counts.copy_from_slice(&seg[i + 1..i + 17]);
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    let vals = seg.get(i + 17..i + 17 + total).ok_or("truncated DHT")?.to_vec();
                    i += 17 + total;
                    if th > 3 {
                        return Err("bad Huffman table id".into());
                    }
                    let h = Huffman::new(&counts, vals)?;
                    if tc == 0 {
                        dc_tables[th] = h;
                    } else {
                        ac_tables[th] = h;
                    }
                }
            }
            0xc0 | 0xc1 | 0xc2 => {
                if seg.len() < 6 {
                    return Err("bad SOF".into());
                }
                if seg[0] != 8 {
                    return Err(format!("{}-bit JPEG not supported", seg[0]));
                }
                let height = be16(seg, 1)?;
                let width = be16(seg, 3)?;
                let n = seg[5] as usize;
                if width == 0 || height == 0 || n == 0 || n > 4 || width * height > 100_000_000 {
                    return Err("unsupported JPEG dimensions".into());
                }
                let mut comps = Vec::new();
                for c in 0..n {
                    let b = seg.get(6 + c * 3..9 + c * 3).ok_or("truncated SOF")?;
                    let (h, v) = ((b[1] >> 4) as usize, (b[1] & 15) as usize);
                    if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
                        return Err("bad sampling factors".into());
                    }
                    comps.push(Component { id: b[0], h, v, tq: (b[2] & 3) as usize, bw: 0, bh: 0, coeffs: vec![], dc_table: 0, ac_table: 0, pred: 0 });
                }
                let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
                let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
                let mcux = width.div_ceil(8 * hmax);
                let mcuy = height.div_ceil(8 * vmax);
                for c in &mut comps {
                    c.bw = mcux * c.h;
                    c.bh = mcuy * c.v;
                    c.coeffs = vec![0; c.bw * c.bh * 64];
                }
                frame = Some(Frame { width, height, progressive: marker == 0xc2, comps, hmax, vmax, mcux, mcuy });
            }
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => return Err("unsupported JPEG coding (lossless or arithmetic)".into()),
            0xdd => restart_interval = be16(seg, 0)?,
            0xee => {
                if seg.len() >= 12 && &seg[..5] == b"Adobe" {
                    adobe = Some(seg[11]);
                }
            }
            0xe1 => {
                if seg.len() > 14 && &seg[..6] == b"Exif\0\0" {
                    orientation = exif_orientation(&seg[6..]).unwrap_or(1);
                }
            }
            0xda => {
                let f = frame.as_mut().ok_or("scan before frame")?;
                let ns = *seg.first().ok_or("bad SOS")? as usize;
                let mut scomp = Vec::new();
                for i in 0..ns {
                    let id = *seg.get(1 + i * 2).ok_or("bad SOS")?;
                    let t = *seg.get(2 + i * 2).ok_or("bad SOS")?;
                    let ci = f.comps.iter().position(|c| c.id == id).ok_or("scan references unknown component")?;
                    f.comps[ci].dc_table = (t >> 4) as usize & 3;
                    f.comps[ci].ac_table = (t & 15) as usize & 3;
                    scomp.push(ci);
                }
                let p = 1 + ns * 2;
                let ss = *seg.get(p).ok_or("bad SOS")? as usize;
                let se = *seg.get(p + 1).ok_or("bad SOS")? as usize;
                let a = *seg.get(p + 2).ok_or("bad SOS")?;
                let (ah, al) = ((a >> 4) as u32, (a & 15) as u32);
                let mut bits = Bits::new(data, pos + len);
                decode_scan(f, &scomp, &dc_tables, &ac_tables, ss, se.min(63), ah, al, restart_interval, &mut bits)?;
                // continue after the entropy-coded data
                pos = bits.pos;
                while pos + 1 < data.len() && !(data[pos] == 0xff && data[pos + 1] != 0 && !(0xd0..=0xd7).contains(&data[pos + 1])) {
                    pos += 1;
                }
                continue;
            }
            _ => {}
        }
        pos += len;
    }
    let f = frame.ok_or("JPEG has no frame")?;
    let img = to_rgba(&f, &qt, adobe)?;
    Ok(orient(img, orientation))
}

#[allow(clippy::too_many_arguments)]
fn decode_scan(f: &mut Frame, scomp: &[usize], dc: &[Huffman], ac: &[Huffman], ss: usize, se: usize, ah: u32, al: u32, ri: usize, bits: &mut Bits) -> Result<(), String> {
    for &c in scomp {
        f.comps[c].pred = 0;
    }
    let mut eobrun = 0u32;
    let progressive = f.progressive;
    let decode_block = |comp: &mut Component, bx: usize, by: usize, bits: &mut Bits, eobrun: &mut u32| -> Result<(), String> {
        let off = (by * comp.bw + bx) * 64;
        let blk = &mut comp.coeffs[off..off + 64];
        if !progressive {
            let t = bits.decode(&dc[comp.dc_table])? as u32;
            let diff = bits.receive_extend(t);
            comp.pred += diff;
            blk[0] = comp.pred;
            let table = &ac[comp.ac_table];
            let mut k = 1;
            while k < 64 {
                let rs = bits.decode(table)?;
                let (r, s) = ((rs >> 4) as usize, (rs & 15) as u32);
                if s == 0 {
                    if r == 15 {
                        k += 16;
                        continue;
                    }
                    break;
                }
                k += r;
                if k > 63 {
                    break;
                }
                blk[ZIGZAG[k]] = bits.receive_extend(s);
                k += 1;
            }
            return Ok(());
        }
        if ss == 0 {
            // DC scans
            if ah == 0 {
                let t = bits.decode(&dc[comp.dc_table])? as u32;
                let diff = bits.receive_extend(t);
                comp.pred += diff;
                blk[0] = comp.pred * (1 << al);
            } else if bits.bit() == 1 {
                blk[0] |= 1 << al;
            }
            return Ok(());
        }
        let table = &ac[comp.ac_table];
        if ah == 0 {
            // AC first pass
            if *eobrun > 0 {
                *eobrun -= 1;
                return Ok(());
            }
            let mut k = ss;
            while k <= se {
                let rs = bits.decode(table)?;
                let (r, s) = ((rs >> 4) as u32, (rs & 15) as u32);
                if s == 0 {
                    if r < 15 {
                        *eobrun = (1 << r) - 1;
                        if r > 0 {
                            *eobrun += bits.bits(r);
                        }
                        break;
                    }
                    k += 16;
                    continue;
                }
                k += r as usize;
                if k > 63 {
                    break;
                }
                blk[ZIGZAG[k]] = bits.receive_extend(s) * (1 << al);
                k += 1;
            }
            return Ok(());
        }
        // AC refinement
        let p1 = 1i32 << al;
        let m1 = -1i32 << al;
        let mut k = ss;
        if *eobrun == 0 {
            while k <= se {
                let rs = bits.decode(table)?;
                let (mut r, s) = ((rs >> 4) as i32, (rs & 15) as u32);
                let mut val = 0;
                if s != 0 {
                    val = if bits.bit() == 1 { p1 } else { m1 };
                } else if r != 15 {
                    *eobrun = 1 << r;
                    if r > 0 {
                        *eobrun += bits.bits(r as u32);
                    }
                    break;
                }
                while k <= se {
                    let z = ZIGZAG[k];
                    if blk[z] != 0 {
                        if bits.bit() == 1 && blk[z] & p1 == 0 {
                            blk[z] += if blk[z] >= 0 { p1 } else { m1 };
                        }
                    } else {
                        if r == 0 {
                            if val != 0 {
                                blk[z] = val;
                            }
                            k += 1;
                            break;
                        }
                        r -= 1;
                    }
                    k += 1;
                }
            }
        }
        if *eobrun > 0 {
            while k <= se {
                let z = ZIGZAG[k];
                if blk[z] != 0 && bits.bit() == 1 && blk[z] & p1 == 0 {
                    blk[z] += if blk[z] >= 0 { p1 } else { m1 };
                }
                k += 1;
            }
            *eobrun -= 1;
        }
        Ok(())
    };

    let mut since_restart = 0usize;
    let restart = |f: &mut Frame, bits: &mut Bits, eobrun: &mut u32, since: &mut usize| {
        if ri > 0 {
            *since += 1;
            if *since == ri {
                *since = 0;
                bits.restart();
                *eobrun = 0;
                for &c in scomp {
                    f.comps[c].pred = 0;
                }
            }
        }
    };
    if scomp.len() == 1 {
        // non-interleaved: the component's own block grid, without MCU padding
        let ci = scomp[0];
        let (cw, ch) = {
            let c = &f.comps[ci];
            ((f.width * c.h).div_ceil(f.hmax * 8), (f.height * c.v).div_ceil(f.vmax * 8))
        };
        for by in 0..ch {
            for bx in 0..cw {
                decode_block(&mut f.comps[ci], bx, by, bits, &mut eobrun)?;
                restart(f, bits, &mut eobrun, &mut since_restart);
            }
        }
    } else {
        for my in 0..f.mcuy {
            for mx in 0..f.mcux {
                for &ci in scomp {
                    let (h, v) = (f.comps[ci].h, f.comps[ci].v);
                    for yy in 0..v {
                        for xx in 0..h {
                            decode_block(&mut f.comps[ci], mx * h + xx, my * v + yy, bits, &mut eobrun)?;
                        }
                    }
                }
                restart(f, bits, &mut eobrun, &mut since_restart);
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- IDCT (integer, as in stb_image)

#[inline]
fn f2f(x: f32) -> i32 {
    (x * 4096.0 + 0.5) as i32
}

#[inline]
fn idct_1d(s: [i32; 8]) -> ([i32; 4], [i32; 4]) {
    let (mut p2, mut p3) = (s[2], s[6]);
    let mut p1 = (p2 + p3) * f2f(0.5411961);
    let mut t2 = p1 + p3 * f2f(-1.847759065);
    let mut t3 = p1 + p2 * f2f(0.765366865);
    p2 = s[0];
    p3 = s[4];
    let mut t0 = (p2 + p3) * 4096;
    let mut t1 = (p2 - p3) * 4096;
    let x0 = t0 + t3;
    let x3 = t0 - t3;
    let x1 = t1 + t2;
    let x2 = t1 - t2;
    t0 = s[7];
    t1 = s[5];
    t2 = s[3];
    t3 = s[1];
    p3 = t0 + t2;
    let mut p4 = t1 + t3;
    p1 = t0 + t3;
    p2 = t1 + t2;
    let p5 = (p3 + p4) * f2f(1.175875602);
    t0 *= f2f(0.298631336);
    t1 *= f2f(2.053119869);
    t2 *= f2f(3.072711026);
    t3 *= f2f(1.501321110);
    p1 = p5 + p1 * f2f(-0.899976223);
    p2 = p5 + p2 * f2f(-2.562915447);
    p3 *= f2f(-1.961570560);
    p4 *= f2f(-0.390180644);
    t3 += p1 + p4;
    t2 += p2 + p3;
    t1 += p2 + p4;
    t0 += p1 + p3;
    ([x0, x1, x2, x3], [t0, t1, t2, t3])
}

fn idct_block(coef: &[i32], q: &[u16; 64], out: &mut [u8], stride: usize) {
    let mut d = [0i32; 64];
    for i in 0..64 {
        d[i] = coef[i].wrapping_mul(q[i] as i32);
    }
    let mut v = [0i32; 64];
    for i in 0..8 {
        if (1..8).all(|r| d[r * 8 + i] == 0) {
            let dc = d[i] * 4;
            for r in 0..8 {
                v[r * 8 + i] = dc;
            }
            continue;
        }
        let (x, t) = idct_1d([d[i], d[8 + i], d[16 + i], d[24 + i], d[32 + i], d[40 + i], d[48 + i], d[56 + i]]);
        let x = x.map(|a| a + 512);
        v[i] = (x[0] + t[3]) >> 10;
        v[56 + i] = (x[0] - t[3]) >> 10;
        v[8 + i] = (x[1] + t[2]) >> 10;
        v[48 + i] = (x[1] - t[2]) >> 10;
        v[16 + i] = (x[2] + t[1]) >> 10;
        v[40 + i] = (x[2] - t[1]) >> 10;
        v[24 + i] = (x[3] + t[0]) >> 10;
        v[32 + i] = (x[3] - t[0]) >> 10;
    }
    let clamp = |x: i32| x.clamp(0, 255) as u8;
    for r in 0..8 {
        let row = &v[r * 8..r * 8 + 8];
        let (x, t) = idct_1d([row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7]]);
        let x = x.map(|a| a + 65536 + (128 << 17));
        let o = &mut out[r * stride..r * stride + 8];
        o[0] = clamp((x[0] + t[3]) >> 17);
        o[7] = clamp((x[0] - t[3]) >> 17);
        o[1] = clamp((x[1] + t[2]) >> 17);
        o[6] = clamp((x[1] - t[2]) >> 17);
        o[2] = clamp((x[2] + t[1]) >> 17);
        o[5] = clamp((x[2] - t[1]) >> 17);
        o[3] = clamp((x[3] + t[0]) >> 17);
        o[4] = clamp((x[3] - t[0]) >> 17);
    }
}

// ---------------------------------------------------------------- colour

fn to_rgba(f: &Frame, qt: &[[u16; 64]; 4], adobe: Option<u8>) -> Result<Image, String> {
    // reconstruct each component plane
    let planes: Vec<(Vec<u8>, usize, usize)> = f
        .comps
        .iter()
        .map(|c| {
            let pw = c.bw * 8;
            let ph = c.bh * 8;
            let mut plane = vec![0u8; pw * ph];
            for by in 0..c.bh {
                for bx in 0..c.bw {
                    let off = (by * c.bw + bx) * 64;
                    idct_block(&c.coeffs[off..off + 64], &qt[c.tq], &mut plane[by * 8 * pw + bx * 8..], pw);
                }
            }
            (plane, pw, ph)
        })
        .collect();
    let (w, h) = (f.width, f.height);
    // sample component `ci` at image pixel (x, y), interpolating subsampled planes
    let sample = |ci: usize, x: usize, y: usize| -> f32 {
        let c = &f.comps[ci];
        let (plane, pw, ph) = (&planes[ci].0, planes[ci].1, planes[ci].2);
        if c.h == f.hmax && c.v == f.vmax {
            return plane[y * pw + x] as f32;
        }
        let sx = ((x as f32 + 0.5) * c.h as f32 / f.hmax as f32 - 0.5).max(0.0);
        let sy = ((y as f32 + 0.5) * c.v as f32 / f.vmax as f32 - 0.5).max(0.0);
        let x0 = (sx as usize).min(pw - 1);
        let y0 = (sy as usize).min(ph - 1);
        let x1 = (x0 + 1).min(pw - 1);
        let y1 = (y0 + 1).min(ph - 1);
        let fx = sx - x0 as f32;
        let fy = sy - y0 as f32;
        let a = plane[y0 * pw + x0] as f32 * (1.0 - fx) + plane[y0 * pw + x1] as f32 * fx;
        let b = plane[y1 * pw + x0] as f32 * (1.0 - fx) + plane[y1 * pw + x1] as f32 * fx;
        a * (1.0 - fy) + b * fy
    };
    let n = f.comps.len();
    let rgb_ids = n == 3 && f.comps[0].id == b'R' && f.comps[1].id == b'G' && f.comps[2].id == b'B';
    let mut out = vec![255u8; w * h * 4];
    let ycc = |y: f32, cb: f32, cr: f32| -> (f32, f32, f32) {
        let (cb, cr) = (cb - 128.0, cr - 128.0);
        (y + 1.402 * cr, y - 0.344136 * cb - 0.714136 * cr, y + 1.772 * cb)
    };
    let cl = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    for y in 0..h {
        for x in 0..w {
            let o = (y * w + x) * 4;
            let (r, g, b) = match n {
                1 => {
                    let v = sample(0, x, y);
                    (v, v, v)
                }
                3 if rgb_ids || adobe == Some(0) => (sample(0, x, y), sample(1, x, y), sample(2, x, y)),
                3 => ycc(sample(0, x, y), sample(1, x, y), sample(2, x, y)),
                4 => {
                    let (c, m, yy) = if adobe == Some(2) { ycc(sample(0, x, y), sample(1, x, y), sample(2, x, y)) } else { (sample(0, x, y), sample(1, x, y), sample(2, x, y)) };
                    let k = sample(3, x, y);
                    // Adobe stores CMYK inverted
                    (c * k / 255.0, m * k / 255.0, yy * k / 255.0)
                }
                _ => return Err("unsupported component count".into()),
            };
            out[o] = cl(r);
            out[o + 1] = cl(g);
            out[o + 2] = cl(b);
        }
    }
    Ok(Image { w: w as u32, h: h as u32, data: out })
}

// ---------------------------------------------------------------- EXIF orientation

fn exif_orientation(tiff: &[u8]) -> Option<u16> {
    let le = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let r16 = |i: usize| -> Option<u16> {
        let b = tiff.get(i..i + 2)?;
        Some(if le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let r32 = |i: usize| -> Option<u32> {
        let b = tiff.get(i..i + 4)?;
        Some(if le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) })
    };
    let ifd = r32(4)? as usize;
    let n = r16(ifd)? as usize;
    for i in 0..n.min(200) {
        let e = ifd + 2 + i * 12;
        if r16(e)? == 0x0112 {
            let v = r16(e + 8)?;
            return (1..=8).contains(&v).then_some(v);
        }
    }
    None
}

/// Apply an EXIF orientation (images are shown upright, as browsers do by default).
pub fn orient(img: Image, o: u16) -> Image {
    if o <= 1 || o > 8 {
        return img;
    }
    let (w, h) = (img.w as usize, img.h as usize);
    let swap = o >= 5;
    let (nw, nh) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                _ => (y, w - 1 - x),
            };
            let s = (y * w + x) * 4;
            let d = (dy * nw + dx) * 4;
            out[d..d + 4].copy_from_slice(&img.data[s..s + 4]);
        }
    }
    Image { w: nw as u32, h: nh as u32, data: out }
}
