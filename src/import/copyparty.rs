//! Reads what an account import needs from a copyparty config file (the `[section]` format
//! of copyparty 1.x): accounts, groups and who may do what in which volume.
//!
//! The rules follow copyparty's own parser (`authsrv.py`, 1.20): tabs count as spaces; a
//! comment is a line whose first non-blank character is `#`, or the rest of a line after
//! `  #` (two spaces; with one space the `#` belongs to the value); `% <file or folder>`
//! includes other files (a folder: its `*.conf` files by name); `[name]:` is `[name]`.
//! Accounts come from `[accounts]` (`name: password`) and from `a: name:password` in
//! `[global]`; a name defined twice keeps its last password. A file copyparty itself
//! refuses is refused here too, so an import never runs on a guess.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

/// A password copied out of the file. Never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(…)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Password {
    Plain(Secret),
    /// copyparty runs with `ah-alg` and keeps only a hash (`+` and 32 characters), which
    /// chestcraft can't check.
    Hashed,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub password: Password,
}

/// One `letters: names` line under a volume's `accs:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// As written, e.g. `"rw"` or `"A"`.
    pub letters: String,
    /// Account names, `*` (everyone), `@group`.
    pub names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// URL path without the outer slashes; `""` is the web root.
    pub url: String,
    /// Folder it shares, as written.
    pub path: String,
    pub grants: Vec<Grant>,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub accounts: Vec<Account>,
    pub groups: BTreeMap<String, Vec<String>>,
    pub volumes: Vec<Volume>,
    /// The group that holds every account (copyparty `grp-all`, default `acct`).
    pub all_group: String,
    /// Things the import can't compare or carry over; shown to the admin.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("{0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("{path}, line {line}: {msg}")]
    Line {
        path: PathBuf,
        line: usize,
        msg: String,
    },
    #[error(
        "{0} is in copyparty's old (v1) config format; convert it with `copyparty --vc -c <file>` first"
    )]
    OldFormat(PathBuf),
    #[error("{0}: includes nest deeper than 64 files")]
    TooDeep(PathBuf),
}

/// A line of the expanded config and where it came from.
struct Line {
    path: PathBuf,
    number: usize,
    text: String,
}

const MAX_DEPTH: usize = 64;

/// Read `path` and every file it includes.
pub fn parse_file(path: &Path) -> Result<Parsed, ParseError> {
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    expand(path, &mut Vec::new(), &mut lines, &mut warnings)?;
    let mut parsed = parse_lines(path, &lines)?;
    warnings.append(&mut parsed.warnings);
    parsed.warnings = warnings;
    Ok(parsed)
}

/// Parse config text that includes nothing (for tests and single files).
pub fn parse_str(text: &str) -> Result<Parsed, ParseError> {
    let path = PathBuf::from("copyparty.conf");
    let lines = split_lines(&path, text);
    parse_lines(&path, &lines)
}

fn split_lines(path: &Path, text: &str) -> Vec<Line> {
    text.trim_start_matches('\u{feff}')
        .replace('\t', " ")
        .split('\n')
        .enumerate()
        .map(|(i, l)| Line {
            path: path.to_path_buf(),
            number: i + 1,
            text: l.trim_end().to_string(),
        })
        .collect()
}

/// The part of a line before a `  #` comment, trimmed (copyparty: `split("  #")[0].strip()`).
fn uncomment(line: &str) -> &str {
    line.split("  #").next().unwrap_or("").trim()
}

fn expand(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    out: &mut Vec<Line>,
    warnings: &mut Vec<String>,
) -> Result<(), ParseError> {
    if stack.len() >= MAX_DEPTH {
        return Err(ParseError::TooDeep(path.to_path_buf()));
    }
    let real = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(_) if !stack.is_empty() => {
            warnings.push(format!("included {} does not exist", path.display()));
            return Ok(());
        }
        // A pipe such as /dev/stdin has no real path; read it as it is.
        Err(_) => path.to_path_buf(),
    };
    if stack.contains(&real) {
        return Ok(());
    }
    if real.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&real)
            .map_err(|e| ParseError::Read(real.clone(), e))?
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.to_lowercase().ends_with(".conf") && !n.starts_with('.'))
            .collect();
        names.sort();
        if names.is_empty() {
            warnings.push(format!(
                "included folder {} has no .conf files",
                real.display()
            ));
        }
        stack.push(real.clone());
        for n in names {
            expand(&real.join(n), stack, out, warnings)?;
        }
        stack.pop();
        return Ok(());
    }
    let text = std::fs::read_to_string(&real).map_err(|e| ParseError::Read(real.clone(), e))?;
    stack.push(real.clone());
    for line in split_lines(&real, &text) {
        let code = uncomment(&line.text);
        if let Some(inc) = code.strip_prefix("% ") {
            let dir = real.parent().unwrap_or(Path::new("/"));
            expand(&dir.join(inc.trim()), stack, out, warnings)?;
        } else {
            out.push(line);
        }
    }
    stack.pop();
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Global,
    Accounts,
    Groups,
    Volume,
    Access,
    Flags,
}

/// copyparty's `split_cfg_ln`: `a, b, key: value` → `[(a, None), (b, None), (key, value)]`.
fn split_cfg_line(mut ln: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    loop {
        ln = ln.trim();
        if ln.is_empty() {
            break;
        }
        let comma = ln.find(',');
        match ln.find(':') {
            // A colon before any comma: the rest of the line is the value.
            Some(v) if comma.is_none_or(|c| c > v) => {
                out.push((
                    ln[..v].trim().to_string(),
                    Some(ln[v + 1..].trim().to_string()),
                ));
                break;
            }
            _ => match comma {
                Some(c) => {
                    out.push((ln[..c].trim().to_string(), None));
                    ln = &ln[c + 1..];
                }
                None => {
                    out.push((ln.to_string(), None));
                    break;
                }
            },
        }
    }
    out
}

const ACCESS_LETTERS: &str = "rwmdgGhaA.";

fn parse_lines(root: &Path, lines: &[Line]) -> Result<Parsed, ParseError> {
    // copyparty's upgrade_cfg_fmt: a file without any of these markers is v1.
    let code: Vec<&str> = lines
        .iter()
        .map(|l| l.text.split('#').next().unwrap_or("").trim())
        .filter(|s| !s.is_empty())
        .collect();
    let v2 = code.iter().any(|s| {
        matches!(*s, "[global]" | "[accounts]" | "accs:" | "flags:") || s.starts_with("[/")
    }) || code.iter().all(|s| s.starts_with('%'));
    if !v2 {
        return Err(ParseError::OldFormat(root.to_path_buf()));
    }

    let mut section = Section::None;
    let mut global_accounts: Vec<(String, String)> = Vec::new();
    let mut file_accounts: Vec<(String, String)> = Vec::new();
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut volumes: Vec<Volume> = Vec::new();
    let mut warnings = Vec::new();
    let mut ah_alg = String::new();
    let mut all_group = "acct".to_string();
    // A volume header was seen and its folder line is still to come.
    let mut want_path: Option<String> = None;
    let mut in_volume = false;

    for line in lines {
        let err = |msg: String| ParseError::Line {
            path: line.path.clone(),
            line: line.number,
            msg,
        };
        let mut ln = uncomment(&line.text);
        if ln.split('#').next().unwrap_or("").trim().is_empty() {
            continue;
        }
        if ln.starts_with('[') && ln.ends_with("]:") {
            ln = &ln[..ln.len() - 1];
        }
        let sub = matches!(ln, "accs:" | "flags:");
        if ln.starts_with('[') || sub {
            if let Some(url) = &want_path {
                return Err(err(format!(
                    "the first line after [/{url}] must be the folder that volume shares"
                )));
            }
            if sub {
                section = if ln == "accs:" {
                    Section::Access
                } else {
                    Section::Flags
                };
                continue;
            }
            in_volume = false;
            section = match ln {
                "[global]" => Section::Global,
                "[accounts]" => Section::Accounts,
                "[groups]" => Section::Groups,
                _ if ln.starts_with("[/") && ln.ends_with(']') => {
                    want_path = Some(ln[1..ln.len() - 1].trim_matches('/').to_string());
                    Section::Volume
                }
                _ => return Err(err(format!("invalid section header {ln}"))),
            };
            continue;
        }
        match section {
            Section::Global => {
                for (key, value) in split_cfg_line(ln) {
                    let key = key.trim_start_matches('-').replace('_', "-");
                    match (key.as_str(), value) {
                        ("a", Some(v)) => match v.split_once(':') {
                            Some((u, p)) => global_accounts.push((u.to_string(), p.to_string())),
                            None => {
                                return Err(err(
                                    "a: must be name:password, found no colon in the value"
                                        .to_string(),
                                ));
                            }
                        },
                        ("ah-alg", Some(v)) => ah_alg = v,
                        ("grp-all", Some(v)) => all_group = v,
                        ("grp", Some(v)) => {
                            let v = v.replace('=', ":");
                            if let Some((g, members)) = v.split_once(':') {
                                groups.insert(
                                    g.to_string(),
                                    members
                                        .replace(':', ",")
                                        .split(',')
                                        .map(|m| m.trim().to_string())
                                        .collect(),
                                );
                            }
                        }
                        ("v", _) => warnings.push(format!(
                            "{}, line {}: volumes given with v: are not compared",
                            line.path.display(),
                            line.number
                        )),
                        _ => {}
                    }
                }
            }
            Section::Accounts => match ln.split_once(':') {
                Some((u, p)) => file_accounts.push((u.trim().to_string(), p.trim().to_string())),
                None => {
                    return Err(err(
                        "lines in [accounts] must be \"name: password\"".to_string()
                    ));
                }
            },
            Section::Groups => match ln.split_once(':') {
                Some((g, members)) => groups
                    .entry(g.trim().to_string())
                    .or_default()
                    .extend(members.split(',').map(|m| m.trim().to_string())),
                None => {
                    return Err(err(
                        "lines in [groups] must be \"group: name1, name2\"".to_string()
                    ));
                }
            },
            _ if want_path.is_some() => {
                let url = want_path.take().expect("checked");
                volumes.push(Volume {
                    url,
                    path: ln.to_string(),
                    grants: Vec::new(),
                });
                in_volume = true;
            }
            Section::Access if !in_volume => {}
            Section::Access => {
                let parts: Vec<&str> = ln.split(':').collect();
                let [letters, names] = parts[..] else {
                    return Err(err(
                        "accs entries must be \"letters: name1, name2\"".to_string()
                    ));
                };
                if letters.is_empty() || letters.chars().any(|c| !ACCESS_LETTERS.contains(c)) {
                    return Err(err(format!("invalid accs permissions \"{letters}\"")));
                }
                // copyparty: after `re.sub(", *", "", names)` no space may be left.
                let mut joined = String::new();
                let mut after_comma = false;
                for c in names.chars() {
                    if c == ',' || (after_comma && c == ' ') {
                        after_comma = true;
                        continue;
                    }
                    after_comma = false;
                    joined.push(c);
                }
                if joined.trim().contains(' ') {
                    return Err(err(
                        "the names in an accs entry must be separated by commas".to_string(),
                    ));
                }
                let names: Vec<String> = names
                    .replace(' ', "")
                    .split(',')
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
                    .collect();
                if names.iter().any(|n| n.contains("${")) {
                    warnings.push(format!(
                        "{}, line {}: per-user volumes (${{u}}, ${{g}}) are not compared",
                        line.path.display(),
                        line.number
                    ));
                }
                let vol = volumes.last_mut().expect("in_volume");
                vol.grants.push(Grant {
                    letters: letters.to_string(),
                    // `r:` without names means everyone, as in copyparty.
                    names: if names.is_empty() {
                        vec!["*".to_string()]
                    } else {
                        names
                    },
                });
            }
            Section::Flags => {}
            // The line itself is not repeated: it might hold a password.
            _ => {
                return Err(err(
                    "can't tell what this line is (outside a section or volume)".to_string(),
                ));
            }
        }
    }
    if let Some(url) = want_path {
        return Err(ParseError::Line {
            path: root.to_path_buf(),
            line: lines.len(),
            msg: format!("[/{url}] has no folder line"),
        });
    }

    let hashing = !ah_alg.is_empty() && ah_alg.split(',').next().map(str::trim) != Some("none");
    let mut accounts: Vec<Account> = Vec::new();
    for (name, pw) in global_accounts.into_iter().chain(file_accounts) {
        let looks_hashed = pw.starts_with('+') && pw.chars().count() == 33;
        if looks_hashed && !hashing {
            warnings.push(format!(
                "the password of {name} looks like a copyparty hash, but this file doesn't set \
                 ah-alg; if copyparty gets it on its command line, reset that account's \
                 password after the import (chestcraft user passwd {name})"
            ));
        }
        let password = if pw.is_empty() {
            Password::Empty
        } else if looks_hashed && hashing {
            Password::Hashed
        } else {
            Password::Plain(Secret(pw))
        };
        match accounts.iter_mut().find(|a| a.name == name) {
            Some(a) => a.password = password,
            None => accounts.push(Account { name, password }),
        }
    }
    Ok(Parsed {
        accounts,
        groups,
        volumes,
        all_group,
        warnings,
    })
}

/// copyparty permission letters that matter for a comparison with chestcraft.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Access {
    pub read: bool,
    pub write: bool,
    pub moves: bool,
    pub delete: bool,
    /// Download by exact link without listing (`g`, `G`, `h`).
    pub get: bool,
}

impl Access {
    fn add(&mut self, letters: &str) {
        for c in letters.chars() {
            match c {
                'r' => self.read = true,
                'w' => self.write = true,
                'm' => self.moves = true,
                'd' => self.delete = true,
                'g' | 'G' | 'h' => self.get = true,
                // A = r w m d a . (everything)
                'A' => {
                    self.read = true;
                    self.write = true;
                    self.moves = true;
                    self.delete = true;
                }
                _ => {}
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Access::default()
    }

    /// `rwmd` letters, then `g` when it only gets files by link.
    pub fn letters(&self) -> String {
        let mut s = String::new();
        for (on, c) in [
            (self.read, 'r'),
            (self.write, 'w'),
            (self.moves, 'm'),
            (self.delete, 'd'),
        ] {
            if on {
                s.push(c);
            }
        }
        if self.get && !self.read {
            s.push('g');
        }
        s
    }
}

impl Parsed {
    /// What `account` may do in `vol`: its own grants, its groups', the all-accounts
    /// group's and everyone's (`*`).
    pub fn access(&self, account: &str, vol: &Volume) -> Access {
        let in_group: BTreeSet<&str> = self
            .groups
            .iter()
            .filter(|(_, members)| members.iter().any(|m| m == account))
            .map(|(g, _)| g.as_str())
            .collect();
        let mut a = Access::default();
        for g in &vol.grants {
            let applies = g.names.iter().any(|n| {
                n == "*"
                    || n == account
                    || n.strip_prefix('@')
                        .is_some_and(|grp| grp == self.all_group || in_group.contains(grp))
            });
            if applies {
                a.add(&g.letters);
            }
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[global]
  p: 3923
  z, qr
  -e2dsa
  hist: /var/lib/copyparty  # db and thumbnails

# create users:
[accounts]
  kim: correct horse battery
  ole: kurz
  jana: geheim  # comment after two spaces
 create volumes:
[/]         # the web root
  .
  accs:
    A: kim

[/Filme]
  /srv/filme
  accs:
    A: kim
    r: jana

[/Austausch]:
  /srv/austausch
  accs:
    r: *
    A: kim
  flags:
    e2d, nodupe
";

    fn plain(a: &Account) -> &str {
        match &a.password {
            Password::Plain(s) => s.expose(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn accounts_volumes_and_the_stray_entry() {
        let p = parse_str(SAMPLE).unwrap();
        let names: Vec<&str> = p.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["kim", "ole", "jana", "create volumes"]);
        assert_eq!(plain(&p.accounts[0]), "correct horse battery");
        assert_eq!(plain(&p.accounts[1]), "kurz");
        assert_eq!(plain(&p.accounts[2]), "geheim");
        assert_eq!(p.accounts[3].password, Password::Empty);
        let urls: Vec<&str> = p.volumes.iter().map(|v| v.url.as_str()).collect();
        assert_eq!(urls, ["", "Filme", "Austausch"]);
        assert_eq!(p.volumes[1].path, "/srv/filme");
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn access_per_account() {
        let p = parse_str(SAMPLE).unwrap();
        let filme = &p.volumes[1];
        let aus = &p.volumes[2];
        assert_eq!(p.access("kim", filme).letters(), "rwmd");
        assert_eq!(p.access("jana", filme).letters(), "r");
        assert!(p.access("ole", filme).is_empty());
        // `*` reaches every account.
        assert_eq!(p.access("ole", aus).letters(), "r");
        assert_eq!(p.access("kim", aus).letters(), "rwmd");
    }

    #[test]
    fn comments_follow_copyparty() {
        // One space before # keeps it in the value; two spaces start a comment.
        let p = parse_str("[accounts]\n  a: pw #1\n  b: pw  #2\n  # c: commented\n").unwrap();
        assert_eq!(plain(&p.accounts[0]), "pw #1");
        assert_eq!(plain(&p.accounts[1]), "pw");
        assert_eq!(p.accounts.len(), 2);
        // Tabs are spaces, so a tab-separated comment counts too.
        let p = parse_str("[accounts]\n\ta: pw\t\t# note\n").unwrap();
        assert_eq!(plain(&p.accounts[0]), "pw");
    }

    #[test]
    fn password_with_colon_and_later_definition_wins() {
        let p = parse_str("[global]\n  a: kim:first\n[accounts]\n  kim: a:b:c\n").unwrap();
        assert_eq!(p.accounts.len(), 1);
        assert_eq!(plain(&p.accounts[0]), "a:b:c");
    }

    #[test]
    fn hashed_passwords_need_ah_alg() {
        let hash = format!("+{}", "A".repeat(32));
        let text = format!("[global]\n  ah-alg: argon2\n[accounts]\n  kim: {hash}\n  ole: plain\n");
        let p = parse_str(&text).unwrap();
        assert_eq!(p.accounts[0].password, Password::Hashed);
        assert_eq!(plain(&p.accounts[1]), "plain");
        // Without ah-alg it is a password that happens to start with +, with a warning.
        let p = parse_str(&format!("[accounts]\n  kim: {hash}\n")).unwrap();
        assert_eq!(plain(&p.accounts[0]), hash);
        assert_eq!(p.warnings.len(), 1);
        let p = parse_str(&format!(
            "[global]\n  ah-alg: none\n[accounts]\n  kim: {hash}\n"
        ))
        .unwrap();
        assert!(matches!(p.accounts[0].password, Password::Plain(_)));
    }

    #[test]
    fn groups_and_all_accounts() {
        let text = "\
[accounts]
  kim: a
  ole: b
  jana: c
[groups]
  family: kim, ole
[/Fotos]
  /srv/fotos
  accs:
    rw: @family
    g: @acct
";
        let p = parse_str(text).unwrap();
        let v = &p.volumes[0];
        assert_eq!(p.access("ole", v).letters(), "rw");
        assert_eq!(p.access("jana", v).letters(), "g");
    }

    #[test]
    fn refuses_what_copyparty_refuses() {
        for bad in [
            "[accounts]\n  nocolon\n",
            "[global]\n[bogus]\n",
            "[/x]\n  accs:\n    r: kim\n",
            "[/x]\n  /srv/x\n  accs:\n    rz: kim\n",
            "[/x]\n  /srv/x\n  accs:\n    r: kim ole\n",
            "[/x]\n",
        ] {
            assert!(parse_str(bad).is_err(), "{bad:?}");
        }
        assert!(matches!(
            parse_str("-a kim:pw\n/srv\n/\nr\n"),
            Err(ParseError::OldFormat(_))
        ));
    }

    #[test]
    fn split_like_copyparty() {
        assert_eq!(
            split_cfg_line("z, qr, p: 3923, 3924"),
            vec![
                ("z".into(), None),
                ("qr".into(), None),
                ("p".into(), Some("3923, 3924".into()))
            ]
        );
        assert_eq!(split_cfg_line("-e2dsa"), vec![("-e2dsa".into(), None)]);
    }

    #[test]
    fn includes_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::write(
            d.join("main.conf"),
            "% users.conf\n% more\n% missing.conf\n% main.conf\n",
        )
        .unwrap();
        std::fs::write(d.join("users.conf"), "[accounts]\n  kim: a\n").unwrap();
        std::fs::create_dir(d.join("more")).unwrap();
        std::fs::write(d.join("more/2.conf"), "[accounts]\n  ole: c\n").unwrap();
        std::fs::write(d.join("more/1.conf"), "[accounts]\n  jana: b\n").unwrap();
        std::fs::write(d.join("more/.hidden.conf"), "[accounts]\n  x: y\n").unwrap();
        std::fs::write(d.join("more/notes.txt"), "[accounts]\n  x: y\n").unwrap();
        let p = parse_file(&d.join("main.conf")).unwrap();
        let names: Vec<&str> = p.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["kim", "jana", "ole"]);
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
    }

    #[test]
    fn secrets_stay_out_of_debug_output_and_errors() {
        let p = parse_str("[accounts]\n  kim: hunter2-geheim\n").unwrap();
        assert!(!format!("{p:?}").contains("hunter2"));
        // An account line in the wrong place is refused without repeating it.
        let e = parse_str("kim: hunter2-geheim\n[accounts]\n").unwrap_err();
        assert!(!e.to_string().contains("hunter2"), "{e}");
    }
}
