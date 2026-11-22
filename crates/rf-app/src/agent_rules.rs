//! Agent rules editor (plan F8), behind the agent layer switch: shows the permission rules Claude Code reads for a project (allow /
//! ask / deny, from the user's, the project's and the local settings file) and adds or removes the deny / allow rules of a folder or
//! file ("Protect from agents", "Allow agents here"). It writes another tool's config, so it is careful: the text of the file is
//! changed in the one array only (every other byte stays), the write is atomic, and a file that is not plain JSON or has a shape
//! this does not expect is refused, never rewritten.
//!
//! Rule syntax (ASSUMED, from Claude Code's documented permission rules; nothing in this repository confirms it): `permissions`
//! holds `allow`, `ask` and `deny` arrays of strings; a rule is `Tool(specifier)`; `Read(...)` and `Edit(...)` take gitignore-style
//! patterns; a pattern starting with a single `/` is relative to the project root, `dir/**` is everything below `dir`. Paths with
//! pattern characters or parentheses are refused rather than escaped (the escaping is not confirmed).

use std::path::{Path, PathBuf};

use gpui_kit::*;

use crate::prompt::{self, Button, Check, Kind as BtnKind};
use crate::t;

// ---------------------------------------------------------------- the text of a settings file (no rewriting of the rest)

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Allow,
    Ask,
    Deny,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Allow, Kind::Ask, Kind::Deny];
    fn key(self) -> &'static str {
        match self {
            Kind::Allow => "allow",
            Kind::Ask => "ask",
            Kind::Deny => "deny",
        }
    }
}

type R<T> = Result<T, String>;

fn bad(what: &str) -> String {
    format!("{what}")
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// The end (exclusive) of the string that starts at `i` (a quote).
fn string_end(b: &[u8], i: usize) -> R<usize> {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return Ok(j + 1),
            _ => j += 1,
        }
    }
    Err(bad("a string is not closed"))
}

/// The end (exclusive) of the value that starts at `i`.
fn value_end(b: &[u8], i: usize) -> R<usize> {
    match b.get(i) {
        Some(b'"') => string_end(b, i),
        Some(b'{') | Some(b'[') => {
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                match b[j] {
                    b'"' => {
                        j = string_end(b, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            Err(bad("a bracket is not closed"))
        }
        Some(_) => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']') && !b[j].is_ascii_whitespace() {
                j += 1;
            }
            Ok(j)
        }
        None => Err(bad("the text ends too early")),
    }
}

struct Member {
    key: String,
    key_start: usize,
    val: (usize, usize),
}

/// The members of the object whose `{` is at `open`, and where its `}` is.
fn members(text: &str, open: usize) -> R<(Vec<Member>, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = skip_ws(b, open + 1);
    loop {
        match b.get(i) {
            Some(b'}') => return Ok((out, i)),
            Some(b'"') => {
                let ke = string_end(b, i)?;
                let key: String = serde_json::from_str(&text[i..ke]).map_err(|_| bad("a key is not readable"))?;
                let c = skip_ws(b, ke);
                if b.get(c) != Some(&b':') {
                    return Err(bad("a colon is missing"));
                }
                let vs = skip_ws(b, c + 1);
                let ve = value_end(b, vs)?;
                out.push(Member { key, key_start: i, val: (vs, ve) });
                i = skip_ws(b, ve);
                match b.get(i) {
                    Some(b',') => i = skip_ws(b, i + 1),
                    Some(b'}') => {}
                    _ => return Err(bad("a comma is missing")),
                }
            }
            _ => return Err(bad("an object is not readable")),
        }
    }
}

/// The string elements of the array whose `[` is at `open` (their text spans), and where its `]` is.
fn elements(text: &str, open: usize) -> R<(Vec<(usize, usize)>, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = skip_ws(b, open + 1);
    loop {
        match b.get(i) {
            Some(b']') => return Ok((out, i)),
            Some(b'"') => {
                let e = string_end(b, i)?;
                out.push((i, e));
                i = skip_ws(b, e);
                match b.get(i) {
                    Some(b',') => i = skip_ws(b, i + 1),
                    Some(b']') => {}
                    _ => return Err(bad("a comma is missing")),
                }
            }
            _ => return Err(bad("the list holds something that is not a text")),
        }
    }
}

/// The white space at the start of the line `pos` is on.
fn line_indent(text: &str, pos: usize) -> String {
    let start = text[..pos].rfind('\n').map_or(0, |n| n + 1);
    text[start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// `text` with `member` (`"key": value`) added as the last member of the object at `open`.
fn add_member(text: &str, open: usize, member: &str) -> R<String> {
    let (ms, close) = members(text, open)?;
    let outer = line_indent(text, open);
    let inner = ms.first().map_or_else(|| format!("{outer}  "), |m| line_indent(text, m.key_start));
    let mut s = text.to_string();
    match ms.last() {
        Some(l) => s.insert_str(l.val.1, &format!(",\n{inner}{member}")),
        None => s.replace_range(open + 1..close, &format!("\n{inner}{member}\n{outer}")),
    }
    Ok(s)
}

/// Where the array `permissions.<kind>` is: `Ok(None)` when it is not there; a wrong shape is an error.
fn locate(text: &str, kind: Kind) -> R<Option<usize>> {
    let b = text.as_bytes();
    let top = skip_ws(b, 0);
    if b.get(top) != Some(&b'{') {
        return Err(bad("the file is not a JSON object"));
    }
    let (ms, _) = members(text, top)?;
    let Some(p) = ms.iter().find(|m| m.key == "permissions") else { return Ok(None) };
    if b[p.val.0] != b'{' {
        return Err(bad("\"permissions\" is not an object"));
    }
    let (pm, _) = members(text, p.val.0)?;
    let Some(a) = pm.iter().find(|m| m.key == kind.key()) else { return Ok(None) };
    if b[a.val.0] != b'[' {
        return Err(bad(&format!("\"{}\" is not a list", kind.key())));
    }
    Ok(Some(a.val.0))
}

fn check_valid(text: &str) -> R<()> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| bad("the file is not valid JSON (comments and trailing commas are not understood)"))?;
    if !v.is_object() {
        return Err(bad("the file is not a JSON object"));
    }
    Ok(())
}

/// The rules of `kind` in `text` (a file that is missing is `"{}"`).
pub fn rules_in(text: &str, kind: Kind) -> R<Vec<String>> {
    check_valid(text)?;
    let Some(open) = locate(text, kind)? else { return Ok(Vec::new()) };
    let (els, _) = elements(text, open)?;
    els.iter().map(|&(s, e)| serde_json::from_str::<String>(&text[s..e]).map_err(|_| bad("a rule is not readable"))).collect()
}

/// `text` with the rules added to `permissions.<kind>` (those already there are not added again), and every other byte as it was.
/// `Ok(None)`: nothing to add. An error: the file is not touched by anyone.
pub fn add_rules(text: &str, kind: Kind, rules: &[String]) -> R<Option<String>> {
    let text = if text.trim().is_empty() { "{}\n" } else { text };
    let have = rules_in(text, kind)?;
    let mut new: Vec<&String> = Vec::new();
    for r in rules {
        if !have.contains(r) && !new.contains(&r) {
            new.push(r);
        }
    }
    if new.is_empty() {
        return Ok(None);
    }
    let b = text.as_bytes();
    let top = skip_ws(b, 0);
    let out = match locate(text, kind)? {
        Some(open) => {
            let (els, close) = elements(text, open)?;
            let multi = text[open..close].contains('\n');
            let mut s = text.to_string();
            if multi {
                let inner = els.first().map_or_else(|| format!("{}  ", line_indent(text, open)), |&(st, _)| line_indent(text, st));
                let outer = line_indent(text, open);
                let add: String = new.iter().map(|r| quote(r)).collect::<Vec<_>>().join(&format!(",\n{inner}"));
                match els.last() {
                    Some(&(_, le)) => s.insert_str(le, &format!(",\n{inner}{add}")),
                    None => s.replace_range(open + 1..close, &format!("\n{inner}{add}\n{outer}")),
                }
            } else {
                let add: String = new.iter().map(|r| quote(r)).collect::<Vec<_>>().join(", ");
                match els.last() {
                    Some(&(_, le)) => s.insert_str(le, &format!(", {add}")),
                    None => s.replace_range(open + 1..close, &add),
                }
            }
            s
        }
        None => {
            let (ms, _) = members(text, top)?;
            let arr = format!("{}: [{}]", quote(kind.key()), new.iter().map(|r| quote(r)).collect::<Vec<_>>().join(", "));
            match ms.iter().find(|m| m.key == "permissions") {
                Some(p) => add_member(text, p.val.0, &arr)?,
                None => add_member(text, top, &format!("\"permissions\": {{\n{}    {arr}\n{}  }}", line_indent(text, top), line_indent(text, top)))?,
            }
        }
    };
    check_valid(&out)?;
    // (what the file now holds, besides the new rules, is what it held)
    let mut want = rules_in(text, kind)?;
    want.extend(new.iter().map(|r| (*r).clone()));
    if rules_in(&out, kind)? != want {
        return Err(bad("the result did not read back as expected"));
    }
    Ok(Some(out))
}

/// `text` with the rules removed from `permissions.<kind>` (exact match). `Ok(None)`: none of them is there.
pub fn remove_rules(text: &str, kind: Kind, rules: &[String]) -> R<Option<String>> {
    check_valid(text)?;
    let before = rules_in(text, kind)?;
    let mut s = text.to_string();
    let mut changed = false;
    for r in rules {
        let Some(open) = locate(&s, kind)? else { break };
        let (els, close) = elements(&s, open)?;
        let Some(k) = els.iter().position(|&(a, e)| serde_json::from_str::<String>(&s[a..e]).is_ok_and(|v| v == *r)) else { continue };
        let (a, e) = els[k];
        if els.len() == 1 {
            s.replace_range(open + 1..close, "");
        } else if k + 1 < els.len() {
            s.replace_range(a..els[k + 1].0, "");
        } else {
            s.replace_range(els[k - 1].1..e, "");
        }
        changed = true;
    }
    if !changed {
        return Ok(None);
    }
    check_valid(&s)?;
    let want: Vec<String> = before.into_iter().filter(|b| !rules.contains(b)).collect();
    if rules_in(&s, kind)? != want {
        return Err(bad("the result did not read back as expected"));
    }
    Ok(Some(s))
}

/// Writes `text` to `path`: a temporary file beside it, then a rename, so a crash leaves the old file or the new one.
pub fn write_atomic(path: &Path, text: &str) -> R<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| bad("the file has no folder"))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.leaf-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let r = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        if let Ok(m) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, m.permissions())?;
        }
        std::fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map_err(|e| e.to_string())
}

/// Reads `path` (a missing file is an empty object), changes it with `f` and writes it; `Ok(false)`: nothing needed changing.
fn edit_file(path: &Path, f: impl FnOnce(&str) -> R<Option<String>>) -> R<bool> {
    let text = match std::fs::read(path) {
        Ok(b) => String::from_utf8(b).map_err(|_| bad("the file is not UTF-8 text"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    match f(&text)? {
        Some(new) => write_atomic(path, &new).map(|_| true),
        None => Ok(false),
    }
}

// ---------------------------------------------------------------- the project, the rules of a path

/// The project of `path`: its Git root, else the nearest folder with a `.claude` folder.
pub fn project_root(path: &Path) -> Option<PathBuf> {
    let from = if path.is_dir() { path } else { path.parent()? };
    leaf_dev::gitignore::repo_root(from).or_else(|| from.ancestors().find(|a| a.join(".claude").is_dir()).map(Path::to_path_buf))
}

/// The two rules (`Read` and `Edit`) for `path` inside `root`; an error when the path has characters whose escaping is not known.
pub fn path_rules(root: &Path, path: &Path) -> R<Vec<String>> {
    let rel = path.strip_prefix(root).map_err(|_| bad("the item is not inside the project"))?;
    let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
    if rel.chars().any(|c| c.is_control() || "()*?[]!#\\\"".contains(c)) {
        return Err(bad("the name has characters that are special in a rule (one of ()*?[]!#\\\"); edit the rule by hand"));
    }
    let pat = match (rel.is_empty(), path.is_dir()) {
        (true, _) => "/**".to_string(),
        (false, true) => format!("/{rel}/**"),
        (false, false) => format!("/{rel}"),
    };
    Ok(vec![format!("Read({pat})"), format!("Edit({pat})")])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    User,
    Project,
    Local,
}

impl Where {
    fn label(self) -> &'static str {
        match self {
            Where::User => t!("user"),
            Where::Project => t!("project"),
            Where::Local => t!("local"),
        }
    }
}

fn file_of(root: &Path, w: Where) -> Option<PathBuf> {
    match w {
        Where::Project => Some(root.join(".claude").join("settings.json")),
        Where::Local => Some(root.join(".claude").join("settings.local.json")),
        Where::User => std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| dirs::home_dir().map(|h| h.join(".claude"))).map(|d| d.join("settings.json")),
    }
}

// ---------------------------------------------------------------- the dialogs

fn window_of(cx: &mut App) -> Option<AnyWindowHandle> {
    cx.active_window().or_else(|| cx.windows().first().copied())
}

/// "Protect from agents" (`allow = false`) and "Allow agents here" (`allow = true`) for the selected items: shows the exact lines and
/// the file, asks, then writes.
pub fn change(paths: Vec<PathBuf>, allow: bool, window: &mut Window, cx: &mut App) {
    let title = if allow { t!("Allow agents here") } else { t!("Protect from agents") };
    let Some(first) = paths.first() else { return };
    let Some(root) = project_root(first) else {
        return prompt::notice(title, t!("This is not inside a project (a Git repository or a folder with .claude).").to_string(), window, cx);
    };
    let mut rules = Vec::new();
    for p in &paths {
        match path_rules(&root, p) {
            Ok(r) => rules.extend(r),
            Err(e) => return prompt::notice(title, format!("{}\n{e}", t!("Nothing was changed:")), window, cx),
        }
    }
    let kind = if allow { Kind::Allow } else { Kind::Deny };
    let local = file_of(&root, Where::Local).unwrap_or_default();
    let shared = file_of(&root, Where::Project).unwrap_or_default();
    let show = |f: &Path| format!("{}\n{}", f.display(), if f.exists() { t!("(exists, only the \"{}\" list is changed)").replace("{}", kind.key()) } else { t!("(does not exist yet, made when you confirm)").to_string() });
    let message = format!(
        "{}\n\n{}\n\n{}\n{}\n\n{}",
        if allow { t!("Adds these rules to Claude Code's allow list:") } else { t!("Adds these rules to Claude Code's deny list, so it may not read or change this:") },
        rules.iter().map(|r| format!("    {r}")).collect::<Vec<_>>().join("\n"),
        t!("Written to:"),
        show(&local),
        t!("The shared file is .claude/settings.json (it is committed with the project); the local one is not."),
    );
    let button = |label: &'static str, kind_btn: BtnKind, file: PathBuf| {
        let rules = rules.clone();
        Button::new(label, kind_btn, move |_, _, cx| {
            let done = edit_file(&file, |text| add_rules(text, kind, &rules)).map_err(|e| format!("{} {e}", t!("Not written:")))?;
            if !done {
                log::info!("agent rules: nothing to add to {}", file.display());
            }
            cx.refresh_windows();
            Ok(())
        })
    };
    prompt::show(
        title,
        Some(message),
        None,
        vec![button(t!("Write to settings.local.json"), BtnKind::Primary, local.clone()), button(t!("Write to settings.json (shared)"), BtnKind::Normal, shared), Button::cancel()],
        window,
        cx,
    );
}

/// The read view for the project of `dir`: the rules by list and file, each rule of the project and local file with a box to remove it.
pub fn show_rules(dir: PathBuf, window: &mut Window, cx: &mut App) {
    let title = t!("Agent rules");
    let Some(root) = project_root(&dir) else {
        return prompt::notice(title, t!("This is not inside a project (a Git repository or a folder with .claude).").to_string(), window, cx);
    };
    let mut message = format!("{} {}\n", t!("Project:"), root.display());
    let mut checks: Vec<Check> = Vec::new();
    // (what each ticked row removes, with the file and list it is in)
    let mut rows: Vec<(std::rc::Rc<std::cell::Cell<bool>>, PathBuf, Kind, String)> = Vec::new();
    for kind in Kind::ALL {
        message.push_str(&format!("\n{}\n", kind.key().to_uppercase()));
        let mut any = false;
        for w in [Where::User, Where::Project, Where::Local] {
            let Some(file) = file_of(&root, w) else { continue };
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            // (a file that is not understood is listed once, under its first list, and left alone)
            match rules_in(&text, kind) {
                Ok(rs) => {
                    for r in rs {
                        any = true;
                        let on = std::rc::Rc::new(std::cell::Cell::new(false));
                        checks.push(Check { label: format!("{}  {r}", kind.key()).into(), note: w.label().into(), on: on.clone(), enabled: w != Where::User });
                        rows.push((on, file.clone(), kind, r));
                    }
                }
                Err(e) if kind == Kind::Allow => message.push_str(&format!("    {} ({}): {}\n", file.display(), w.label(), e)),
                Err(_) => {}
            }
        }
        if !any {
            message.push_str(&format!("    {}\n", t!("none")));
        }
    }
    message.push_str(&format!("\n{}", t!("Tick a rule of the project or local file to remove it (the user file is only shown).")));
    // (nothing to tick: no Remove button to press)
    let has_rules = !rows.is_empty();
    let mut buttons = vec![]; if has_rules { buttons.push(
        Button::new(t!("Remove ticked…"), BtnKind::Danger, move |_, _, cx| {
            let picked: Vec<(PathBuf, Kind, String)> = rows.iter().filter(|r| r.0.get()).map(|r| (r.1.clone(), r.2, r.3.clone())).collect();
            if !picked.is_empty() {
                cx.defer(move |cx| {
                    if let Some(w) = window_of(cx) {
                        let _ = w.update(cx, |_, window, cx| confirm_remove(picked, window, cx));
                    }
                });
            }
            Ok(())
        })); }
    buttons.push(Button::new(t!("Close"), BtnKind::Normal, |_, _, _| Ok(())));
    prompt::show_with_checks(title, Some(message), None, Vec::new(), checks, buttons, window, cx);
}

/// The preview and the question for removing rules (exact match; leaf marks nothing in the file).
fn confirm_remove(picked: Vec<(PathBuf, Kind, String)>, window: &mut Window, cx: &mut App) {
    let message = format!(
        "{}\n\n{}\n\n{}",
        t!("Removes these rules (each only when it is still in the file exactly as shown):"),
        picked.iter().map(|(f, k, r)| format!("    {}  {r}\n        {}", k.key(), f.display())).collect::<Vec<_>>().join("\n"),
        t!("Nothing else in the files is changed."),
    );
    prompt::show(
        t!("Remove rules"),
        Some(message),
        None,
        vec![
            Button::new(t!("Remove"), BtnKind::Danger, move |_, _, cx| {
                let mut files: Vec<PathBuf> = picked.iter().map(|p| p.0.clone()).collect();
                files.dedup();
                for f in files {
                    for kind in Kind::ALL {
                        let rs: Vec<String> = picked.iter().filter(|p| p.0 == f && p.1 == kind).map(|p| p.2.clone()).collect();
                        if rs.is_empty() {
                            continue;
                        }
                        edit_file(&f, |text| remove_rules(text, kind, &rs)).map_err(|e| format!("{} {}: {e}", t!("Not written:"), f.display()))?;
                    }
                }
                cx.refresh_windows();
                Ok(())
            }),
            Button::cancel(),
        ],
        window,
        cx,
    );
}

#[cfg(test)]
mod tests {
    use super::{Kind, add_rules, edit_file, path_rules, remove_rules, rules_in};

    /// Adding and removing keep every other byte; an invalid or unexpected file is refused; a rule is never added twice.
    #[test]
    fn rules_are_added_and_removed_in_place_and_odd_files_are_refused() {
        in_text();
        odd_files();
        on_disk();
    }

    fn r(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    fn in_text() {
        let deny = Kind::Deny;
        // an existing file: other keys, their order and the formatting stay, the new rule lands at the end of the one list
        let text = "{\n  \"model\": \"x\",\n  \"permissions\": {\n    \"allow\": [\"Bash(ls)\"],\n    \"deny\": [\n      \"Read(/a)\"\n    ]\n  },\n  \"env\": { \"K\": 1 }\n}\n";
        let added = add_rules(text, deny, &r("Edit(/a)")).unwrap().unwrap();
        assert_eq!(added, text.replace("\"Read(/a)\"\n", "\"Read(/a)\",\n      \"Edit(/a)\"\n"));
        assert_eq!(rules_in(&added, deny).unwrap(), ["Read(/a)", "Edit(/a)"]);
        // never twice
        assert!(add_rules(&added, deny, &r("Edit(/a)")).unwrap().is_none());
        assert_eq!(add_rules(&added, deny, &["Read(/b)".into(), "Read(/b)".into()]).unwrap().map(|t| rules_in(&t, deny).unwrap().len()), Some(3));
        // removing it again gives the first text back
        assert_eq!(remove_rules(&added, deny, &r("Edit(/a)")).unwrap().unwrap(), text);
        assert!(remove_rules(&added, deny, &r("Edit(/zzz)")).unwrap().is_none());
        // the other lists and a missing list or missing permissions
        let t2 = add_rules(text, Kind::Ask, &r("Bash(rm:*)")).unwrap().unwrap();
        assert!(t2.contains("\"ask\": [\"Bash(rm:*)\"]") && t2.contains("\"env\": { \"K\": 1 }") && rules_in(&t2, Kind::Allow).unwrap() == ["Bash(ls)"]);
        let t3 = add_rules("{\n  \"model\": \"x\"\n}\n", deny, &r("Read(/s/**)")).unwrap().unwrap();
        assert!(t3.starts_with("{\n  \"model\": \"x\",\n  \"permissions\"") && rules_in(&t3, deny).unwrap() == ["Read(/s/**)"]);
        // a file that is not there or empty
        let t4 = add_rules("", Kind::Allow, &r("Read(/s)")).unwrap().unwrap();
        assert_eq!(rules_in(&t4, Kind::Allow).unwrap(), ["Read(/s)"]);
        // a single rule removed leaves an empty list; an inline list stays inline
        assert_eq!(remove_rules("{\"permissions\":{\"deny\":[\"a\"]}}", deny, &r("a")).unwrap().unwrap(), "{\"permissions\":{\"deny\":[]}}");
        assert_eq!(add_rules("{\"permissions\":{\"deny\":[\"a\"]}}", deny, &r("b")).unwrap().unwrap(), "{\"permissions\":{\"deny\":[\"a\", \"b\"]}}");
    }

    fn odd_files() {
        let deny = Kind::Deny;
        // refused, never rewritten: invalid JSON, comments, not an object, wrong shapes
        for bad_text in ["{ \"a\": ", "// c\n{}", "[]", "{\"permissions\": 3}", "{\"permissions\": {\"deny\": \"x\"}}", "{\"permissions\": {\"deny\": [1]}}"] {
            assert!(add_rules(bad_text, deny, &r("Read(/a)")).is_err(), "{bad_text}");
            assert!(remove_rules(bad_text, deny, &r("Read(/a)")).is_err(), "{bad_text}");
        }
    }

    fn on_disk() {
        let deny = Kind::Deny;
        // the file on disk is untouched by a refused change
        let dir = std::env::temp_dir().join(format!("leaf-agent-rules-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("settings.local.json");
        std::fs::write(&f, "{ not json").unwrap();
        assert!(edit_file(&f, |t| add_rules(t, deny, &r("Read(/a)"))).is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{ not json");
        // made on a confirmed write only, and read back
        let g = dir.join(".claude").join("settings.local.json");
        assert!(edit_file(&g, |t| add_rules(t, deny, &r("Read(/a)"))).unwrap());
        assert_eq!(rules_in(&std::fs::read_to_string(&g).unwrap(), deny).unwrap(), ["Read(/a)"]);
        // the rules of a path
        std::fs::create_dir_all(dir.join("secrets")).unwrap();
        assert_eq!(path_rules(&dir, &dir.join("secrets")).unwrap(), ["Read(/secrets/**)", "Edit(/secrets/**)"]);
        assert!(path_rules(&dir, &dir.join("a(b).txt")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
