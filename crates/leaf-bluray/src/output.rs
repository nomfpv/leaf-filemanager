//! Where the converted file is written: `name.mkv.leaf-part` beside the target while it is made, renamed to the target only when
//! it is complete, and never over a file that is there: when the name is taken at that moment, the next free "name (2).mkv" is
//! used. The caller gives the no-replace rename of the system (`leaf_sys::fileops::rename_no_replace`).

use std::io;
use std::path::{Path, PathBuf};

pub const PART_EXT: &str = ".leaf-part";

pub fn part_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(PART_EXT);
    PathBuf::from(s)
}

/// "name (n).ext" beside `target`.
pub fn numbered(target: &Path, n: u32) -> PathBuf {
    let stem = target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = match target.extension() {
        Some(e) => format!("{stem} ({n}).{}", e.to_string_lossy()),
        None => format!("{stem} ({n})"),
    };
    target.with_file_name(name)
}

/// The first of `target`, "name (2).ext", ... that does not exist (nor its part file).
pub fn free_name(target: &Path) -> PathBuf {
    let taken = |p: &Path| p.exists() || part_path(p).exists();
    if !taken(target) {
        return target.to_path_buf();
    }
    (2..10_000).map(|n| numbered(target, n)).find(|p| !taken(p)).unwrap_or_else(|| numbered(target, 10_000))
}

/// Opens the part file of `target` for writing. A part file left by an earlier attempt (leaf's own) is started again; nothing
/// else is ever opened for writing.
pub fn create_part(target: &Path) -> io::Result<std::fs::File> {
    let part = part_path(target);
    if part.is_file() {
        std::fs::remove_file(&part)?;
    }
    std::fs::OpenOptions::new().write(true).create_new(true).open(&part)
}

/// Gives the finished part file its name: `target`, or the next free numbered name when `target` exists now. Returns where it is.
pub fn finish(target: &Path, rename_no_replace: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<PathBuf> {
    let part = part_path(target);
    let mut n = 1;
    loop {
        let to = if n == 1 { target.to_path_buf() } else { numbered(target, n) };
        if !to.exists() {
            match rename_no_replace(&part, &to) {
                Ok(()) => return Ok(to),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        n += 1;
        if n > 10_000 {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free name"));
        }
    }
}

/// A no-replace rename with the standard library only: a hard link (which fails when the name exists), then the old name goes.
/// For tests and for systems where the caller has nothing better.
pub fn rename_no_replace_std(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_is_never_written_over_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("leaf-bluray-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("Film.mkv");
        std::fs::write(&target, b"the user's own film").unwrap();
        // the name is taken when the job starts: a free one is chosen
        assert_eq!(free_name(&target), dir.join("Film (2).mkv"));
        // the name became taken while the job ran: the finished file goes beside it
        {
            use std::io::Write;
            let mut f = create_part(&target).unwrap();
            f.write_all(b"converted").unwrap();
        }
        let at = finish(&target, rename_no_replace_std).unwrap();
        assert_eq!(at, dir.join("Film (2).mkv"));
        assert_eq!(std::fs::read(&target).unwrap(), b"the user's own film");
        assert_eq!(std::fs::read(&at).unwrap(), b"converted");
        assert!(!part_path(&target).exists());
        // a part file is never opened over something that is not ours
        assert!(std::fs::OpenOptions::new().write(true).create_new(true).open(&target).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
