//! File types by name: the icon a file gets (`data-type`, `i-<type>` in the icon sprite),
//! how the viewer shows it, and where its thumbnail comes from.

/// The icon family of a file (`data-type` in the markup).
pub fn icon_type(name: &str) -> &'static str {
    let ext = ext(name);
    let e = ext.as_str();
    match e {
        "jpg" | "jpeg" | "jfif" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "avif"
        | "heic" | "heif" | "ico" | "jxl" | "qoi" | "raw" | "cr2" | "cr3" | "nef" | "arw"
        | "dng" | "orf" | "rw2" => "image",
        "pdf" => "pdf",
        "txt" | "md" | "markdown" | "log" | "nfo" => "text",
        "doc" | "docx" | "odt" | "rtf" | "pages" => "doc",
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => "sheet",
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "lz4" | "cab" => {
            "archive"
        }
        "stl" | "3mf" | "step" | "stp" | "obj" | "iges" | "igs" | "f3d" | "f3z" | "blend"
        | "fbx" | "ply" | "amf" | "scad" => "model",
        "gcode" | "gco" | "g" | "bgcode" => "gcode",
        "svg" | "dxf" | "ai" | "eps" | "lbrn" | "lbrn2" | "cdr" => "vector",
        "iso" | "img" | "dmg" | "vhd" | "vhdx" | "qcow2" | "vmdk" | "vdi" => "disk",
        _ if VIDEO.contains(&e) => "video",
        _ if AUDIO.contains(&e) => "audio",
        _ if CODE.contains(&e) => "code",
        _ => "file",
    }
}

const VIDEO: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "mpg", "mpeg", "ts", "mts", "m2ts",
    "3gp", "ogv",
];
const AUDIO: &[&str] = &[
    "mp3", "m4a", "aac", "flac", "wav", "ogg", "oga", "opus", "wma", "aiff", "aif",
];
const CODE: &[&str] = &[
    "rs",
    "py",
    "js",
    "mjs",
    "ts",
    "tsx",
    "jsx",
    "json",
    "toml",
    "yaml",
    "yml",
    "sh",
    "bash",
    "fish",
    "zsh",
    "html",
    "htm",
    "css",
    "scss",
    "c",
    "h",
    "cpp",
    "hpp",
    "cc",
    "go",
    "java",
    "kt",
    "rb",
    "php",
    "pl",
    "lua",
    "sql",
    "xml",
    "ini",
    "conf",
    "cfg",
    "env",
    "properties",
    "bat",
    "ps1",
    "cmake",
    "mk",
    "dockerfile",
    "nix",
    "vue",
    "svelte",
    "swift",
    "cs",
    "diff",
    "patch",
];

/// Where a thumbnail comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbSource {
    /// Raster image decoded in process.
    Image,
    /// Still image ffmpeg decodes (HEIC, AVIF).
    Still,
    /// A frame of a video (ffmpeg).
    Video,
    Stl,
    ThreeMf,
    Gcode,
    Bgcode,
    Svg,
}

impl ThumbSource {
    pub fn tag(self) -> &'static str {
        match self {
            ThumbSource::Image => "image",
            ThumbSource::Still => "still",
            ThumbSource::Video => "video",
            ThumbSource::Stl => "stl",
            ThumbSource::ThreeMf => "3mf",
            ThumbSource::Gcode => "gcode",
            ThumbSource::Bgcode => "bgcode",
            ThumbSource::Svg => "svg",
        }
    }

    pub fn uses_ffmpeg(self) -> bool {
        matches!(self, ThumbSource::Still | ThumbSource::Video)
    }
}

pub fn thumb_source(name: &str) -> Option<ThumbSource> {
    let ext = ext(name);
    Some(match ext.as_str() {
        "jpg" | "jpeg" | "jfif" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "qoi" => {
            ThumbSource::Image
        }
        "heic" | "heif" | "avif" => ThumbSource::Still,
        "stl" => ThumbSource::Stl,
        "3mf" => ThumbSource::ThreeMf,
        "gcode" | "gco" | "g" => ThumbSource::Gcode,
        "bgcode" => ThumbSource::Bgcode,
        "svg" => ThumbSource::Svg,
        e if VIDEO.contains(&e) => ThumbSource::Video,
        _ => return None,
    })
}

/// How the viewer shows a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview {
    /// The large thumbnail, with a link to the original.
    Image,
    Video,
    Audio,
    Pdf,
    Markdown,
    Text,
    /// The large thumbnail of something that isn't a picture (3D model, G-code, SVG).
    Rendered,
    /// Text detection by content (files without a known extension).
    Sniff,
    None,
}

pub fn preview(name: &str) -> Preview {
    let ext = ext(name);
    let e = ext.as_str();
    match thumb_source(name) {
        Some(ThumbSource::Image | ThumbSource::Still) => return Preview::Image,
        Some(ThumbSource::Video) => return Preview::Video,
        Some(_) => return Preview::Rendered,
        None => {}
    }
    match e {
        "pdf" => Preview::Pdf,
        "md" | "markdown" => Preview::Markdown,
        "txt" | "log" | "nfo" | "csv" | "tsv" => Preview::Text,
        "" => Preview::Sniff,
        _ if AUDIO.contains(&e) => Preview::Audio,
        _ if CODE.contains(&e) => Preview::Text,
        _ => Preview::None,
    }
}

/// Lowercase extension, empty when there is none (also for dotfiles like `.bashrc`).
fn ext(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, e)) if !stem.is_empty() => e.to_ascii_lowercase(),
        _ => {
            // Well-known names without an extension.
            match name.to_ascii_lowercase().as_str() {
                "dockerfile" | "makefile" => "mk".into(),
                _ => String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_by_extension() {
        assert_eq!(icon_type("IMG_2041.JPG"), "image");
        assert_eq!(icon_type("Gehäuse_Deckel.stl"), "model");
        assert_eq!(icon_type("Halterung_v3.step"), "model");
        assert_eq!(icon_type("Deckel_PLA_0.2mm.gcode"), "gcode");
        assert_eq!(icon_type("Schild Werkstatt.svg"), "vector");
        assert_eq!(icon_type("Rechnung 2026-014.pdf"), "pdf");
        assert_eq!(icon_type("film.mkv"), "video");
        assert_eq!(icon_type("lied.flac"), "audio");
        assert_eq!(icon_type("main.rs"), "code");
        assert_eq!(icon_type("README"), "file");
        assert_eq!(icon_type(".bashrc"), "file");

        assert_eq!(thumb_source("a.HEIC"), Some(ThumbSource::Still));
        assert_eq!(thumb_source("plate.gcode.3mf"), Some(ThumbSource::ThreeMf));
        assert_eq!(thumb_source("Halterung_v3.step"), None);

        assert_eq!(preview("x.stl"), Preview::Rendered);
        assert_eq!(preview("notes.md"), Preview::Markdown);
        assert_eq!(preview("Makefile"), Preview::Text);
        assert_eq!(preview("README"), Preview::Sniff);
        assert_eq!(preview("song.mp3"), Preview::Audio);
        assert_eq!(preview("archive.zip"), Preview::None);
    }
}
