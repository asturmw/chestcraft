//! Raster images: decode (format from the content, EXIF orientation applied), shrink to
//! the requested size, encode as JPEG, or as PNG when the picture has transparency.

use super::{Result, Thumb, unusable};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use std::io::{BufRead, Cursor, Seek};

const JPEG_QUALITY: u8 = 82;
/// Largest picture decoded (pixels on either edge).
const MAX_EDGE: u32 = 30_000;
/// Memory one decode may use.
const MAX_ALLOC: u64 = 1 << 30;

pub fn decode(r: impl BufRead + Seek) -> Result<DynamicImage> {
    let mut reader = ImageReader::new(r)
        .with_guessed_format()
        .map_err(|e| unusable(format!("read: {e}")))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    limits.max_alloc = Some(MAX_ALLOC);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| unusable(format!("format: {e}")))?;
    let orientation = decoder.orientation().ok();
    let mut img =
        DynamicImage::from_decoder(decoder).map_err(|e| unusable(format!("decode: {e}")))?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

pub fn decode_bytes(bytes: &[u8]) -> Result<DynamicImage> {
    decode(Cursor::new(bytes))
}

/// Shrink to `px` on the longest edge (never enlarge) and encode.
pub fn encode(img: DynamicImage, px: u32) -> Result<Thumb> {
    let img = if img.width() > px || img.height() > px {
        img.thumbnail(px, px)
    } else {
        img
    };
    let opaque = !img.color().has_alpha() || img.to_rgba8().pixels().all(|p| p.0[3] == 255);
    let mut out = Vec::new();
    if opaque {
        let rgb = img.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
            .encode_image(&rgb)
            .map_err(|e| unusable(format!("jpeg: {e}")))?;
        Ok(Thumb {
            bytes: out,
            mime: "image/jpeg",
        })
    } else {
        let rgba = img.to_rgba8();
        rgba.write_with_encoder(PngEncoder::new_with_quality(
            &mut out,
            CompressionType::Default,
            FilterType::Adaptive,
        ))
        .map_err(|e| unusable(format!("png: {e}")))?;
        Ok(Thumb {
            bytes: out,
            mime: "image/png",
        })
    }
}
