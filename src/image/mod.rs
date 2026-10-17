//! Image decoding.

pub mod inflate;
pub mod gif;
pub mod jpeg;
pub mod png;

use crate::raster::Image;

/// Decode an image by sniffing its signature.
pub fn decode(data: &[u8]) -> Result<Image, String> {
    if data.starts_with(b"\x89PNG") {
        png::decode(data)
    } else if data.starts_with(&[0xff, 0xd8]) {
        jpeg::decode(data)
    } else if data.starts_with(b"GIF8") {
        gif::decode(data)
    } else {
        Err("unknown image format".into())
    }
}
