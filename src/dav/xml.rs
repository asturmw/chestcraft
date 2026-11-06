//! WebDAV XML: request bodies are parsed with quick-xml into a small tree (namespace-aware;
//! DTDs and custom entities are refused, so nothing expands), responses are written by hand.

use quick_xml::NsReader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;

pub const DAV: &str = "DAV:";
pub const MS: &str = "urn:schemas-microsoft-com:";

const MAX_DEPTH: usize = 32;
const MAX_ELEMENTS: usize = 10_000;

/// A namespaced element name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Name {
    pub ns: String,
    pub local: String,
}

impl Name {
    pub fn new(ns: &str, local: &str) -> Name {
        Name {
            ns: ns.to_string(),
            local: local.to_string(),
        }
    }

    pub fn dav(local: &str) -> Name {
        Name::new(DAV, local)
    }

    pub fn is(&self, ns: &str, local: &str) -> bool {
        self.ns == ns && self.local == local
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub name: Name,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    fn children_named<'a>(
        &'a self,
        ns: &'a str,
        local: &'a str,
    ) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |c| c.name.is(ns, local))
    }

    fn has(&self, ns: &str, local: &str) -> bool {
        self.children.iter().any(|c| c.name.is(ns, local))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("malformed XML body")]
pub struct BadXml;

/// Parse a whole document into its root element. `None` for an empty body.
pub fn parse(body: &[u8]) -> Result<Option<Element>, BadXml> {
    let text = std::str::from_utf8(body).map_err(|_| BadXml)?;
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(None);
    }
    // No text trimming: it would cut the spaces around `&amp;` in "a &amp; b". Values are
    // trimmed where they are used.
    let mut r = NsReader::from_str(text);
    r.config_mut().trim_text(false);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut count = 0usize;
    loop {
        let (ns, ev) = r.read_resolved_event().map_err(|_| BadXml)?;
        let ns = match ns {
            ResolveResult::Bound(n) => n.0.to_string(),
            ResolveResult::Unbound => String::new(),
            ResolveResult::Unknown(_) => return Err(BadXml),
        };
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                count += 1;
                if count > MAX_ELEMENTS || stack.len() >= MAX_DEPTH || root.is_some() {
                    return Err(BadXml);
                }
                stack.push(Element {
                    name: Name {
                        ns,
                        local: e.local_name().as_ref().to_string(),
                    },
                    children: Vec::new(),
                    text: String::new(),
                });
                if matches!(ev, Event::Empty(_)) {
                    close(&mut stack, &mut root);
                }
            }
            Event::End(_) => close(&mut stack, &mut root),
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.xml10_content());
                }
            }
            Event::CData(c) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&c.xml10_content());
                }
            }
            Event::GeneralRef(g) => {
                let s = match g.resolve_char_ref().map_err(|_| BadXml)? {
                    Some(ch) => ch.to_string(),
                    None => resolve_predefined_entity(&g).ok_or(BadXml)?.to_string(),
                };
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&s);
                }
            }
            Event::DocType(_) => return Err(BadXml),
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(BadXml);
    }
    root.map(Some).ok_or(BadXml)
}

fn close(stack: &mut Vec<Element>, root: &mut Option<Element>) {
    if let Some(el) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.children.push(el),
            None => *root = Some(el),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Requests

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Propfind {
    AllProp,
    PropName,
    Props(Vec<Name>),
}

pub fn propfind(body: &[u8]) -> Result<Propfind, BadXml> {
    let Some(root) = parse(body)? else {
        return Ok(Propfind::AllProp);
    };
    if !root.name.is(DAV, "propfind") {
        return Err(BadXml);
    }
    if root.has(DAV, "propname") {
        return Ok(Propfind::PropName);
    }
    if let Some(prop) = root.children_named(DAV, "prop").next() {
        return Ok(Propfind::Props(
            prop.children.iter().map(|c| c.name.clone()).collect(),
        ));
    }
    if root.has(DAV, "allprop") {
        return Ok(Propfind::AllProp);
    }
    Err(BadXml)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchOp {
    pub set: bool,
    pub name: Name,
    /// The text value; None when the value has child elements.
    pub value: Option<String>,
}

pub fn proppatch(body: &[u8]) -> Result<Vec<PatchOp>, BadXml> {
    let root = parse(body)?.ok_or(BadXml)?;
    if !root.name.is(DAV, "propertyupdate") {
        return Err(BadXml);
    }
    let mut ops = Vec::new();
    for action in &root.children {
        let set = match (action.name.ns.as_str(), action.name.local.as_str()) {
            (DAV, "set") => true,
            (DAV, "remove") => false,
            _ => continue,
        };
        for prop in action.children_named(DAV, "prop") {
            for p in &prop.children {
                ops.push(PatchOp {
                    set,
                    name: p.name.clone(),
                    value: p.children.is_empty().then(|| p.text.trim().to_string()),
                });
            }
        }
    }
    Ok(ops)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockInfo {
    pub shared: bool,
}

/// `None` for an empty body (a lock refresh).
pub fn lockinfo(body: &[u8]) -> Result<Option<LockInfo>, BadXml> {
    let Some(root) = parse(body)? else {
        return Ok(None);
    };
    if !root.name.is(DAV, "lockinfo") {
        return Err(BadXml);
    }
    let shared = root
        .children_named(DAV, "lockscope")
        .any(|s| s.has(DAV, "shared"));
    Ok(Some(LockInfo { shared }))
}

// ---------------------------------------------------------------------------------------
// Responses

pub const HEADER: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n";

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// `<D:name>inner</D:name>` for DAV properties, with an own namespace declaration for
/// others. `inner` is already XML; `None` writes an empty element.
pub fn element(out: &mut String, name: &Name, inner: Option<&str>) {
    let (open, close) = if name.ns == DAV {
        (format!("D:{}", name.local), format!("D:{}", name.local))
    } else if name.ns.is_empty() {
        (format!("{} xmlns=\"\"", name.local), name.local.clone())
    } else {
        (
            format!("x:{} xmlns:x=\"{}\"", name.local, esc(&name.ns)),
            format!("x:{}", name.local),
        )
    };
    match inner {
        None | Some("") => {
            out.push('<');
            out.push_str(&open);
            out.push_str("/>");
        }
        Some(inner) => {
            out.push('<');
            out.push_str(&open);
            out.push('>');
            out.push_str(inner);
            out.push_str("</");
            out.push_str(&close);
            out.push('>');
        }
    }
}

pub fn status_line(code: http::StatusCode) -> String {
    format!(
        "HTTP/1.1 {} {}",
        code.as_u16(),
        code.canonical_reason().unwrap_or("")
    )
}

/// One `<D:propstat>` group.
pub fn propstat(out: &mut String, code: http::StatusCode, props: &str) {
    out.push_str("<D:propstat><D:prop>");
    out.push_str(props);
    out.push_str("</D:prop><D:status>");
    out.push_str(&status_line(code));
    out.push_str("</D:status></D:propstat>");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propfind_bodies() {
        assert_eq!(propfind(b"").unwrap(), Propfind::AllProp);
        assert_eq!(
            propfind(
                b"<?xml version=\"1.0\"?><D:propfind xmlns:D=\"DAV:\"><D:allprop/></D:propfind>"
            )
            .unwrap(),
            Propfind::AllProp
        );
        assert_eq!(
            propfind(b"<propfind xmlns=\"DAV:\"><propname/></propfind>").unwrap(),
            Propfind::PropName
        );
        // rclone's owncloud request.
        let rclone = br#"<?xml version="1.0"?>
<d:propfind  xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns">
 <d:prop>
  <d:displayname />
  <d:getlastmodified />
  <d:resourcetype />
  <oc:checksums />
 </d:prop>
</d:propfind>"#;
        assert_eq!(
            propfind(rclone).unwrap(),
            Propfind::Props(vec![
                Name::dav("displayname"),
                Name::dav("getlastmodified"),
                Name::dav("resourcetype"),
                Name::new("http://owncloud.org/ns", "checksums"),
            ])
        );
        assert!(propfind(b"<propfind/>").is_err(), "not in DAV:");
        assert!(propfind(b"<D:propfind xmlns:D=\"DAV:\">").is_err());
        assert!(propfind(b"<x:propfind/>").is_err(), "unknown prefix");
    }

    #[test]
    fn entities_are_not_expanded() {
        let bomb = br#"<?xml version="1.0"?>
<!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;">]>
<D:propfind xmlns:D="DAV:"><D:prop><D:x>&lol2;</D:x></D:prop></D:propfind>"#;
        assert_eq!(propfind(bomb), Err(BadXml));
        let ops = proppatch(
            br#"<D:propertyupdate xmlns:D="DAV:"><D:set><D:prop><D:displayname>a &amp; b &#x41;</D:displayname></D:prop></D:set></D:propertyupdate>"#,
        )
        .unwrap();
        assert_eq!(ops[0].value.as_deref(), Some("a & b A"));
        assert!(proppatch(br#"<D:propertyupdate xmlns:D="DAV:"><D:set><D:prop><D:x>&foo;</D:x></D:prop></D:set></D:propertyupdate>"#).is_err());
    }

    #[test]
    fn proppatch_of_rclone_and_windows() {
        let rclone = br#"<?xml version="1.0" encoding="utf-8" ?>
<D:propertyupdate xmlns:D="DAV:">
 <D:set>
  <D:prop>
   <lastmodified xmlns="DAV:">1700000000</lastmodified>
  </D:prop>
 </D:set>
</D:propertyupdate>"#;
        assert_eq!(
            proppatch(rclone).unwrap(),
            vec![PatchOp {
                set: true,
                name: Name::dav("lastmodified"),
                value: Some("1700000000".into())
            }]
        );
        let win = br#"<?xml version="1.0" encoding="utf-8" ?><D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:schemas-microsoft-com:"><D:set><D:prop><Z:Win32LastModifiedTime>Wed, 15 Nov 2023 10:00:00 GMT</Z:Win32LastModifiedTime><Z:Win32FileAttributes>00000020</Z:Win32FileAttributes></D:prop></D:set><D:remove><D:prop><Z:Other/></D:prop></D:remove></D:propertyupdate>"#;
        let ops = proppatch(win).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(ops[0].name.is(MS, "Win32LastModifiedTime") && ops[0].set);
        assert!(!ops[2].set && ops[2].value.as_deref() == Some(""));
    }

    #[test]
    fn lock_bodies() {
        assert_eq!(lockinfo(b"").unwrap(), None);
        let finder = br#"<?xml version="1.0" encoding="utf-8"?>
<D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><D:href>http://www.apple.com/webdav_fs/</D:href></D:owner></D:lockinfo>"#;
        assert_eq!(lockinfo(finder).unwrap(), Some(LockInfo { shared: false }));
        assert_eq!(
            lockinfo(b"<lockinfo xmlns=\"DAV:\"><lockscope><shared/></lockscope></lockinfo>")
                .unwrap(),
            Some(LockInfo { shared: true })
        );
    }

    #[test]
    fn writing() {
        let mut s = String::new();
        element(&mut s, &Name::dav("displayname"), Some(&esc("a<b & \"c\"")));
        element(
            &mut s,
            &Name::new("http://owncloud.org/ns", "checksums"),
            None,
        );
        element(&mut s, &Name::new("", "plain"), None);
        assert_eq!(
            s,
            "<D:displayname>a&lt;b &amp; &quot;c&quot;</D:displayname>\
             <x:checksums xmlns:x=\"http://owncloud.org/ns\"/><plain xmlns=\"\"/>"
        );
        assert_eq!(
            status_line(http::StatusCode::NOT_FOUND),
            "HTTP/1.1 404 Not Found"
        );
    }
}
