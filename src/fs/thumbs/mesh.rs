//! Triangle meshes: the STL reader and a small software rasterizer. Models are seen from
//! the front right, 30° above (Z up, as slicers show them), with an orthographic camera,
//! flat shading of a neutral gray material, a key and a fill light, and a transparent
//! background. Supersampling smooths the edges.

use super::{Result, unusable};
use image::RgbaImage;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

pub type V3 = [f32; 3];

/// Largest mesh rendered (triangles).
pub const MAX_TRIANGLES: usize = 10_000_000;

#[derive(Default)]
pub struct Mesh {
    pub tris: Vec<[V3; 3]>,
}

impl Mesh {
    pub fn push(&mut self, t: [V3; 3]) -> Result<()> {
        if self.tris.len() >= MAX_TRIANGLES {
            return Err(unusable("mesh too large"));
        }
        if t.iter().flatten().all(|c| c.is_finite()) {
            self.tris.push(t);
        }
        Ok(())
    }
}

/// Binary or ASCII STL.
pub fn read_stl(mut f: impl Read + Seek, len: u64) -> Result<Mesh> {
    let mut head = [0u8; 84];
    let got = read_up_to(&mut f, &mut head)?;
    let count = if got == 84 {
        u64::from(u32::from_le_bytes([head[80], head[81], head[82], head[83]]))
    } else {
        0
    };
    let binary_len = 84 + 50 * count;
    let ascii = head.starts_with(b"solid") && binary_len != len;
    f.seek(SeekFrom::Start(0))?;
    if ascii {
        return read_ascii_stl(BufReader::new(f));
    }
    if got < 84 || binary_len > len {
        return Err(unusable("not an STL file"));
    }
    if count as usize > MAX_TRIANGLES {
        return Err(unusable("mesh too large"));
    }
    f.seek(SeekFrom::Start(84))?;
    let mut r = BufReader::with_capacity(1 << 16, f);
    let mut mesh = Mesh {
        tris: Vec::with_capacity(count as usize),
    };
    let mut rec = [0u8; 50];
    for _ in 0..count {
        r.read_exact(&mut rec)?;
        let v = |i: usize| -> V3 {
            let at = |j: usize| {
                let o = 12 + i * 12 + j * 4;
                f32::from_le_bytes([rec[o], rec[o + 1], rec[o + 2], rec[o + 3]])
            };
            [at(0), at(1), at(2)]
        };
        mesh.push([v(0), v(1), v(2)])?;
    }
    Ok(mesh)
}

fn read_up_to(f: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn read_ascii_stl(r: impl BufRead) -> Result<Mesh> {
    let mut mesh = Mesh::default();
    let mut verts: Vec<V3> = Vec::with_capacity(3);
    for line in r.split(b'\n') {
        let line = line?;
        let Ok(s) = std::str::from_utf8(&line) else {
            return Err(unusable("not an STL file"));
        };
        let mut words = s.split_ascii_whitespace();
        match words.next() {
            Some("vertex") => {
                let mut c = [0f32; 3];
                for slot in &mut c {
                    *slot = words
                        .next()
                        .and_then(|w| w.parse().ok())
                        .ok_or_else(|| unusable("bad vertex"))?;
                }
                verts.push(c);
            }
            Some("endloop") => {
                // Polygons with more than three corners: fan them out.
                for i in 1..verts.len().saturating_sub(1) {
                    mesh.push([verts[0], verts[i], verts[i + 1]])?;
                }
                verts.clear();
            }
            _ => {}
        }
    }
    if mesh.tris.is_empty() {
        return Err(unusable("no triangles"));
    }
    Ok(mesh)
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> Option<V3> {
    let l = dot(a, a).sqrt();
    (l > 0.0 && l.is_finite()).then(|| [a[0] / l, a[1] / l, a[2] / l])
}
fn add_scaled(a: V3, b: V3, s: f32) -> V3 {
    [a[0] + b[0] * s, a[1] + b[1] * s, a[2] + b[2] * s]
}

const ALBEDO: V3 = [0.72, 0.74, 0.78];

/// Render `mesh` with `px` pixels on the longest edge of the image.
pub fn render(mesh: &Mesh, px: u32) -> Result<RgbaImage> {
    if mesh.tris.is_empty() {
        return Err(unusable("empty mesh"));
    }
    let (el, az) = (30f32.to_radians(), 45f32.to_radians());
    // From the model towards the camera.
    let view = [el.cos() * az.sin(), -el.cos() * az.cos(), el.sin()];
    let fwd = [-view[0], -view[1], -view[2]];
    let right = norm(cross(fwd, [0.0, 0.0, 1.0])).unwrap_or([1.0, 0.0, 0.0]);
    let up = cross(right, fwd);
    let key = norm(add_scaled(add_scaled(view, up, 0.7), right, -0.5)).unwrap_or(view);
    let fill = norm(add_scaled(add_scaled(view, up, -0.3), right, 0.9)).unwrap_or(view);
    let half = norm([view[0] + key[0], view[1] + key[1], view[2] + key[2]]).unwrap_or(view);

    let project = |p: V3| -> [f32; 3] { [dot(p, right), dot(p, up), dot(p, fwd)] };
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for t in &mesh.tris {
        for &p in t {
            let s = project(p);
            for i in 0..2 {
                lo[i] = lo[i].min(s[i]);
                hi[i] = hi[i].max(s[i]);
            }
        }
    }
    let (w, h) = (hi[0] - lo[0], hi[1] - lo[1]);
    let extent = w.max(h);
    if extent <= 0.0 || !extent.is_finite() {
        return Err(unusable("flat mesh"));
    }
    let pad = (px as f32 * 0.04).round();
    let scale = (px as f32 - 2.0 * pad) / extent;
    let out_w = ((w * scale).ceil() + 2.0 * pad).clamp(1.0, px as f32) as u32;
    let out_h = ((h * scale).ceil() + 2.0 * pad).clamp(1.0, px as f32) as u32;
    let ss: u32 = if px <= 1024 { 3 } else { 2 };
    let (bw, bh) = (out_w * ss, out_h * ss);
    let n = (bw as usize) * (bh as usize);
    let mut depth = vec![f32::INFINITY; n];
    let mut color = vec![[0u8; 3]; n];
    let fs = ss as f32;

    for t in &mesh.tris {
        let Some(mut normal) = norm(cross(sub(t[1], t[0]), sub(t[2], t[0]))) else {
            continue;
        };
        if dot(normal, view) < 0.0 {
            normal = [-normal[0], -normal[1], -normal[2]];
        }
        let diffuse = 0.24 + 0.62 * dot(normal, key).max(0.0) + 0.22 * dot(normal, fill).max(0.0);
        let spec = 0.16 * dot(normal, half).max(0.0).powi(24);
        let shade = |c: f32| ((c * diffuse + spec).clamp(0.0, 1.0) * 255.0).round() as u8;
        let rgb = [shade(ALBEDO[0]), shade(ALBEDO[1]), shade(ALBEDO[2])];

        let s: Vec<[f32; 3]> = t
            .iter()
            .map(|&p| {
                let q = project(p);
                [
                    ((q[0] - lo[0]) * scale + pad) * fs,
                    ((hi[1] - q[1]) * scale + pad) * fs,
                    q[2],
                ]
            })
            .collect();
        let area =
            (s[1][0] - s[0][0]) * (s[2][1] - s[0][1]) - (s[2][0] - s[0][0]) * (s[1][1] - s[0][1]);
        if area.abs() < 1e-12 {
            continue;
        }
        let x0 = s
            .iter()
            .map(|p| p[0])
            .fold(f32::MAX, f32::min)
            .floor()
            .max(0.0) as u32;
        let x1 = (s.iter().map(|p| p[0]).fold(f32::MIN, f32::max).ceil() as u32).min(bw);
        let y0 = s
            .iter()
            .map(|p| p[1])
            .fold(f32::MAX, f32::min)
            .floor()
            .max(0.0) as u32;
        let y1 = (s.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil() as u32).min(bh);
        let inv = 1.0 / area;
        for y in y0..y1 {
            let py = y as f32 + 0.5;
            for x in x0..x1 {
                let px_ = x as f32 + 0.5;
                let e = |a: [f32; 3], b: [f32; 3]| {
                    (b[0] - a[0]) * (py - a[1]) - (b[1] - a[1]) * (px_ - a[0])
                };
                let w0 = e(s[1], s[2]) * inv;
                let w1 = e(s[2], s[0]) * inv;
                let w2 = e(s[0], s[1]) * inv;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let z = w0 * s[0][2] + w1 * s[1][2] + w2 * s[2][2];
                let i = y as usize * bw as usize + x as usize;
                if z < depth[i] {
                    depth[i] = z;
                    color[i] = rgb;
                }
            }
        }
    }

    // Average each ss×ss block; alpha is the covered share.
    let mut img = RgbaImage::new(out_w, out_h);
    let full = ss * ss;
    for (x, y, p) in img.enumerate_pixels_mut() {
        let (mut sum, mut hits) = ([0u32; 3], 0u32);
        for dy in 0..ss {
            let row = (y * ss + dy) as usize * bw as usize;
            for dx in 0..ss {
                let i = row + (x * ss + dx) as usize;
                if depth[i].is_finite() {
                    hits += 1;
                    for c in 0..3 {
                        sum[c] += u32::from(color[i][c]);
                    }
                }
            }
        }
        if let (Some(r), Some(g), Some(b)) = (
            sum[0].checked_div(hits),
            sum[1].checked_div(hits),
            sum[2].checked_div(hits),
        ) {
            p.0 = [
                r as u8,
                g as u8,
                b as u8,
                ((hits * 255 + full / 2) / full) as u8,
            ];
        }
    }
    Ok(img)
}

#[cfg(test)]
pub fn cube() -> Mesh {
    let c = |x: f32, y: f32, z: f32| [x, y, z];
    let v = [
        c(0., 0., 0.),
        c(10., 0., 0.),
        c(10., 10., 0.),
        c(0., 10., 0.),
        c(0., 0., 10.),
        c(10., 0., 10.),
        c(10., 10., 10.),
        c(0., 10., 10.),
    ];
    let faces = [
        [0, 2, 1],
        [0, 3, 2],
        [4, 5, 6],
        [4, 6, 7],
        [0, 1, 5],
        [0, 5, 4],
        [1, 2, 6],
        [1, 6, 5],
        [2, 3, 7],
        [2, 7, 6],
        [3, 0, 4],
        [3, 4, 7],
    ];
    Mesh {
        tris: faces.iter().map(|f| [v[f[0]], v[f[1]], v[f[2]]]).collect(),
    }
}

#[cfg(test)]
pub fn binary_stl(m: &Mesh) -> Vec<u8> {
    let mut out = vec![0u8; 80];
    out.extend((m.tris.len() as u32).to_le_bytes());
    for t in &m.tris {
        out.extend([0u8; 12]);
        for p in t {
            for c in p {
                out.extend(c.to_le_bytes());
            }
        }
        out.extend([0u8; 2]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reads_binary_and_ascii_stl() {
        let bin = binary_stl(&cube());
        let m = read_stl(Cursor::new(&bin), bin.len() as u64).unwrap();
        assert_eq!(m.tris.len(), 12);
        // A binary file whose header starts with "solid" is still binary.
        let mut tricky = bin.clone();
        tricky[..5].copy_from_slice(b"solid");
        assert_eq!(
            read_stl(Cursor::new(&tricky), tricky.len() as u64)
                .unwrap()
                .tris
                .len(),
            12
        );
        let ascii = "solid t\n facet normal 0 0 1\n  outer loop\n   vertex 0 0 0\n   vertex 1 0 0\n   vertex 1 1 0\n  endloop\n endfacet\nendsolid t\n";
        let m = read_stl(Cursor::new(ascii), ascii.len() as u64).unwrap();
        assert_eq!(m.tris, vec![[[0., 0., 0.], [1., 0., 0.], [1., 1., 0.]]]);
        assert!(read_stl(Cursor::new(b"garbage"), 7).is_err());
        // A count that doesn't fit the file.
        let mut short = bin.clone();
        short.truncate(200);
        assert!(read_stl(Cursor::new(&short), 200).is_err());
    }

    #[test]
    fn renders_a_cube_with_transparent_corners() {
        let img = render(&cube(), 96).unwrap();
        assert!(img.width() <= 96 && img.height() <= 96);
        assert!(img.width() == 96 || img.height() == 96);
        assert_eq!(img.get_pixel(0, 0).0[3], 0, "corner is background");
        let c = img.get_pixel(img.width() / 2, img.height() / 2).0;
        assert_eq!(c[3], 255, "center is covered");
        // Three visible faces get three different shades.
        let mut shades: Vec<u8> = img
            .pixels()
            .filter(|p| p.0[3] == 255)
            .map(|p| p.0[0])
            .collect();
        shades.sort_unstable();
        shades.dedup();
        assert!(shades.len() >= 3, "{shades:?}");
    }
}
