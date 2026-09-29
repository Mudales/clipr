//! Converting clipboard images to/from what we store: a full PNG plus a small
//! PNG thumbnail for the picker.

use anyhow::{Context, Result};
use image::{ImageFormat, RgbaImage};
use std::borrow::Cow;
use std::io::Cursor;

const THUMB_MAX: u32 = 320;
/// Images larger than this (encoded) are not stored.
pub const MAX_IMAGE_BYTES: usize = 32 << 20;

pub struct Stored {
    pub png: Vec<u8>,
    pub thumb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)?;
    Ok(out)
}

fn store(img: RgbaImage, png: Option<Vec<u8>>) -> Result<Stored> {
    let (width, height) = img.dimensions();
    let thumb = image::imageops::thumbnail(
        &img,
        (width * THUMB_MAX / width.max(height)).max(1),
        (height * THUMB_MAX / width.max(height)).max(1),
    );
    let png = match png {
        Some(png) => png,
        None => encode_png(&img)?,
    };
    Ok(Stored { thumb: encode_png(&thumb)?, png, width, height })
}

/// From raw RGBA pixels (what arboard returns).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Stored> {
    let img = RgbaImage::from_raw(width, height, rgba).context("bad image buffer")?;
    store(img, None)
}

/// From an encoded file (PNG/JPEG, e.g. from `wl-paste`). PNGs are kept as-is.
pub fn from_encoded(bytes: &[u8]) -> Result<Stored> {
    let format = image::guess_format(bytes)?;
    let img = image::load_from_memory_with_format(bytes, format)?.into_rgba8();
    let keep = (format == ImageFormat::Png).then(|| bytes.to_vec());
    store(img, keep)
}

pub fn looks_like_image(bytes: &[u8]) -> bool {
    matches!(image::guess_format(bytes), Ok(ImageFormat::Png | ImageFormat::Jpeg))
}

/// Decodes a stored PNG into RGBA for putting back on the clipboard.
pub fn decode(png: &[u8]) -> Result<arboard::ImageData<'static>> {
    let img = image::load_from_memory_with_format(png, ImageFormat::Png)?.into_rgba8();
    let (width, height) = img.dimensions();
    Ok(arboard::ImageData {
        width: width as usize,
        height: height as usize,
        bytes: Cow::Owned(img.into_raw()),
    })
}

/// Content key for an image: unique per picture, and searchable as "image".
pub fn content_key(stored: &Stored) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    stored.png.hash(&mut h);
    format!("image:{}x{}:{:016x}", stored.width, stored.height, h.finish())
}

/// Parses the size back out of a content key.
pub fn parse_key(content: &str) -> Option<(u32, u32)> {
    let size = content.strip_prefix("image:")?.split(':').next()?;
    let (w, h) = size.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}
