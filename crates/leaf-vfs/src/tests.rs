//! The semantics of `Fs`, one suite run against `MemFs` and against `LocalFs` on a temp folder, and the copy between them.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::copy::{Conflict, Copy, part_name};
use crate::*;

fn names(fs: &dyn Fs, dir: &VPath) -> Vec<String> {
    let mut v = Vec::new();
    fs.list(dir, 2, &|| false, &mut |b| v.extend(b.into_iter().map(|e| e.name.to_string()))).unwrap();
    v.sort();
    v
}

fn write(fs: &dyn Fs, p: &VPath, data: &[u8]) {
    let mut w = fs.open_write(p, WriteOpts::REPLACE).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap();
}

fn read(fs: &dyn Fs, p: &VPath, offset: u64) -> Vec<u8> {
    let mut v = Vec::new();
    fs.open_read(p, offset).unwrap().read_to_end(&mut v).unwrap();
    v
}

/// `make_link(at, target)`: a link at `at` to the folder `target`; false when the system does not let us make one.
fn suite(fs: &dyn Fs, root: &VPath, make_link: &dyn Fn(&VPath, &VPath) -> bool) {
    let a = root.join("a");
    fs.mkdir(&a).unwrap();
    assert_eq!(fs.mkdir(&a), Err(FsError::Exists));
    assert_eq!(fs.mkdir(&root.join("missing/x")), Err(FsError::NotFound));
    assert!(fs.stat(&a).unwrap().is_dir());
    assert_eq!(fs.stat(&root.join("nothing")), Err(FsError::NotFound));

    // write, read back from an offset, resume at an offset, create-new refuses
    let f = a.join("one.txt");
    write(fs, &f, b"hello world");
    assert_eq!(read(fs, &f, 6), b"world");
    let m = fs.stat(&f).unwrap();
    assert_eq!((m.kind, m.size), (Kind::File, 11));
    assert!(matches!(fs.open_write(&f, WriteOpts::CREATE_NEW), Err(FsError::Exists)));
    let mut w = fs.open_write(&f, WriteOpts { mode: WriteMode::At(5) }).unwrap();
    w.write_all(b"!").unwrap();
    w.finish().unwrap();
    assert_eq!(read(fs, &f, 0), b"hello!");
    assert!(fs.open_write(&root.join("missing/f"), WriteOpts::REPLACE).is_err());

    // listing in batches
    for i in 0..5 {
        write(fs, &a.join(&format!("n{i}")), b"x");
    }
    assert_eq!(names(fs, &a), ["n0", "n1", "n2", "n3", "n4", "one.txt"]);
    let seen = AtomicUsize::new(0);
    let r = fs.list(&a, 2, &|| seen.load(Ordering::Relaxed) > 0, &mut |b| {
        seen.fetch_add(b.len(), Ordering::Relaxed);
    });
    assert!(r == Err(FsError::Cancelled) || r == Ok(6), "cancel between batches: {r:?}");
    assert_eq!(fs.list(&a.join("nope"), 10, &|| false, &mut |_| {}), Err(FsError::NotFound));

    // rename never replaces unless asked, and then only a file
    let g = a.join("two.txt");
    write(fs, &g, b"two");
    assert_eq!(fs.rename(&f, &g, false), Err(FsError::Exists));
    assert_eq!(read(fs, &g, 0), b"two", "the target is untouched");
    assert_eq!(read(fs, &f, 0), b"hello!", "the source is untouched");
    fs.rename(&f, &g, true).unwrap();
    assert_eq!(read(fs, &g, 0), b"hello!");
    assert_eq!(fs.stat(&f), Err(FsError::NotFound));
    let b = root.join("b");
    fs.mkdir(&b).unwrap();
    assert_eq!(fs.rename(&g, &b, true), Err(FsError::Exists), "a folder is never replaced");
    assert!(fs.rename(&a, &a.join("inside"), false).is_err(), "not into itself");
    // a folder moves with what is in it
    let c = root.join("c");
    fs.rename(&a, &c, false).unwrap();
    assert_eq!(names(fs, &c), ["n0", "n1", "n2", "n3", "n4", "two.txt"]);
    assert_eq!(fs.stat(&a), Err(FsError::NotFound));

    // set_meta: the modification time
    let t = 1_600_000_000_000i64;
    fs.set_meta(&c.join("n0"), &MetaChange { modified_ms: Some(t), mode: None }).unwrap();
    assert_eq!(fs.stat(&c.join("n0")).unwrap().modified_ms, t);

    // remove refuses a folder with something in it unless recursive
    assert_eq!(fs.remove(&c, false), Err(FsError::NotEmpty));
    fs.remove(&c.join("n0"), false).unwrap();
    fs.remove(&b, false).unwrap();
    fs.remove(&c, true).unwrap();
    assert_eq!(fs.stat(&c), Err(FsError::NotFound));
    assert_eq!(fs.remove(&c, true), Err(FsError::NotFound));

    // links: listed as links, removing one leaves what it points to
    let target = root.join("target");
    fs.mkdir(&target).unwrap();
    write(fs, &target.join("keep.txt"), b"keep");
    let link = root.join("link");
    if make_link(&link, &target) {
        assert_eq!(fs.stat(&link).unwrap().kind, Kind::Link);
        let mut es = Vec::new();
        fs.list(root, 100, &|| false, &mut |b| es.extend(b)).unwrap();
        assert!(es.iter().any(|e| &*e.name == "link" && e.link), "the listing marks the link");
        fs.remove(&link, true).unwrap();
        assert_eq!(read(fs, &target.join("keep.txt"), 0), b"keep");
    }
    fs.remove(&target, true).unwrap();
    assert!(names(fs, root).is_empty(), "the suite leaves nothing behind: {:?}", names(fs, root));
}

/// A tree to copy: files, a sub folder, and a link to a folder outside the tree.
fn tree(fs: &dyn Fs, root: &VPath, make_link: &dyn Fn(&VPath, &VPath) -> bool) -> (VPath, bool) {
    let src = root.join("src");
    fs.mkdir(&src).unwrap();
    fs.mkdir(&src.join("sub")).unwrap();
    write(fs, &src.join("big.bin"), &vec![7u8; 100_000]);
    write(fs, &src.join("sub/small.txt"), b"small");
    fs.set_meta(&src.join("sub/small.txt"), &MetaChange { modified_ms: Some(1_500_000_000_000), mode: None }).unwrap();
    let outside = root.join("outside");
    fs.mkdir(&outside).unwrap();
    write(fs, &outside.join("secret.txt"), b"outside the tree");
    let linked = make_link(&src.join("out-link"), &outside);
    (src, linked)
}

fn copy_between(from: &dyn Fs, from_root: &VPath, to: &dyn Fs, to_root: &VPath, make_link: &dyn Fn(&VPath, &VPath) -> bool) {
    let (src, linked) = tree(from, from_root, make_link);
    let dst = to_root.join("copy");

    // cancel in the middle of the big file: no partial file is left
    let chunks = AtomicUsize::new(0);
    let cancelled = || chunks.load(Ordering::Relaxed) >= 3;
    let mut progress = |_: &copy::Progress| {
        chunks.fetch_add(1, Ordering::Relaxed);
    };
    let mut conflict = |_: &VPath, _: &Meta| Conflict::Cancel;
    let mut c = Copy::new(from, to, &cancelled, &mut progress, &mut conflict);
    c.opts.chunk = 4096;
    assert_eq!(c.run(&src.join("big.bin"), &to_root.join("big.bin")), Err(FsError::Cancelled));
    assert_eq!(to.stat(&to_root.join("big.bin")), Err(FsError::NotFound));
    assert_eq!(to.stat(&to_root.join(&part_name("big.bin"))), Err(FsError::NotFound), "the partial file is removed");

    // the whole tree; the link is not followed
    let mut progress = |_: &copy::Progress| {};
    let mut conflict = |_: &VPath, _: &Meta| Conflict::Cancel;
    let r = Copy::new(from, to, &|| false, &mut progress, &mut conflict).run(&src, &dst).unwrap();
    assert_eq!((r.files, r.dirs, r.bytes), (2, 2, 100_005));
    assert_eq!(read(to, &dst.join("sub/small.txt"), 0), b"small");
    assert_eq!(read(to, &dst.join("big.bin"), 0).len(), 100_000);
    assert_eq!(to.stat(&dst.join("sub/small.txt")).unwrap().modified_ms, 1_500_000_000_000, "mtime kept");
    if linked {
        assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
        assert_eq!(to.stat(&dst.join("out-link")), Err(FsError::NotFound), "the link is not followed");
    }
    assert!(!names(to, &dst).iter().any(|n| n.ends_with(".leaf-part")));

    // again: conflicts are asked; keep both, then skip
    let asked = AtomicUsize::new(0);
    let mut conflict = |_: &VPath, _: &Meta| {
        asked.fetch_add(1, Ordering::Relaxed);
        Conflict::KeepBoth
    };
    let mut progress = |_: &copy::Progress| {};
    Copy::new(from, to, &|| false, &mut progress, &mut conflict).run(&src.join("sub/small.txt"), &dst.join("sub/small.txt")).unwrap();
    assert_eq!(asked.load(Ordering::Relaxed), 1);
    assert_eq!(names(to, &dst.join("sub")), ["small (2).txt", "small.txt"]);
    let mut conflict = |_: &VPath, _: &Meta| Conflict::Skip;
    let r = Copy::new(from, to, &|| false, &mut progress, &mut conflict).run(&src, &dst).unwrap();
    assert_eq!(r.files, 0);

    to.remove(&dst, true).unwrap();
    if linked {
        from.remove(&src.join("out-link"), false).unwrap();
    }
    from.remove(&src, true).unwrap();
    from.remove(&from_root.join("outside"), true).unwrap();
}

fn mem_link(fs: &MemFs) -> impl Fn(&VPath, &VPath) -> bool + '_ {
    move |at, target| {
        fs.put_link(at, target.path());
        true
    }
}

/// A folder link on the disk: a symbolic link where allowed, else a junction (Windows lets anyone make those).
fn disk_link(at: &VPath, target: &VPath) -> bool {
    let (at, target) = (LocalFs::native(at), LocalFs::native(target));
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(&target, &at).is_ok() {
            return true;
        }
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&at)
            .arg(&target)
            .output()
            .is_ok_and(|o| o.status.success())
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, &at).is_ok()
    }
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!("leaf-vfs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn addr(&self) -> VPath {
        LocalFs::address(&self.0)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn semantics_mem() {
    let fs = MemFs::new();
    let root = VPath::parse("mem://test/").unwrap();
    suite(&fs, &root, &mem_link(&fs));
}

#[test]
fn semantics_local() {
    let tmp = TempDir::new("suite");
    suite(&LocalFs, &tmp.addr(), &disk_link);
}

#[test]
fn copy_mem_to_local_and_back() {
    let mem = MemFs::new();
    let tmp = TempDir::new("copy");
    let mroot = VPath::parse("mem://test/").unwrap();
    copy_between(&mem, &mroot, &LocalFs, &tmp.addr(), &mem_link(&mem));
    copy_between(&LocalFs, &tmp.addr(), &mem, &mroot, &disk_link);
    let other = mroot.join("other");
    mem.mkdir(&other).unwrap();
    copy_between(&mem, &mroot, &mem, &other, &mem_link(&mem));
    mem.remove(&other, false).unwrap();
}

#[test]
fn registry_and_test_servers() {
    assert!(has_backend("mem") && has_backend("FILE"));
    assert_eq!(fs_for("nope").err(), Some(FsError::Unsupported));
    let fs = fs_for("mem").unwrap();
    assert!(Arc::ptr_eq(&fs, &fs_for("mem").unwrap()), "created once");
    let demo = VPath::parse("mem://demo/").unwrap();
    assert!(names(&*fs, &demo).contains(&"Documents".to_string()));
    let off = VPath::parse("mem://offline/").unwrap();
    assert!(matches!(fs.list(&off, 10, &|| false, &mut |_| {}), Err(FsError::Offline(_))));
    assert!(matches!(fs.stat(&VPath::parse("mem://locked/").unwrap()), Err(FsError::Auth(_))));
    let p = std::path::Path::new(r"C:\data\x");
    if cfg!(windows) {
        assert_eq!(LocalFs::address(p).to_string(), "file:///C:/data/x");
        assert_eq!(LocalFs::native(&LocalFs::address(p)), p);
        assert_eq!(LocalFs::native(&VPath::parse("file://server/share/d").unwrap()), std::path::Path::new(r"\\server\share\d"));
    }
}
