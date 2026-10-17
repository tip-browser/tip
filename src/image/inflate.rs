//! DEFLATE (RFC 1951) decoder plus zlib/gzip wrappers, and a simple zlib encoder.

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    nbits: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, bit: 0, nbits: 0 }
    }
    #[inline]
    fn need(&mut self, n: u32) -> Result<(), String> {
        while self.nbits < n {
            let b = *self.data.get(self.pos).ok_or("unexpected end of deflate stream")?;
            self.pos += 1;
            self.bit |= (b as u32) << self.nbits;
            self.nbits += 8;
        }
        Ok(())
    }
    #[inline]
    fn get(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }
    fn align(&mut self) {
        self.bit = 0;
        self.nbits = 0;
    }
}

/// Canonical Huffman decoding table.
struct Huff {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Result<Huff, String> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + counts[i - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huff { counts, symbols })
    }

    fn decode(&self, b: &mut Bits) -> Result<u16, String> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= b.get(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("invalid huffman code".into())
    }
}

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

pub fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(data.len() * 4);
    let mut b = Bits::new(data);
    loop {
        let last = b.get(1)?;
        let ty = b.get(2)?;
        match ty {
            0 => {
                b.align();
                if b.pos + 4 > data.len() {
                    return Err("truncated stored block".into());
                }
                let len = u16::from_le_bytes([data[b.pos], data[b.pos + 1]]) as usize;
                b.pos += 4;
                let end = (b.pos + len).min(data.len());
                out.extend_from_slice(&data[b.pos..end]);
                b.pos = end;
            }
            1 | 2 => {
                let (lit, dist) = if ty == 1 {
                    let mut l = [0u8; 288];
                    for (i, v) in l.iter_mut().enumerate() {
                        *v = match i {
                            0..=143 => 8,
                            144..=255 => 9,
                            256..=279 => 7,
                            _ => 8,
                        };
                    }
                    (Huff::new(&l)?, Huff::new(&[5u8; 30])?)
                } else {
                    let hlit = b.get(5)? as usize + 257;
                    let hdist = b.get(5)? as usize + 1;
                    let hclen = b.get(4)? as usize + 4;
                    const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                    let mut cl = [0u8; 19];
                    for &o in ORDER.iter().take(hclen) {
                        cl[o] = b.get(3)? as u8;
                    }
                    let clh = Huff::new(&cl)?;
                    let mut lens = vec![0u8; hlit + hdist];
                    let mut i = 0;
                    while i < hlit + hdist {
                        let sym = clh.decode(&mut b)?;
                        match sym {
                            0..=15 => {
                                lens[i] = sym as u8;
                                i += 1;
                            }
                            16 => {
                                let prev = *lens.get(i.wrapping_sub(1)).ok_or("repeat with no previous length")?;
                                let r = 3 + b.get(2)? as usize;
                                for _ in 0..r {
                                    if i < lens.len() {
                                        lens[i] = prev;
                                        i += 1;
                                    }
                                }
                            }
                            17 => i += 3 + b.get(3)? as usize,
                            _ => i += 11 + b.get(7)? as usize,
                        }
                    }
                    lens.truncate(hlit + hdist);
                    (Huff::new(&lens[..hlit])?, Huff::new(&lens[hlit..])?)
                };
                loop {
                    let sym = lit.decode(&mut b)?;
                    if sym < 256 {
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let li = (sym - 257) as usize;
                        if li >= 29 {
                            return Err("bad length symbol".into());
                        }
                        let len = LEN_BASE[li] as usize + b.get(LEN_EXTRA[li] as u32)? as usize;
                        let di = dist.decode(&mut b)? as usize;
                        if di >= 30 {
                            return Err("bad distance symbol".into());
                        }
                        let d = DIST_BASE[di] as usize + b.get(DIST_EXTRA[di] as u32)? as usize;
                        if d > out.len() {
                            return Err("distance too far back".into());
                        }
                        let start = out.len() - d;
                        for k in 0..len {
                            let v = out[start + k];
                            out.push(v);
                        }
                    }
                }
            }
            _ => return Err("invalid block type".into()),
        }
        if last == 1 {
            break;
        }
    }
    Ok(out)
}

pub fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 2 {
        return Err("zlib stream too short".into());
    }
    inflate(&data[2..])
}

pub fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b {
        return Err("not gzip".into());
    }
    let flags = data[3];
    let mut p = 10;
    if flags & 4 != 0 {
        let xlen = u16::from_le_bytes([data[p], data[p + 1]]) as usize;
        p += 2 + xlen;
    }
    if flags & 8 != 0 {
        while p < data.len() && data[p] != 0 {
            p += 1;
        }
        p += 1;
    }
    if flags & 16 != 0 {
        while p < data.len() && data[p] != 0 {
            p += 1;
        }
        p += 1;
    }
    if flags & 2 != 0 {
        p += 2;
    }
    inflate(&data[p.min(data.len())..])
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// zlib-compress using fixed Huffman codes with a simple hash-chain LZ77 matcher.
pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter { out: vec![0x78, 0x01], bit: 0, n: 0 };
    w.put(1, 1); // final
    w.put(1, 2); // fixed huffman
    const WINDOW: usize = 32768;
    let mut head = vec![usize::MAX; 1 << 15];
    let mut prev = vec![usize::MAX; data.len()];
    let hash = |i: usize| -> usize { ((data[i] as usize) << 10 ^ (data[i + 1] as usize) << 5 ^ data[i + 2] as usize) & 0x7fff };
    let mut i = 0;
    while i < data.len() {
        let mut best_len = 0;
        let mut best_dist = 0;
        if i + 2 < data.len() {
            let h = hash(i);
            let mut cand = head[h];
            let mut tries = 0;
            while cand != usize::MAX && i - cand <= WINDOW && tries < 32 {
                let mut l = 0;
                while l < 258 && i + l < data.len() && data[cand + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - cand;
                    if l == 258 {
                        break;
                    }
                }
                cand = prev[cand];
                tries += 1;
            }
            prev[i] = head[h];
            head[h] = i;
        }
        if best_len >= 3 {
            w.lit_len(257 + len_code(best_len) as u16);
            let li = len_code(best_len);
            w.put((best_len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
            let di = dist_code(best_dist);
            w.put_rev(di as u32, 5);
            w.put((best_dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di] as u32);
            for k in 1..best_len {
                let j = i + k;
                if j + 2 < data.len() {
                    let h = hash(j);
                    prev[j] = head[h];
                    head[h] = j;
                }
            }
            i += best_len;
        } else {
            w.lit_len(data[i] as u16);
            i += 1;
        }
    }
    w.lit_len(256);
    w.flush();
    let mut out = w.out;
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn len_code(len: usize) -> usize {
    let mut i = 28;
    while LEN_BASE[i] as usize > len {
        i -= 1;
    }
    i
}

fn dist_code(d: usize) -> usize {
    let mut i = 29;
    while DIST_BASE[i] as usize > d {
        i -= 1;
    }
    i
}

struct BitWriter {
    out: Vec<u8>,
    bit: u32,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.bit |= v << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.bit as u8);
            self.bit >>= 8;
            self.n -= 8;
        }
    }
    /// Huffman codes are written MSB-first.
    fn put_rev(&mut self, code: u32, n: u32) {
        let mut r = 0;
        for k in 0..n {
            r |= ((code >> k) & 1) << (n - 1 - k);
        }
        self.put(r, n);
    }
    fn lit_len(&mut self, sym: u16) {
        let s = sym as u32;
        match sym {
            0..=143 => self.put_rev(0x30 + s, 8),
            144..=255 => self.put_rev(0x190 + s - 144, 9),
            256..=279 => self.put_rev(s - 256, 7),
            _ => self.put_rev(0xc0 + s - 280, 8),
        }
    }
    fn flush(&mut self) {
        if self.n > 0 {
            self.out.push(self.bit as u8);
            self.bit = 0;
            self.n = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let data: Vec<u8> = b"hello hello hello world, the quick brown fox jumps over the lazy dog. hello hello!".repeat(50);
        let z = zlib_compress(&data);
        assert!(z.len() < data.len() / 4);
        assert_eq!(zlib_decompress(&z).unwrap(), data);
        let empty = zlib_compress(&[]);
        assert_eq!(zlib_decompress(&empty).unwrap(), Vec::<u8>::new());
    }
}
