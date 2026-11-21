use std::io::Read;
use std::path::PathBuf;

use leaf_vfs::{Fs, LocalFs, MemFs, VPath};

use crate::{Action, Control, Job, Mode, Report, TRASH_DIR, plan, restore_from_trash, run};

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("leaf-sync-{name}-{}-{}", std::process::id(), crate::now_ms()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn v(s: &str) -> VPath {
    VPath::parse(s).unwrap()
}

fn read(fs: &dyn Fs, p: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    fs.open_read(&v(p), 0).ok()?.read_to_end(&mut out).ok()?;
    Some(out)
}

fn go(p: &crate::Plan, l: &dyn Fs, r: &dyn Fs, state: &std::path::Path, trash: &dyn Fn(&[PathBuf]) -> Result<(), String>) -> Report {
    let no = || false;
    let mut progress = |_: &crate::Progress| {};
    let mut ctl = Control { cancelled: &no, paused: &no, progress: &mut progress, trash_local: trash };
    run(p, l, r, state, &mut ctl).unwrap()
}

fn no_trash(_: &[PathBuf]) -> Result<(), String> {
    Err("no Trash in this test".into())
}

const T0: i64 = 1_700_000_000_000;

#[test]
fn mirror_never_deletes_when_the_left_root_is_missing() {
    let state = temp("missing");
    let fs = MemFs::new();
    fs.put_file(&v("mem://t/right/a.txt"), b"keep".to_vec(), T0);
    fs.put_file(&v("mem://t/right/b/c.txt"), b"keep too".to_vec(), T0);
    let job = Job::new("m", "mem://t/left", "mem://t/right");
    let no = || false;
    // no left folder at all: refused, nothing planned
    let e = plan(&job, &fs, &fs, &state, &no).err().expect("refused");
    assert!(e.message.contains("not there"), "{e}");

    // after a run that saw files, an EMPTY left (the drive's mount point is there, the drive is not) is refused as well
    fs.put_file(&v("mem://t/left/a.txt"), b"keep".to_vec(), T0);
    fs.put_file(&v("mem://t/left/b/c.txt"), b"keep too".to_vec(), T0);
    let p = plan(&job, &fs, &fs, &state, &no).unwrap();
    assert!(p.is_empty());
    go(&p, &fs, &fs, &state, &no_trash);
    fs.remove(&v("mem://t/left/a.txt"), false).unwrap();
    fs.remove(&v("mem://t/left/b"), true).unwrap();
    let e = plan(&job, &fs, &fs, &state, &no).err().expect("refused");
    assert!(e.message.contains("empty now") && e.message.contains("may not be connected"), "{e}");
    assert_eq!(read(&fs, "mem://t/right/a.txt").as_deref(), Some(&b"keep"[..]));
    assert_eq!(read(&fs, "mem://t/right/b/c.txt").as_deref(), Some(&b"keep too"[..]));
}

#[test]
fn two_way_conflict_keeps_both() {
    let state = temp("conflict");
    let fs = MemFs::new();
    fs.put_file(&v("mem://t/l/doc.txt"), b"same".to_vec(), T0);
    fs.put_file(&v("mem://t/r/doc.txt"), b"same".to_vec(), T0);
    let mut job = Job::new("c", "mem://t/l", "mem://t/r");
    job.mode = Mode::TwoWay;
    let no = || false;
    let p = plan(&job, &fs, &fs, &state, &no).unwrap();
    assert!(p.is_empty());
    go(&p, &fs, &fs, &state, &no_trash);

    // both sides change the file after that run
    fs.put_file(&v("mem://t/l/doc.txt"), b"left edit".to_vec(), T0 + 60_000);
    fs.put_file(&v("mem://t/r/doc.txt"), b"right edit, longer".to_vec(), T0 + 120_000);
    let p = plan(&job, &fs, &fs, &state, &no).unwrap();
    assert_eq!(p.rows.len(), 1);
    assert_eq!(p.rows[0].action, Action::Conflict);
    let rep = go(&p, &fs, &fs, &state, &no_trash);
    assert_eq!(rep.conflicts, vec!["doc.txt".to_string()]);
    assert!(rep.done.is_empty() && rep.failed.is_empty(), "{rep:?}");
    assert_eq!(read(&fs, "mem://t/l/doc.txt").as_deref(), Some(&b"left edit"[..]));
    assert_eq!(read(&fs, "mem://t/r/doc.txt").as_deref(), Some(&b"right edit, longer"[..]));
    // and it is still a conflict next time (never resolved by itself)
    let p = plan(&job, &fs, &fs, &state, &no).unwrap();
    assert_eq!(p.rows[0].action, Action::Conflict);
}

#[test]
fn a_deletion_goes_to_the_trash_and_is_restorable() {
    let no = || false;
    // a server side: into .leaf-sync-trash/<run>/ by rename, and back from there
    let state = temp("trash");
    let fs = MemFs::new();
    fs.put_file(&v("mem://t/l/keep.txt"), b"k".to_vec(), T0);
    fs.put_file(&v("mem://t/r/keep.txt"), b"k".to_vec(), T0);
    fs.put_file(&v("mem://t/r/old/extra.txt"), b"precious".to_vec(), T0);
    let job = Job::new("t", "mem://t/l", "mem://t/r");
    let mut p = plan(&job, &fs, &fs, &state, &no).unwrap();
    assert_eq!(p.rows.len(), 1, "the folder goes as a whole");
    assert!(p.rows[0].whole && p.rows[0].files == 1);
    // (half of the right side's files: the app asks a second time)
    assert!(p.needs_second_ask());
    p.rows[0].action = Action::DeleteRight;
    let rep = go(&p, &fs, &fs, &state, &no_trash);
    assert_eq!(rep.removed, 1, "{rep:?}");
    assert!(read(&fs, "mem://t/r/old/extra.txt").is_none());
    let kept = format!("mem://t/r/{TRASH_DIR}/{}/old/extra.txt", rep.trash_stamp);
    assert_eq!(read(&fs, &kept).as_deref(), Some(&b"precious"[..]));
    // the trash folder is never part of a plan
    assert!(plan(&job, &fs, &fs, &state, &no).unwrap().is_empty());
    restore_from_trash(&fs, &v("mem://t/r"), &rep.trash_stamp, "old").unwrap();
    assert_eq!(read(&fs, "mem://t/r/old/extra.txt").as_deref(), Some(&b"precious"[..]));

    // a local side: to the Trash through the caller (here a folder standing in for it), and an overwritten file too
    let state = temp("trash-local-state");
    let (l, r, bin) = (temp("trash-l"), temp("trash-r"), temp("trash-bin"));
    std::fs::write(l.join("a.txt"), b"new").unwrap();
    std::fs::write(r.join("a.txt"), b"old version").unwrap();
    std::fs::write(r.join("gone.txt"), b"was here").unwrap();
    let job = Job::new("t", &LocalFs::address(&l).to_string(), &LocalFs::address(&r).to_string());
    let p = plan(&job, &LocalFs, &LocalFs, &state, &no).unwrap();
    let trash = |paths: &[PathBuf]| -> Result<(), String> {
        for p in paths {
            std::fs::rename(p, bin.join(p.file_name().unwrap())).map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    let rep = go(&p, &LocalFs, &LocalFs, &state, &trash);
    assert!(rep.failed.is_empty(), "{rep:?}");
    assert_eq!(std::fs::read(r.join("a.txt")).unwrap(), b"new");
    assert!(!r.join("gone.txt").exists());
    assert_eq!(std::fs::read(bin.join("gone.txt")).unwrap(), b"was here");
    assert_eq!(std::fs::read(bin.join("a.txt")).unwrap(), b"old version");
}

#[test]
fn a_link_on_the_right_is_not_written_through() {
    let state = temp("link");
    let no = || false;
    let fs = MemFs::new();
    fs.put_dir(&v("mem://t/elsewhere"), T0);
    fs.put_file(&v("mem://t/l/sub/a.txt"), b"x".to_vec(), T0);
    fs.put_link(&v("mem://t/r/sub"), "/elsewhere");
    let job = Job::new("k", "mem://t/l", "mem://t/r");
    let mut p = plan(&job, &fs, &fs, &state, &no).unwrap();
    assert!(p.rows.iter().all(|r| r.action == Action::Skip && r.fixed), "{:?}", p.rows);
    let i = p.rows.iter().position(|r| r.key == "sub/a.txt").unwrap();
    assert!(!p.set_action(i, Action::ToRight), "a row inside a link can only be skipped");
    go(&p, &fs, &fs, &state, &no_trash);
    assert!(read(&fs, "mem://t/elsewhere/a.txt").is_none());

    // a link that appears between the preview and the run: the row fails, nothing goes through it
    let fs = MemFs::new();
    fs.put_dir(&v("mem://t/elsewhere"), T0);
    fs.put_file(&v("mem://t/l/sub/a.txt"), b"x".to_vec(), T0);
    fs.put_dir(&v("mem://t/r/sub"), T0);
    let p = plan(&job, &fs, &fs, &temp("link2"), &no).unwrap();
    assert!(p.rows.iter().any(|r| r.key == "sub/a.txt" && r.action == Action::ToRight));
    fs.remove(&v("mem://t/r/sub"), true).unwrap();
    fs.put_link(&v("mem://t/r/sub"), "/elsewhere");
    let rep = go(&p, &fs, &fs, &temp("link3"), &no_trash);
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(read(&fs, "mem://t/elsewhere/a.txt").is_none());
}

#[test]
fn the_plan_equals_what_the_run_does() {
    let state = temp("equal");
    let no = || false;
    let fs = MemFs::new();
    fs.put_file(&v("mem://t/l/same.txt"), b"s".to_vec(), T0);
    fs.put_file(&v("mem://t/r/same.txt"), b"s".to_vec(), T0);
    fs.put_file(&v("mem://t/l/changed.txt"), b"new text".to_vec(), T0 + 10_000);
    fs.put_file(&v("mem://t/r/changed.txt"), b"old".to_vec(), T0);
    fs.put_file(&v("mem://t/l/new/deep/n.txt"), b"n".to_vec(), T0);
    fs.put_dir(&v("mem://t/l/empty"), T0);
    fs.put_file(&v("mem://t/l/skip-me.txt"), b"later".to_vec(), T0);
    fs.put_file(&v("mem://t/r/extra.txt"), b"e".to_vec(), T0);
    fs.put_file(&v("mem://t/r/olddir/x.txt"), b"x".to_vec(), T0);
    fs.put_file(&v("mem://t/l/data.tmp"), b"left out".to_vec(), T0);
    fs.put_file(&v("mem://t/r/cache.tmp"), b"left out too".to_vec(), T0);
    let mut job = Job::new("e", "mem://t/l", "mem://t/r");
    job.exclude = vec!["*.tmp".into()];
    let mut p = plan(&job, &fs, &fs, &state, &no).unwrap();
    let i = p.rows.iter().position(|r| r.key == "skip-me.txt").unwrap();
    assert!(p.set_action(i, Action::Skip));
    let mut want: Vec<(String, Action)> =
        p.rows.iter().filter(|r| !matches!(r.action, Action::Skip | Action::Conflict)).map(|r| (r.key.clone(), r.action)).collect();
    let rep = go(&p, &fs, &fs, &state, &no_trash);
    let mut got = rep.done.clone();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(got, want);
    assert!(rep.failed.is_empty(), "{rep:?}");
    let c = p.counts();
    assert_eq!((rep.copied, rep.removed, rep.skipped), (c.to_right + c.to_left, c.delete_left + c.delete_right, c.skipped));
    // what the plan left out is untouched, and a second plan only has the row the user skipped
    assert!(read(&fs, "mem://t/r/data.tmp").is_none());
    assert_eq!(read(&fs, "mem://t/r/cache.tmp").as_deref(), Some(&b"left out too"[..]));
    assert_eq!(read(&fs, "mem://t/r/changed.txt").as_deref(), Some(&b"new text"[..]));
    let again = plan(&job, &fs, &fs, &state, &no).unwrap();
    assert_eq!(again.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), vec!["skip-me.txt"]);
}

/// A place whose reads end early (a server read that stops with Ok(0), or a file that shrank while it was read).
struct ShortReads<'a>(&'a MemFs);

impl Fs for ShortReads<'_> {
    fn caps(&self) -> leaf_vfs::Caps {
        self.0.caps()
    }
    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<leaf_vfs::Entry>)) -> Result<usize, leaf_vfs::FsError> {
        self.0.list(dir, batch, cancelled, on_batch)
    }
    fn stat(&self, p: &VPath) -> Result<leaf_vfs::Meta, leaf_vfs::FsError> {
        self.0.stat(p)
    }
    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, leaf_vfs::FsError> {
        Ok(Box::new(self.0.open_read(p, offset)?.take(3)))
    }
    fn open_write(&self, p: &VPath, o: leaf_vfs::WriteOpts) -> Result<Box<dyn leaf_vfs::WriteSink>, leaf_vfs::FsError> {
        self.0.open_write(p, o)
    }
    fn mkdir(&self, p: &VPath) -> Result<(), leaf_vfs::FsError> {
        self.0.mkdir(p)
    }
    fn rename(&self, from: &VPath, to: &VPath, replace: bool) -> Result<(), leaf_vfs::FsError> {
        self.0.rename(from, to, replace)
    }
    fn remove(&self, p: &VPath, recursive: bool) -> Result<(), leaf_vfs::FsError> {
        self.0.remove(p, recursive)
    }
    fn set_meta(&self, p: &VPath, m: &leaf_vfs::MetaChange) -> Result<(), leaf_vfs::FsError> {
        self.0.set_meta(p, m)
    }
}

#[test]
fn a_read_that_stops_early_never_replaces_the_target() {
    let state = temp("short");
    let no = || false;
    let fs = MemFs::new();
    fs.put_file(&v("mem://t/l/doc.txt"), b"the whole new text".to_vec(), T0 + 10_000);
    fs.put_file(&v("mem://t/r/doc.txt"), b"old".to_vec(), T0);
    let job = Job::new("s", "mem://t/l", "mem://t/r");
    let p = plan(&job, &fs, &fs, &state, &no).unwrap();
    assert_eq!(p.rows.len(), 1);
    let rep = go(&p, &ShortReads(&fs), &fs, &state, &no_trash);
    assert_eq!(rep.copied, 0, "{rep:?}");
    assert_eq!(rep.failed.len(), 1, "{rep:?}");
    assert!(rep.failed[0].1.contains("changed while it was copied"), "{rep:?}");
    assert_eq!(read(&fs, "mem://t/r/doc.txt").as_deref(), Some(&b"old"[..]));
    // the old file was not moved away and no part file is left
    let mut names = Vec::new();
    fs.list(&v("mem://t/r"), 100, &no, &mut |b| names.extend(b.into_iter().map(|e| e.name.to_string()))).unwrap();
    assert_eq!(names, vec!["doc.txt".to_string()]);
}
