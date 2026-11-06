//! 3MF: a zip package. The slicer's thumbnail (the package relationship of type
//! `…/metadata/thumbnail`, else `Metadata/thumbnail.png` or `Metadata/plate_1.png`) wins;
//! without one the mesh of the build is rendered. Objects may be meshes or components of
//! other objects, also in other model parts (production extension, `p:path`), each with a
//! 3×4 transform.

use super::mesh::{MAX_TRIANGLES, Mesh, V3};
use super::{Result, unusable};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::HashMap;
use std::io::{BufReader, Read, Seek};
use zip::ZipArchive;

const THUMB_REL: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail";
const MODEL_REL: &str = "http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel";
const MAX_PICTURE: u64 = 32 << 20;
const MAX_RELS: u64 = 1 << 20;
const MAX_VERTICES: usize = 2 * MAX_TRIANGLES;
const MAX_DEPTH: usize = 16;

pub enum Content {
    Picture(Vec<u8>),
    Mesh(Mesh),
}

pub fn read(f: impl Read + Seek) -> Result<Content> {
    let mut zip = ZipArchive::new(f).map_err(|e| unusable(format!("3mf: {e}")))?;
    let rels = read_entry(&mut zip, "_rels/.rels", MAX_RELS)
        .ok()
        .map(|b| parse_rels(&b))
        .unwrap_or_default();
    let mut pictures: Vec<String> = rels
        .iter()
        .filter(|(ty, _)| ty == THUMB_REL)
        .map(|(_, t)| t.clone())
        .collect();
    pictures.push("Metadata/thumbnail.png".into());
    pictures.push("Metadata/plate_1.png".into());
    for p in &pictures {
        if let Ok(b) = read_entry(&mut zip, p, MAX_PICTURE)
            && !b.is_empty()
        {
            return Ok(Content::Picture(b));
        }
    }
    let root = rels
        .iter()
        .find(|(ty, _)| ty == MODEL_REL)
        .map(|(_, t)| part_name(t))
        .unwrap_or_else(|| "3D/3dmodel.model".into());
    let mut models = Models::default();
    models.load(&mut zip, &root)?;
    let mut mesh = Mesh::default();
    let items = models
        .parts
        .get(&root)
        .map(|p| p.build.clone())
        .unwrap_or_default();
    if items.is_empty() {
        // No build section: show every object of the root part.
        let ids: Vec<u32> = models
            .parts
            .get(&root)
            .map(|p| p.objects.keys().copied().collect())
            .unwrap_or_default();
        for id in ids {
            models.emit(&mut zip, &root, id, IDENTITY, &mut mesh, 0)?;
        }
    } else {
        for item in items {
            let part = item.path.unwrap_or_else(|| root.clone());
            models.emit(&mut zip, &part, item.object, item.transform, &mut mesh, 0)?;
        }
    }
    if mesh.tris.is_empty() {
        return Err(unusable("3mf without triangles"));
    }
    Ok(Content::Mesh(mesh))
}

/// Zip entry names have no leading slash; relationship targets do.
fn part_name(target: &str) -> String {
    target.trim_start_matches('/').to_string()
}

fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str, max: u64) -> Result<Vec<u8>> {
    let entry = zip
        .by_name(&part_name(name))
        .map_err(|e| unusable(format!("{name}: {e}")))?;
    let mut out = Vec::new();
    entry.take(max + 1).read_to_end(&mut out)?;
    if out.len() as u64 > max {
        return Err(unusable(format!("{name} too large")));
    }
    Ok(out)
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .map(|a| a.value.into_owned())
}

/// (type, target) of every package relationship.
fn parse_rels(xml: &[u8]) -> Vec<(String, String)> {
    let mut r = Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) if e.local_name().as_ref() == "Relationship" => {
                if let (Some(ty), Some(target)) = (attr(&e, "Type"), attr(&e, "Target")) {
                    out.push((ty, target));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

type Mat = [f32; 12];
const IDENTITY: Mat = [1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0.];

/// `m00 m01 m02 m10 m11 m12 m20 m21 m22 m30 m31 m32`, applied to row vectors.
fn parse_mat(s: Option<String>) -> Mat {
    let Some(s) = s else { return IDENTITY };
    let v: Vec<f32> = s
        .split_ascii_whitespace()
        .filter_map(|w| w.parse().ok())
        .collect();
    match <[f32; 12]>::try_from(v) {
        Ok(m) if m.iter().all(|c| c.is_finite()) => m,
        _ => IDENTITY,
    }
}

fn apply(m: &Mat, p: V3) -> V3 {
    [
        p[0] * m[0] + p[1] * m[3] + p[2] * m[6] + m[9],
        p[0] * m[1] + p[1] * m[4] + p[2] * m[7] + m[10],
        p[0] * m[2] + p[1] * m[5] + p[2] * m[8] + m[11],
    ]
}

/// First `a`, then `b`.
fn then(a: &Mat, b: &Mat) -> Mat {
    let mut out = [0f32; 12];
    for row in 0..4 {
        for col in 0..3 {
            let mut v = if row == 3 { b[9 + col] } else { 0.0 };
            for k in 0..3 {
                v += a[row * 3 + k] * b[k * 3 + col];
            }
            out[row * 3 + col] = v;
        }
    }
    out
}

#[derive(Clone)]
struct Item {
    object: u32,
    path: Option<String>,
    transform: Mat,
}

enum Object {
    Mesh { verts: Vec<V3>, tris: Vec<[u32; 3]> },
    Components(Vec<Item>),
}

#[derive(Default)]
struct Part {
    objects: HashMap<u32, Object>,
    build: Vec<Item>,
}

#[derive(Default)]
struct Models {
    parts: HashMap<String, Part>,
    vertices: usize,
}

impl Models {
    fn load<R: Read + Seek>(&mut self, zip: &mut ZipArchive<R>, name: &str) -> Result<()> {
        if self.parts.contains_key(name) {
            return Ok(());
        }
        let entry = zip
            .by_name(name)
            .map_err(|e| unusable(format!("{name}: {e}")))?;
        let part = parse_model(BufReader::with_capacity(1 << 16, entry), &mut self.vertices)?;
        self.parts.insert(name.to_string(), part);
        Ok(())
    }

    fn emit<R: Read + Seek>(
        &mut self,
        zip: &mut ZipArchive<R>,
        part: &str,
        id: u32,
        m: Mat,
        mesh: &mut Mesh,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(unusable("3mf components nest too deep"));
        }
        self.load(zip, part)?;
        let comps = match self.parts.get(part).and_then(|p| p.objects.get(&id)) {
            None => return Ok(()),
            Some(Object::Mesh { verts, tris }) => {
                for t in tris {
                    let v = |i: u32| verts.get(i as usize).map(|&p| apply(&m, p));
                    if let (Some(a), Some(b), Some(c)) = (v(t[0]), v(t[1]), v(t[2])) {
                        mesh.push([a, b, c])?;
                    }
                }
                return Ok(());
            }
            Some(Object::Components(c)) => c.clone(),
        };
        for c in comps {
            let sub = c.path.unwrap_or_else(|| part.to_string());
            self.emit(zip, &sub, c.object, then(&c.transform, &m), mesh, depth + 1)?;
        }
        Ok(())
    }
}

fn parse_model(r: impl std::io::BufRead, vertex_count: &mut usize) -> Result<Part> {
    let mut x = Reader::from_reader(r);
    let mut buf = Vec::new();
    let mut part = Part::default();
    let mut current: Option<(u32, Object)> = None;
    let num = |e: &BytesStart, n: &str| -> Option<f32> {
        e.attributes()
            .flatten()
            .find(|a| a.key.local_name().as_ref() == n)
            .and_then(|a| a.value.trim().parse().ok())
    };
    let idx = |e: &BytesStart, n: &str| -> Option<u32> {
        e.attributes()
            .flatten()
            .find(|a| a.key.local_name().as_ref() == n)
            .and_then(|a| a.value.trim().parse().ok())
    };
    let item = |e: &BytesStart| -> Option<Item> {
        Some(Item {
            object: idx(e, "objectid")?,
            path: attr(e, "path").map(|p| part_name(&p)),
            transform: parse_mat(attr(e, "transform")),
        })
    };
    loop {
        let ev = x
            .read_event_into(&mut buf)
            .map_err(|e| unusable(format!("3mf xml: {e}")))?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => match e.local_name().as_ref() {
                "object" => {
                    if let Some((id, o)) = current.take() {
                        part.objects.insert(id, o);
                    }
                    if let Some(id) = idx(e, "id") {
                        current = Some((id, Object::Components(Vec::new())));
                    }
                }
                "mesh" => {
                    if let Some((_, o)) = &mut current {
                        *o = Object::Mesh {
                            verts: Vec::new(),
                            tris: Vec::new(),
                        };
                    }
                }
                "vertex" => {
                    if let Some((_, Object::Mesh { verts, .. })) = &mut current {
                        *vertex_count += 1;
                        if *vertex_count > MAX_VERTICES {
                            return Err(unusable("mesh too large"));
                        }
                        verts.push([
                            num(e, "x").unwrap_or(f32::NAN),
                            num(e, "y").unwrap_or(f32::NAN),
                            num(e, "z").unwrap_or(f32::NAN),
                        ]);
                    }
                }
                "triangle" => {
                    if let Some((_, Object::Mesh { tris, .. })) = &mut current
                        && let (Some(a), Some(b), Some(c)) =
                            (idx(e, "v1"), idx(e, "v2"), idx(e, "v3"))
                    {
                        if tris.len() >= MAX_TRIANGLES {
                            return Err(unusable("mesh too large"));
                        }
                        tris.push([a, b, c]);
                    }
                }
                "component" => {
                    if let Some((_, Object::Components(list))) = &mut current
                        && let Some(it) = item(e)
                    {
                        list.push(it);
                    }
                }
                "item" => {
                    if let Some(it) = item(e) {
                        part.build.push(it);
                    }
                }
                _ => {}
            },
            Event::End(ref e) if e.local_name().as_ref() == "object" => {
                if let Some((id, o)) = current.take() {
                    part.objects.insert(id, o);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if let Some((id, o)) = current.take() {
        part.objects.insert(id, o);
    }
    Ok(part)
}

#[cfg(test)]
pub fn package(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in files {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
 <Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/>
</Relationships>"#;

    /// A root part that places a component from another part twice.
    const ROOT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"
  xmlns:p="http://schemas.microsoft.com/3dmanufacturing/production/2015/06">
 <resources>
  <object id="2" type="model">
   <components><component p:path="/3D/Objects/object_1.model" objectid="1"/></components>
  </object>
 </resources>
 <build>
  <item objectid="2"/>
  <item objectid="2" transform="1 0 0 0 1 0 0 0 1 20 0 0"/>
 </build>
</model>"#;

    const OBJECT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
 <resources>
  <object id="1" type="model">
   <mesh>
    <vertices>
     <vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/>
     <vertex x="0" y="0" z="10"/>
    </vertices>
    <triangles>
     <triangle v1="0" v2="2" v3="1"/><triangle v1="0" v2="1" v3="3"/>
     <triangle v1="1" v2="2" v3="3"/><triangle v1="0" v2="3" v3="2"/>
     <triangle v1="0" v2="1" v3="99"/>
    </triangles>
   </mesh>
  </object>
 </resources>
</model>"#;

    #[test]
    fn embedded_thumbnail_wins() {
        let rels = RELS.replace(
            "</Relationships>",
            r#"<Relationship Target="/Metadata/plate_7.png" Id="r1" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail"/></Relationships>"#,
        );
        let pkg = package(&[
            ("_rels/.rels", rels.as_bytes()),
            ("3D/3dmodel.model", ROOT.as_bytes()),
            ("Metadata/plate_7.png", b"PNGDATA"),
        ]);
        match read(Cursor::new(pkg)).unwrap() {
            Content::Picture(b) => assert_eq!(b, b"PNGDATA"),
            Content::Mesh(_) => panic!("expected the picture"),
        }
    }

    #[test]
    fn components_across_parts_with_transforms() {
        let pkg = package(&[
            ("_rels/.rels", RELS.as_bytes()),
            ("3D/3dmodel.model", ROOT.as_bytes()),
            ("3D/Objects/object_1.model", OBJECT.as_bytes()),
        ]);
        let Content::Mesh(m) = read(Cursor::new(pkg)).unwrap() else {
            panic!("expected a mesh");
        };
        // Two placements of four valid triangles; the one with a bad index is dropped.
        assert_eq!(m.tris.len(), 8);
        let max_x = m
            .tris
            .iter()
            .flatten()
            .map(|p| p[0])
            .fold(f32::MIN, f32::max);
        assert_eq!(max_x, 30.0);
    }

    #[test]
    fn matrices_compose_in_order() {
        let shift = parse_mat(Some("1 0 0 0 1 0 0 0 1 5 0 0".into()));
        let scale = parse_mat(Some("2 0 0 0 2 0 0 0 2 0 0 0".into()));
        // Shift first, then scale: (1+5)*2.
        assert_eq!(apply(&then(&shift, &scale), [1., 0., 0.]), [12., 0., 0.]);
        assert_eq!(parse_mat(Some("1 2".into())), IDENTITY);
    }

    #[test]
    fn not_a_package() {
        assert!(read(Cursor::new(b"nope".to_vec())).is_err());
    }
}
