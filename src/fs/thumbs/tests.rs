//! Thumbnails end to end: open a file, get its thumbnail through the cache.

use super::*;
use crate::fs::volume::VolumeRoot;
use image::{DynamicImage, GenericImageView, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use std::io::Cursor;
use std::path::Path;

struct T {
    tmp: tempfile::TempDir,
    thumbs: Thumbs,
}

fn setup() -> T {
    let tmp = tempfile::tempdir().unwrap();
    let thumbs = Thumbs {
        cache: Arc::new(Cache::new(tmp.path().join("cache"), 1 << 30)),
        cpu: Semaphore::new(2),
        ffmpeg: Semaphore::new(2),
        ffmpeg_path: PathBuf::from(FFMPEG),
        inflight: Mutex::new(HashMap::new()),
    };
    T { tmp, thumbs }
}

impl T {
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.tmp.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    async fn thumb(&self, path: &Path, size: Size) -> Result<Option<Thumb>> {
        self.thumb_if(path, size, true).await
    }

    async fn thumb_if(&self, path: &Path, size: Size, render: bool) -> Result<Option<Thumb>> {
        let fd: OwnedFd = std::fs::File::open(path).unwrap().into();
        let meta = VolumeRoot::meta(&fd).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        let src = crate::fs::filetype::thumb_source(name).expect("has a thumbnail source");
        self.thumbs.get(fd, &meta, src, size, || render).await
    }
}

fn encode(img: DynamicImage, fmt: ImageFormat) -> Vec<u8> {
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), fmt).unwrap();
    out
}

fn dims(t: &Thumb) -> (u32, u32) {
    image::load_from_memory(&t.bytes).unwrap().dimensions()
}

/// A JPEG with an EXIF orientation tag (6 = rotate 90° clockwise).
fn jpeg_with_orientation(img: RgbImage, orientation: u16) -> Vec<u8> {
    let plain = encode(DynamicImage::ImageRgb8(img), ImageFormat::Jpeg);
    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    tiff.extend(1u16.to_le_bytes());
    tiff.extend(0x0112u16.to_le_bytes());
    tiff.extend(3u16.to_le_bytes());
    tiff.extend(1u32.to_le_bytes());
    tiff.extend(orientation.to_le_bytes());
    tiff.extend([0, 0]);
    tiff.extend(0u32.to_le_bytes());
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend(tiff);
    let mut out = plain[..2].to_vec();
    out.extend([0xff, 0xe1]);
    out.extend(((app1.len() + 2) as u16).to_be_bytes());
    out.extend(app1);
    out.extend(&plain[2..]);
    out
}

#[tokio::test]
async fn photos_shrink_to_jpeg_and_come_from_the_cache_next_time() {
    let t = setup();
    let img = RgbImage::from_pixel(1000, 500, Rgb([200, 120, 40]));
    let p = t.file("IMG_2041.jpg", &encode(img.into(), ImageFormat::Jpeg));
    let grid = t.thumb(&p, Size::Grid).await.unwrap().unwrap();
    assert_eq!(grid.mime, "image/jpeg");
    assert_eq!(dims(&grid), (480, 240));
    // Cached: no render allowed, still there.
    let again = t.thumb_if(&p, Size::Grid, false).await.unwrap().unwrap();
    assert_eq!(again.bytes, grid.bytes);
    // The viewer size of a small picture is the picture's own size.
    let view = t.thumb(&p, Size::View).await.unwrap().unwrap();
    assert_eq!(dims(&view), (1000, 500));
    // A new thumbnail that may not be rendered now.
    let other = t.file(
        "b.jpg",
        &encode(RgbImage::new(10, 10).into(), ImageFormat::Jpeg),
    );
    assert!(matches!(
        t.thumb_if(&other, Size::Grid, false).await,
        Err(ThumbError::Busy)
    ));
}

#[tokio::test]
async fn exif_orientation_is_applied() {
    let t = setup();
    let p = t.file(
        "portrait.jpg",
        &jpeg_with_orientation(RgbImage::from_pixel(600, 300, Rgb([10, 10, 10])), 6),
    );
    let th = t.thumb(&p, Size::Grid).await.unwrap().unwrap();
    assert_eq!(dims(&th), (240, 480));
}

#[tokio::test]
async fn transparency_keeps_png_and_broken_files_are_remembered() {
    let t = setup();
    let mut img = RgbaImage::from_pixel(64, 64, Rgba([0, 0, 255, 255]));
    img.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
    let p = t.file("logo.png", &encode(img.into(), ImageFormat::Png));
    let th = t.thumb(&p, Size::Grid).await.unwrap().unwrap();
    assert_eq!(th.mime, "image/png");

    // Opaque PNG screenshots become JPEG.
    let p = t.file(
        "shot.png",
        &encode(
            RgbaImage::from_pixel(8, 8, Rgba([1, 2, 3, 255])).into(),
            ImageFormat::Png,
        ),
    );
    assert_eq!(
        t.thumb(&p, Size::Grid).await.unwrap().unwrap().mime,
        "image/jpeg"
    );

    let bad = t.file("broken.jpg", b"not really a jpeg");
    assert!(t.thumb(&bad, Size::Grid).await.unwrap().is_none());
    // Remembered: no new render needed to answer.
    assert!(t.thumb_if(&bad, Size::Grid, false).await.unwrap().is_none());
}

#[tokio::test]
async fn models_gcode_and_svg() {
    let t = setup();
    let stl = t.file("Gehäuse_Deckel.stl", &mesh::binary_stl(&mesh::cube()));
    let th = t.thumb(&stl, Size::Grid).await.unwrap().unwrap();
    assert_eq!(th.mime, "image/png");
    let (w, h) = dims(&th);
    assert!(w.max(h) == GRID_PX, "{w}x{h}");

    let pic = encode(
        RgbImage::from_pixel(300, 300, Rgb([0, 128, 0])).into(),
        ImageFormat::Png,
    );
    let g = t.file("Deckel_PLA_0.2mm.gcode", &gcode::text_gcode(&[(300, &pic)]));
    assert_eq!(
        dims(&t.thumb(&g, Size::Grid).await.unwrap().unwrap()),
        (300, 300)
    );
    let bg = t.file("Deckel.bgcode", &gcode::binary_gcode(&[(300, &pic)]));
    assert_eq!(
        dims(&t.thumb(&bg, Size::Grid).await.unwrap().unwrap()),
        (300, 300)
    );
    let mf = t.file(
        "Halterung_v3.3mf",
        &threemf::package(&[("Metadata/thumbnail.png", &pic)]),
    );
    assert_eq!(
        dims(&t.thumb(&mf, Size::Grid).await.unwrap().unwrap()),
        (300, 300)
    );

    let svg = t.file(
        "Schild Werkstatt.svg",
        br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50"><circle cx="25" cy="25" r="20"/></svg>"#,
    );
    let th = t.thumb(&svg, Size::View).await.unwrap().unwrap();
    assert_eq!((th.mime, dims(&th)), ("image/png", (VIEW_PX, VIEW_PX / 2)));
}

fn has_ffmpeg() -> bool {
    let ok = Path::new(FFMPEG).is_file();
    if !ok {
        eprintln!("skipped: {FFMPEG} is missing");
    }
    ok
}

#[tokio::test]
async fn video_frames_through_ffmpeg() {
    if !has_ffmpeg() {
        return;
    }
    let t = setup();
    let out = t.tmp.path().join("clip.mp4");
    let made = std::process::Command::new(FFMPEG)
        .args(["-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc=duration=1:size=320x240:rate=10")
        .args(["-pix_fmt", "yuv420p"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(made.success());
    // One second long: the frame at 3 s doesn't exist, the first one is used.
    let th = t.thumb(&out, Size::Grid).await.unwrap().unwrap();
    assert_eq!((th.mime, dims(&th)), ("image/jpeg", (320, 240)));
}

#[tokio::test]
async fn ffmpeg_never_follows_playlists_to_other_files() {
    if !has_ffmpeg() {
        return;
    }
    let t = setup();
    let secret = t.tmp.path().join("secret.png");
    std::fs::write(
        &secret,
        encode(
            RgbImage::from_pixel(32, 32, Rgb([255, 0, 0])).into(),
            ImageFormat::Png,
        ),
    )
    .unwrap();
    let concat = format!("ffconcat version 1.0\nfile '{}'\n", secret.display());
    let p = t.file("evil.mp4", concat.as_bytes());
    assert!(t.thumb(&p, Size::Grid).await.unwrap().is_none());
    let hls = format!(
        "#EXTM3U\n#EXTINF:1,\nfile://{}\n#EXT-X-ENDLIST\n",
        secret.display()
    );
    let p = t.file("evil.mkv", hls.as_bytes());
    assert!(t.thumb(&p, Size::Grid).await.unwrap().is_none());
}

#[tokio::test]
async fn missing_ffmpeg_means_no_thumbnail() {
    let mut t = setup();
    t.thumbs.set_ffmpeg(t.tmp.path().join("no-ffmpeg"));
    let p = t.file("clip.webm", b"whatever");
    assert!(matches!(
        t.thumb(&p, Size::Grid).await,
        Err(ThumbError::Skipped(_))
    ));
    // Not remembered: once ffmpeg is there, the next request tries again.
    assert!(matches!(
        t.thumb_if(&p, Size::Grid, false).await,
        Err(ThumbError::Busy)
    ));
}
