//! leaf's trash on a server (B7): a delete on an SFTP or FTP host moves the item into `.leaf-trash/<time>/<path>` in the
//! folder the server logs the user into, by a rename on the server (nothing is copied). Undo renames it back; "Empty
//! server trash" removes what is in that folder, and nothing outside it.
//!
//! - `<path>` is the item's path below the login folder (`docs/a.txt`); an item outside it goes under `_root/<its full path>`.
//! - Only a rename: when the server will not move it (another filesystem behind the folder, a server without rename across
//!   folders, no write right in the login folder), the caller asks "Delete permanently?" ([`TrashError::CannotMove`]).
//! - Links are never followed: the rename moves the link itself. The folders the trash makes on the way are checked to be
//!   folders (not links) before anything goes into them; a `.leaf-trash` that is a link or a file is refused.
//! - The login folder, any folder above it and the server's root are refused (as by the recursive remove); something inside
//!   the trash is not moved again (the caller deletes it permanently after asking).

use leaf_vfs::{Fs, FsError, Kind, VPath};

/// The trash folder's name in the login folder.
pub const TRASH_DIR: &str = ".leaf-trash";

/// Where an item went: the undo record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trashed {
    pub original: VPath,
    pub in_trash: VPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrashError {
    /// The server did not move it into the trash; the text says why in leaf's words (ask "Delete permanently?").
    CannotMove(String),
    Fs(FsError),
}

impl From<FsError> for TrashError {
    fn from(e: FsError) -> TrashError {
        TrashError::Fs(e)
    }
}

/// The trash folder of the server whose login folder is `home`.
pub fn root(home: &VPath) -> VPath {
    home.join(TRASH_DIR)
}

/// The name of one delete's folder in the trash: the local time to the millisecond (`2026-11-21_03-15-22.123`).
pub fn stamp_now() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S%.3f").to_string()
}

/// `/~` and `/~/x` as the real path below `home`.
fn real(v: &VPath, home: &VPath) -> VPath {
    match v.path().strip_prefix("/~") {
        Some("") => home.clone(),
        Some(rest) if rest.starts_with('/') => v.with_path(&format!("{}{rest}", home.path().trim_end_matches('/'))),
        _ => v.clone(),
    }
}

/// `a` is `b` or inside it, comparing paths ignoring case too (a Windows server; refusing more is the safe side).
fn within(a: &VPath, b: &VPath) -> bool {
    let (a, b) = (a.path().to_ascii_lowercase(), b.path().trim_end_matches('/').to_ascii_lowercase());
    b.is_empty() || a == b || a.strip_prefix(&b).is_some_and(|r| r.starts_with('/'))
}

/// Makes the folder `dir` (or takes it when it is there and is a folder, not a link or a file).
fn ensure_dir(fs: &dyn Fs, dir: &VPath) -> Result<(), TrashError> {
    match fs.mkdir(dir) {
        Ok(()) => Ok(()),
        Err(FsError::Exists) => match fs.stat(dir)?.kind {
            Kind::Dir => Ok(()),
            _ => Err(TrashError::CannotMove(format!("{} is not a folder (a link or a file); leaf puts nothing into it", dir.path()))),
        },
        Err(e @ (FsError::Offline(_) | FsError::Cancelled | FsError::Auth(_))) => Err(e.into()),
        Err(e) => Err(TrashError::CannotMove(format!("leaf's trash folder cannot be made here ({e})"))),
    }
}

/// Moves `v` into the trash of its server under the folder `stamp` (one per delete). `home`: the login folder.
pub fn move_to_trash(fs: &dyn Fs, home: &VPath, v: &VPath, stamp: &str) -> Result<VPath, TrashError> {
    let item = real(v, home);
    if item.is_root() || within(home, &item) {
        return Err(FsError::Denied.into());
    }
    let trash = root(home);
    if within(&item, &trash) {
        return Err(TrashError::CannotMove("It is in leaf's trash already".into()));
    }
    if stamp.is_empty() || stamp.contains(['/', '\\']) || stamp.starts_with('.') {
        return Err(FsError::Other("The trash folder's name cannot be used".into()).into());
    }
    let rel: Vec<&str> = if within(&item, home) {
        item.path()[home.path().trim_end_matches('/').len()..].split('/').filter(|s| !s.is_empty()).collect()
    } else {
        std::iter::once("_root").chain(item.path().split('/').filter(|s| !s.is_empty())).collect()
    };
    let Some((name, folders)) = rel.split_last() else { return Err(FsError::Denied.into()) };
    // (what is moved is the item itself: a link is renamed, never followed)
    fs.stat(v)?;
    ensure_dir(fs, &trash)?;
    let mut dir = trash.join(stamp);
    ensure_dir(fs, &dir)?;
    for f in folders {
        dir = dir.join(f);
        ensure_dir(fs, &dir)?;
    }
    let to = dir.join(name);
    match fs.rename(v, &to, false) {
        Ok(()) => Ok(to),
        Err(e @ (FsError::NotFound | FsError::Offline(_) | FsError::Cancelled | FsError::Auth(_))) => {
            remove_empty(fs, &dir, &trash);
            Err(e.into())
        }
        Err(e) => {
            remove_empty(fs, &dir, &trash);
            Err(TrashError::CannotMove(format!("The server did not move it into leaf's trash ({e})")))
        }
    }
}

/// Removes `dir` and the folders above it up to `stop` (not `stop` itself) while they are empty; best effort.
fn remove_empty(fs: &dyn Fs, dir: &VPath, stop: &VPath) {
    let mut d = dir.clone();
    while d.starts_with(stop) && d != *stop {
        if fs.remove(&d, false).is_err() {
            return;
        }
        let Some(up) = d.parent() else { return };
        d = up;
    }
}

/// Undo: renames the item back to where it was (never over something that took its place; the folders it was in are made
/// again when they are gone). `home`: the login folder.
pub fn restore(fs: &dyn Fs, home: &VPath, t: &Trashed) -> Result<(), FsError> {
    let trash = root(home);
    if !within(&t.in_trash, &trash) {
        return Err(FsError::Other("This item is not in leaf's trash".into()));
    }
    match fs.stat(&t.in_trash) {
        Err(FsError::NotFound) => return Err(FsError::Other("It is no longer in leaf's trash (emptied?)".into())),
        r => {
            r?;
        }
    }
    // the folders it was in, when they went meanwhile
    let mut missing = Vec::new();
    let mut up = t.original.parent();
    while let Some(d) = up {
        if d.is_root() || !matches!(fs.stat(&d), Err(FsError::NotFound)) {
            break;
        }
        up = d.parent();
        missing.push(d);
    }
    for d in missing.iter().rev() {
        match fs.mkdir(d) {
            Ok(()) | Err(FsError::Exists) => {}
            Err(e) => return Err(e),
        }
    }
    match fs.rename(&t.in_trash, &t.original, false) {
        Ok(()) => {}
        Err(FsError::Exists) => {
            return Err(FsError::Other(format!("Something has its name in {} now; it stays in leaf's trash at {}", t.original.parent().map(|p| p.path().to_string()).unwrap_or_default(), t.in_trash.path())));
        }
        Err(e) => return Err(e),
    }
    if let Some(dir) = t.in_trash.parent() {
        remove_empty(fs, &dir, &trash);
    }
    Ok(())
}

/// What the trash holds: (files and links, bytes). Folders are walked, links never followed. `home`: the login folder.
pub fn usage(fs: &dyn Fs, home: &VPath, cancelled: &dyn Fn() -> bool) -> Result<(u64, u64), FsError> {
    let trash = root(home);
    match fs.stat(&trash) {
        Err(FsError::NotFound) => return Ok((0, 0)),
        Ok(m) if m.kind != Kind::Dir => return Err(FsError::Other(format!("{} is not a folder", trash.path()))),
        r => {
            r?;
        }
    }
    let (mut files, mut bytes) = (0u64, 0u64);
    let mut todo = vec![(trash, 0usize)];
    while let Some((dir, depth)) = todo.pop() {
        if cancelled() {
            return Err(FsError::Cancelled);
        }
        if depth >= leaf_vfs::copy::MAX_DEPTH {
            return Err(leaf_vfs::copy::too_deep());
        }
        let mut children = Vec::new();
        fs.list(&dir, 1024, cancelled, &mut |b| children.extend(b))?;
        for e in children {
            let Ok(child) = leaf_vfs::names::vchild(&dir, &e.name) else { continue };
            if e.is_dir() && !e.link {
                todo.push((child, depth + 1));
            } else {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(e.size);
            }
        }
    }
    Ok((files, bytes))
}

/// "Empty server trash": removes everything inside `.leaf-trash` of the login folder `home` (each entry by the backend's
/// recursive remove, which unlinks links and never goes through them), then the trash folder itself when it is empty.
/// Nothing outside the trash folder is touched.
pub fn empty(fs: &dyn Fs, home: &VPath, cancelled: &dyn Fn() -> bool) -> Result<(), FsError> {
    let trash = root(home);
    match fs.stat(&trash) {
        Err(FsError::NotFound) => return Ok(()),
        // a link named .leaf-trash: not entered, not emptied
        Ok(m) if m.kind != Kind::Dir => return Err(FsError::Other(format!("{} is not a folder; leaf does not empty it", trash.path()))),
        r => {
            r?;
        }
    }
    let mut children = Vec::new();
    fs.list(&trash, 1024, cancelled, &mut |b| children.extend(b))?;
    for e in children {
        if cancelled() {
            return Err(FsError::Cancelled);
        }
        let Ok(child) = leaf_vfs::names::vchild(&trash, &e.name) else { continue };
        if !within(&child, &trash) || child == trash {
            continue;
        }
        match fs.remove(&child, true) {
            Ok(()) | Err(FsError::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    let _ = fs.remove(&trash, false);
    Ok(())
}
