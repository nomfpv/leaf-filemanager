//! A crash-safe, append-only record file: one record per line, `crc32<TAB>fields…`, where the
//! fields are separated by tabs (Windows paths can't contain tabs or line breaks; on Linux a path
//! that could is written through [`bytes_field`]). A line whose checksum doesn't match, and
//! everything after it, is ignored on reading, so a record torn by a crash or a power cut is
//! dropped instead of trusted.
//!
//! Format version 1 (the only one so far): the first record is the job (`B …`); readers skip
//! record types they don't know, so an engine may add its own (leaf-unix adds per-file progress)
//! without another engine misreading the journal. A later, incompatible version would announce
//! itself with a `V <n>` record right after `B`.
//!
//! Durability: [`Writer::sync`] flushes to the disk (`FlushFileBuffers` / `fdatasync`). Callers
//! sync what must not be lost (the plan of a copy, before the first byte is copied) and batch the
//! rest.
//!
//! A running job holds its journal so that another process can tell it (open) from an
//! interrupted one (the writer died): [`in_use`]. On Windows the file is opened without write
//! sharing; on Linux the writer holds an exclusive `flock`, which the kernel drops when the
//! process dies.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::Path;

/// The format these functions read and write (see the module text).
pub const FORMAT_VERSION: u32 = 1;

#[cfg(windows)]
const FILE_SHARE_READ: u32 = 1;

pub struct Writer {
    out: BufWriter<File>,
}

fn line(fields: &[&str]) -> String {
    let payload = fields.join("\t");
    format!("{:08x}\t{payload}\n", crc32fast::hash(payload.as_bytes()))
}

#[cfg(windows)]
fn open(o: &mut OpenOptions, path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    o.share_mode(FILE_SHARE_READ).open(path)
}

#[cfg(unix)]
fn lock(f: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: flock on a descriptor we hold.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

#[cfg(unix)]
fn open(o: &mut OpenOptions, path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = o.mode(0o600).custom_flags(libc::O_CLOEXEC).open(path)?;
    lock(&f)?;
    Ok(f)
}

impl Writer {
    /// A new journal file (fails if it exists).
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let f = open(OpenOptions::new().write(true).create_new(true), path)?;
        Ok(Self { out: BufWriter::with_capacity(64 << 10, f) })
    }

    /// An existing journal, appended to (resuming a job).
    pub fn append(path: &Path) -> std::io::Result<Self> {
        let f = open(OpenOptions::new().append(true), path)?;
        Ok(Self { out: BufWriter::with_capacity(64 << 10, f) })
    }

    pub fn record(&mut self, fields: &[&str]) -> std::io::Result<()> {
        self.out.write_all(line(fields).as_bytes())
    }

    /// Everything written so far is handed to the system (it survives the process being killed, not a power cut).
    pub fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }

    /// Everything written so far is on the disk when this returns.
    pub fn sync(&mut self) -> std::io::Result<()> {
        self.out.flush()?;
        self.out.get_ref().sync_data()
    }
}

/// The intact records of journal bytes, in order: reading stops at the first line that is torn
/// or fails its checksum.
pub fn parse(bytes: &[u8]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
        let (ln, tail) = rest.split_at(nl);
        rest = &tail[1..];
        let Ok(s) = std::str::from_utf8(ln) else { break };
        let Some((crc, payload)) = s.split_once('\t') else { break };
        if u32::from_str_radix(crc, 16).ok() != Some(crc32fast::hash(payload.as_bytes())) {
            break;
        }
        out.push(payload.split('\t').map(str::to_string).collect());
    }
    out
}

/// The intact records of a journal, in order (see [`parse`]).
pub fn read(path: &Path) -> std::io::Result<Vec<Vec<String>>> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    Ok(parse(&bytes))
}

/// Replaces the journal with exactly `records` (dropping a corrupt tail before more is appended)
/// and returns a writer that appends to it. The new file is synced before it takes the old one's
/// place, so a crash here leaves either the old journal or the new one, never neither. On Linux
/// the returned writer is the new file's own, locked from its creation, so no other process can
/// take the job between the rename and the next record (the caller holds the old file, see
/// [`hold`], until this returns).
pub fn rewrite(path: &Path, records: &[Vec<String>]) -> std::io::Result<Writer> {
    let tmp = path.with_extension("jrn-new");
    let _ = std::fs::remove_file(&tmp);
    let mut w = Writer::create(&tmp)?;
    for r in records {
        w.record(&r.iter().map(String::as_str).collect::<Vec<_>>())?;
    }
    w.sync()?;
    #[cfg(unix)]
    {
        std::fs::rename(&tmp, path)?;
        Ok(w)
    }
    #[cfg(not(unix))]
    {
        drop(w);
        std::fs::rename(&tmp, path)?;
        Writer::append(path)
    }
}

/// Takes an interrupted journal for carrying it on, before it is read: the lock a writer holds,
/// so a second resume (another window, another process) is refused (`WouldBlock`) instead of
/// running the same job twice. Kept until [`rewrite`] returned the new writer. Fails as well when
/// the name no longer is the file locked (rewritten by another resume meanwhile).
#[cfg(unix)]
pub fn hold(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let f = OpenOptions::new().read(true).custom_flags(libc::O_CLOEXEC).open(path)?;
    lock(&f)?;
    let (a, b) = (f.metadata()?, std::fs::metadata(path)?);
    if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
        return Err(std::io::ErrorKind::WouldBlock.into());
    }
    Ok(f)
}

/// True while a writer has the journal open (its job is running, maybe in another process).
#[cfg(windows)]
pub fn in_use(path: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new().append(true).share_mode(FILE_SHARE_READ).open(path).is_err() && path.exists()
}

/// True while a writer has the journal open (its job is running, maybe in another process).
#[cfg(unix)]
pub fn in_use(path: &Path) -> bool {
    // (the probe's own lock goes with its descriptor at the end of this function)
    match File::open(path) {
        Ok(f) => lock(&f).is_err(),
        Err(_) => false,
    }
}

/// Marks a field written by [`bytes_field`] as hex (a byte no journal field starts with otherwise).
const HEX_MARK: char = '\u{1}';

/// A journal field for a name or path given as bytes (Linux names may hold tabs, line breaks or
/// bytes that are not UTF-8): the text itself when it is safe, else a mark and the bytes in hex.
/// Text fields from Windows are never touched by this (their paths are always safe).
pub fn bytes_field(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) if !s.contains(['\t', '\n', '\r']) && !s.starts_with(HEX_MARK) => s.to_string(),
        _ => {
            let mut out = String::with_capacity(1 + b.len() * 2);
            out.push(HEX_MARK);
            for x in b {
                out.push_str(&format!("{x:02x}"));
            }
            out
        }
    }
}

/// The bytes of a field written by [`bytes_field`] (`None` for broken hex).
pub fn field_bytes(f: &str) -> Option<Vec<u8>> {
    let Some(hex) = f.strip_prefix(HEX_MARK) else { return Some(f.as_bytes().to_vec()) };
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect()
}

/// A path as a journal field (any bytes, see [`bytes_field`]).
#[cfg(unix)]
pub fn path_field(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    bytes_field(p.as_os_str().as_bytes())
}

/// The path of a field written by [`path_field`].
#[cfg(unix)]
pub fn field_path(f: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    field_bytes(f).map(|b| std::ffi::OsString::from_vec(b).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rf-journal-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        d
    }

    #[test]
    fn torn_and_corrupt_tails_are_dropped() {
        let p = tmp("torn");
        {
            let mut w = Writer::create(&p).unwrap();
            w.record(&["B", "c", r"C:\a b\ç.txt"]).unwrap();
            w.record(&["F", "0"]).unwrap();
            w.sync().unwrap();
            assert!(in_use(&p), "an open writer marks the job as running");
        }
        assert!(!in_use(&p));
        // a torn record (no newline) and garbage after a crash
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(line(&["F", "1"]).as_bytes()).unwrap();
        f.write_all(b"deadbeef\tF\t2\n").unwrap();
        f.write_all(line(&["F", "3"]).as_bytes()).unwrap();
        f.write_all(b"0123").unwrap();
        drop(f);
        let r = read(&p).unwrap();
        assert_eq!(r, vec![vec!["B".to_string(), "c".into(), r"C:\a b\ç.txt".into()], vec!["F".into(), "0".into()], vec!["F".into(), "1".into()]]);
        // appending after a reopen keeps the earlier records
        let mut w = Writer::append(&p).unwrap();
        w.record(&["F", "9"]).unwrap();
        w.sync().unwrap();
        drop(w);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn byte_fields_round_trip() {
        for b in [&b"plain/name.txt"[..], b"tab\there", b"line\nbreak", b"\xff\xfe not utf-8", b"\x01starts with the mark", b""] {
            assert_eq!(field_bytes(&bytes_field(b)).as_deref(), Some(b));
            assert!(!bytes_field(b).contains(['\t', '\n']));
        }
        assert_eq!(bytes_field(br"C:\a b\x.txt"), r"C:\a b\x.txt", "Windows paths are written as they are");
    }
}
