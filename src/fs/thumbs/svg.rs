//! SVG through resvg. The file is rendered on its own: `<image>` references to other files
//! are ignored (only `data:` URLs load), so an SVG can't pull a picture from elsewhere on
//! the server into its thumbnail. Text uses the system fonts, loaded once.

use super::{Result, unusable};
use image::RgbaImage;
use resvg::{tiny_skia, usvg};
use std::io::Read;
use std::sync::{Arc, LazyLock};

const MAX_SVG: u64 = 32 << 20;

static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    Arc::new(db)
});

pub fn render(mut f: impl Read, len: u64, px: u32) -> Result<RgbaImage> {
    if len > MAX_SVG {
        return Err(unusable("SVG too large"));
    }
    let mut data = Vec::with_capacity(len as usize);
    (&mut f).take(MAX_SVG).read_to_end(&mut data)?;
    render_bytes(&data, px)
}

pub fn render_bytes(data: &[u8], px: u32) -> Result<RgbaImage> {
    let opt = usvg::Options {
        fontdb: FONTS.clone(),
        resources_dir: None,
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(data, &opt).map_err(|e| unusable(format!("svg: {e}")))?;
    let size = tree.size();
    if !(size.width() > 0.0 && size.height() > 0.0) {
        return Err(unusable("svg without size"));
    }
    let (x, y, w, h) = content_box(&tree, size.width(), size.height());
    // Vector art: scale to the requested size either way.
    let scale = px as f32 / w.max(h);
    let (pw, ph) = (
        ((w * scale).round() as u32).clamp(1, px),
        ((h * scale).round() as u32).clamp(1, px),
    );
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or_else(|| unusable("svg size"))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, -x * scale, -y * scale),
        &mut pixmap.as_mut(),
    );
    let mut img = RgbaImage::new(pw, ph);
    for (dst, src) in img.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        dst.0 = [c.red(), c.green(), c.blue(), c.alpha()];
    }
    Ok(img)
}

/// The part of the canvas that has something on it, with a small margin: drawings for
/// cutting or printing often sit small in a corner of a whole A4 page, which would leave the
/// thumbnail nearly empty. The whole canvas when nothing is drawn or it is mostly filled.
fn content_box(tree: &usvg::Tree, w: f32, h: f32) -> (f32, f32, f32, f32) {
    let b = tree.root().abs_stroke_bounding_box();
    let (x0, y0) = (b.left().max(0.0), b.top().max(0.0));
    let (x1, y1) = (b.right().min(w), b.bottom().min(h));
    if !(x1 > x0 && y1 > y0) || (x1 - x0) * (y1 - y0) > 0.8 * w * h {
        return (0.0, 0.0, w, h);
    }
    let pad = (x1 - x0).max(y1 - y0) * 0.03;
    let (x0, y0) = ((x0 - pad).max(0.0), (y0 - pad).max(0.0));
    let (x1, y1) = ((x1 + pad).min(w), (y1 + pad).min(h));
    (x0, y0, x1 - x0, y1 - y0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_scales_vector_art() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
            <rect x="0" y="0" width="20" height="20" fill="#ff0000"/></svg>"##;
        // Half the canvas is drawn: cropped to the drawing and a small margin.
        let img = render_bytes(svg, 200).unwrap();
        assert_eq!((img.width(), img.height()), (200, 194));
        assert_eq!(img.get_pixel(100, 97).0, [255, 0, 0, 255]);
        // Mostly drawn: the whole canvas, the empty strip stays.
        let wide = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
            <rect x="0" y="0" width="36" height="20" fill="#ff0000"/></svg>"##;
        let img = render_bytes(wide, 200).unwrap();
        assert_eq!(img.dimensions(), (200, 100));
        assert_eq!(img.get_pixel(10, 50).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(195, 50).0[3], 0);
    }

    #[test]
    fn crops_small_drawings_on_a_big_page() {
        // A4 in points with a 60 × 30 drawing near the top left.
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="595" height="842">
            <rect x="20" y="40" width="60" height="30" fill="#00ff00"/></svg>"##;
        let img = render_bytes(svg, 200).unwrap();
        assert_eq!(img.width(), 200);
        assert!((100..=120).contains(&img.height()), "{}", img.height());
        assert_eq!(img.get_pixel(100, img.height() / 2).0, [0, 255, 0, 255]);
    }

    #[test]
    fn never_loads_files_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let secret = tmp.path().join("secret.png");
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 255, 0, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        std::fs::write(&secret, &png).unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10">
              <image width="10" height="10" xlink:href="{}"/></svg>"#,
            secret.display()
        );
        let img = render_bytes(svg.as_bytes(), 10).unwrap();
        assert!(img.pixels().all(|p| p.0[3] == 0), "nothing was drawn");
    }
}
