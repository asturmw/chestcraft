//! Background pictures: decoded in process (EXIF orientation applied), shrunk to at most
//! 3840 px on the longest edge, re-encoded as JPEG (which leaves every metadata block
//! behind, EXIF and GPS included), plus a small preview for the settings pages.

use crate::auth::random;
use crate::db::{BackgroundImage, BackgroundSet};
use image::DynamicImage;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use std::io::Write;
use std::path::Path;

/// Longest edge of a stored picture.
pub const MAX_EDGE: u32 = 3840;
/// Longest edge of the preview.
pub const SMALL_EDGE: u32 = 480;
/// Largest upload per picture. Three of them stay below Cloudflare's 100 MB request cap.
pub const MAX_UPLOAD_BYTES: usize = 30 << 20;
const QUALITY: u8 = 85;
const SMALL_QUALITY: u8 = 80;
const ID_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const ID_LEN: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum AddError {
    #[error("no picture was sent")]
    Empty,
    #[error("{0}: the picture is larger than 30 MiB")]
    TooLarge(String),
    #[error("{0}: not a picture chestcraft can read")]
    NotAPicture(String),
    #[error("shutting down")]
    Busy,
    #[error("{0}")]
    Storage(String),
}

/// A converted picture.
pub struct Converted {
    pub full: Vec<u8>,
    pub small: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn shrink(img: &DynamicImage, edge: u32) -> DynamicImage {
    if img.width() <= edge && img.height() <= edge {
        return img.clone();
    }
    img.resize(edge, edge, FilterType::CatmullRom)
}

fn jpeg(img: &DynamicImage, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&img.to_rgb8())
        .ok()?;
    Some(out)
}

/// Decode, shrink and re-encode one upload. None: not a picture we can read.
pub fn convert(bytes: &[u8]) -> Option<Converted> {
    let img = crate::fs::thumbs::raster::decode_bytes(bytes).ok()?;
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    let full = shrink(&img, MAX_EDGE);
    drop(img);
    let small = shrink(&full, SMALL_EDGE);
    Some(Converted {
        width: full.width(),
        height: full.height(),
        full: jpeg(&full, QUALITY)?,
        small: jpeg(&small, SMALL_QUALITY)?,
    })
}

fn write_synced(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(data)?;
    f.sync_all()
}

/// Check, convert and store a set under `dir/<new id>/` (blocking). The pictures are
/// written to a temporary folder first and renamed into place once all are converted.
pub fn write_set(
    dir: &Path,
    owner: &str,
    pictures: Vec<(String, Vec<u8>)>,
    now_ms: i64,
) -> Result<BackgroundSet, AddError> {
    let mut pictures: Vec<(String, Vec<u8>)> = pictures
        .into_iter()
        .filter(|(_, b)| !b.is_empty())
        .collect();
    if pictures.is_empty() {
        return Err(AddError::Empty);
    }
    pictures.sort_by_key(|(slot, _)| super::SLOTS.iter().position(|s| s == slot));
    for (i, (slot, bytes)) in pictures.iter().enumerate() {
        if !super::is_slot(slot) || pictures[..i].iter().any(|(s, _)| s == slot) {
            return Err(AddError::Storage(format!(
                "unknown or repeated slot {slot}"
            )));
        }
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(AddError::TooLarge(slot.clone()));
        }
    }
    let storage = |e: std::io::Error| AddError::Storage(e.to_string());
    std::fs::create_dir_all(dir).map_err(storage)?;
    let id = random::string(ID_ALPHABET, ID_LEN);
    let tmp = dir.join(format!(".tmp-{id}"));
    std::fs::create_dir(&tmp).map_err(storage)?;
    let result = (|| {
        let mut images = Vec::new();
        for (slot, bytes) in &pictures {
            let c = convert(bytes).ok_or_else(|| AddError::NotAPicture(slot.clone()))?;
            write_synced(&tmp.join(format!("{slot}.jpg")), &c.full).map_err(storage)?;
            write_synced(&tmp.join(format!("{slot}.small.jpg")), &c.small).map_err(storage)?;
            images.push(BackgroundImage {
                slot: slot.clone(),
                width: c.width,
                height: c.height,
                bytes: c.full.len() as u64,
            });
        }
        std::fs::rename(&tmp, dir.join(&id)).map_err(storage)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(images)
    })();
    match result {
        Ok(images) => Ok(BackgroundSet {
            id,
            owner: owner.to_string(),
            created_ms: now_ms,
            images,
        }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};
    use std::io::Cursor;

    pub fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb([200, 40, 90])))
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn pictures_are_shrunk_and_re_encoded() {
        let c = convert(&png(4000, 2000)).unwrap();
        assert_eq!((c.width, c.height), (MAX_EDGE, MAX_EDGE / 2));
        assert_eq!(&c.full[..3], b"\xff\xd8\xff");
        let small = image::load_from_memory(&c.small).unwrap();
        assert_eq!(
            (small.width(), small.height()),
            (SMALL_EDGE, SMALL_EDGE / 2)
        );
        // Small pictures keep their size.
        let c = convert(&png(300, 500)).unwrap();
        assert_eq!((c.width, c.height), (300, 500));
        assert!(convert(b"not a picture").is_none());
    }

    #[test]
    fn exif_is_not_kept() {
        // A JPEG with an EXIF block (orientation 6 = rotate 90° clockwise).
        let mut src = Vec::new();
        DynamicImage::ImageRgb8(RgbImage::from_pixel(60, 30, Rgb([1, 2, 3])))
            .write_to(&mut Cursor::new(&mut src), ImageFormat::Jpeg)
            .unwrap();
        let mut tiff = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec();
        let mut app1 = b"Exif\0\0".to_vec();
        app1.append(&mut tiff);
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        jpeg.extend(((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend(&app1);
        jpeg.extend(&src[2..]);
        let c = convert(&jpeg).unwrap();
        assert_eq!((c.width, c.height), (30, 60), "orientation applied");
        assert!(!c.full.windows(4).any(|w| w == b"Exif"));
    }

    #[test]
    fn sets_are_written_whole_or_not_at_all() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("backgrounds");
        let set = write_set(
            &dir,
            "jana",
            vec![
                ("tall".into(), png(100, 160)),
                ("wide".into(), png(160, 90)),
                ("square".into(), Vec::new()),
            ],
            7,
        )
        .unwrap();
        let slots: Vec<&str> = set.images.iter().map(|i| i.slot.as_str()).collect();
        assert_eq!(slots, ["wide", "tall"]);
        assert_eq!((set.owner.as_str(), set.created_ms), ("jana", 7));
        for f in ["wide.jpg", "wide.small.jpg", "tall.jpg", "tall.small.jpg"] {
            assert!(dir.join(&set.id).join(f).is_file(), "{f}");
        }
        let bad = write_set(
            &dir,
            "jana",
            vec![
                ("wide".into(), png(10, 10)),
                ("tall".into(), b"junk".to_vec()),
            ],
            8,
        );
        assert!(matches!(bad, Err(AddError::NotAPicture(s)) if s == "tall"));
        assert!(matches!(
            write_set(&dir, "jana", vec![("wide".into(), Vec::new())], 9),
            Err(AddError::Empty)
        ));
        assert!(matches!(
            write_set(&dir, "jana", vec![("side".into(), png(4, 4))], 9),
            Err(AddError::Storage(_))
        ));
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [set.id]);
    }
}
