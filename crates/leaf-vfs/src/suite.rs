//! The read-only part of the `Fs` semantics, shared with the backends of other crates (feature `suite`): a backend that
//! cannot write yet (SFTP in its first lane) is checked against the same rules as `MemFs` and `LocalFs`.
//!
//! The tree is made through `setup` (a backend that writes to the same place, e.g. `LocalFs` on the folder a test server
//! serves) and read back through `fs`.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{Fs, FsError, Kind, VPath, WriteOpts};

fn put(fs: &dyn Fs, p: &VPath, data: &[u8]) {
    let mut w = fs.open_write(p, WriteOpts::REPLACE).unwrap();
    w.write_all(data).unwrap();
    w.finish().unwrap();
}

fn read(fs: &dyn Fs, p: &VPath, offset: u64) -> Vec<u8> {
    let mut v = Vec::new();
    fs.open_read(p, offset).unwrap().read_to_end(&mut v).unwrap();
    v
}

fn names(fs: &dyn Fs, dir: &VPath) -> Vec<String> {
    let mut v = Vec::new();
    fs.list(dir, 2, &|| false, &mut |b| v.extend(b.into_iter().map(|e| e.name.to_string()))).unwrap();
    v.sort();
    v
}

/// A file big enough to need many reads (and not a multiple of any read size).
fn big() -> Vec<u8> {
    (0..300_001u32).map(|i| (i * 7 + i / 251) as u8).collect()
}

/// `make_link(at, target)` makes a folder link through `setup`; false when the system does not let us.
pub fn read_only(fs: &dyn Fs, root: &VPath, setup: &dyn Fs, setup_root: &VPath, make_link: &dyn Fn(&VPath, &VPath) -> bool) {
    // the tree, made through `setup`
    let sa = setup_root.join("a");
    setup.mkdir(&sa).unwrap();
    put(setup, &sa.join("one.txt"), b"hello world");
    for i in 0..5 {
        put(setup, &sa.join(&format!("n{i}")), b"x");
    }
    put(setup, &setup_root.join("big.bin"), &big());
    setup.mkdir(&setup_root.join("target")).unwrap();
    put(setup, &setup_root.join("target/keep.txt"), b"keep");
    let linked = make_link(&setup_root.join("link"), &setup_root.join("target"));

    // read back through `fs`
    let a = root.join("a");
    assert!(fs.stat(&a).unwrap().is_dir());
    assert_eq!(fs.stat(&root.join("nothing")), Err(FsError::NotFound));
    let m = fs.stat(&a.join("one.txt")).unwrap();
    assert_eq!((m.kind, m.size), (Kind::File, 11));
    assert_eq!(read(fs, &a.join("one.txt"), 0), b"hello world");
    assert_eq!(read(fs, &a.join("one.txt"), 6), b"world");
    assert!(read(fs, &a.join("one.txt"), 11).is_empty(), "from the end: nothing");
    assert!(fs.open_read(&a.join("missing.txt"), 0).is_err());
    let b = big();
    assert_eq!(fs.stat(&root.join("big.bin")).unwrap().size, b.len() as u64);
    assert!(read(fs, &root.join("big.bin"), 0) == b, "the big file reads back whole");
    assert!(read(fs, &root.join("big.bin"), 200_001) == b[200_001..], "and from an offset");
    // a reader dropped half way leaves the backend usable
    let mut r = fs.open_read(&root.join("big.bin"), 0).unwrap();
    let mut half = vec![0u8; 1000];
    r.read_exact(&mut half).unwrap();
    drop(r);
    assert_eq!(half, b[..1000]);

    // listing in batches, cancel between batches, a missing folder
    assert_eq!(names(fs, &a), ["n0", "n1", "n2", "n3", "n4", "one.txt"]);
    let seen = AtomicUsize::new(0);
    let r = fs.list(&a, 2, &|| seen.load(Ordering::Relaxed) > 0, &mut |b| {
        seen.fetch_add(b.len(), Ordering::Relaxed);
    });
    assert!(r == Err(FsError::Cancelled) || r == Ok(6), "cancel between batches: {r:?}");
    assert_eq!(fs.list(&a.join("nope"), 10, &|| false, &mut |_| {}), Err(FsError::NotFound));

    // links: listed and stat'ed as links, not followed
    if linked {
        assert_eq!(fs.stat(&root.join("link")).unwrap().kind, Kind::Link);
        let mut es = Vec::new();
        fs.list(root, 100, &|| false, &mut |b| es.extend(b)).unwrap();
        assert!(es.iter().any(|e| &*e.name == "link" && e.link), "the listing marks the link");
    }

    // a backend that cannot write says so, and changes nothing
    if !fs.caps().write {
        let f = a.join("one.txt");
        assert!(matches!(fs.open_write(&a.join("new.txt"), WriteOpts::CREATE_NEW), Err(FsError::Unsupported)));
        assert!(matches!(fs.open_write(&f, WriteOpts::REPLACE), Err(FsError::Unsupported)));
        assert_eq!(fs.mkdir(&root.join("new")), Err(FsError::Unsupported));
        assert_eq!(fs.rename(&f, &a.join("moved.txt"), false), Err(FsError::Unsupported));
        assert_eq!(fs.remove(&f, false), Err(FsError::Unsupported));
        assert_eq!(fs.set_meta(&f, &crate::MetaChange { modified_ms: Some(0), mode: None }), Err(FsError::Unsupported));
        assert_eq!(read(fs, &f, 0), b"hello world", "untouched");
    }

    // clean up through `setup`
    if linked {
        setup.remove(&setup_root.join("link"), false).unwrap();
    }
    setup.remove(&setup_root.join("target"), true).unwrap();
    setup.remove(&sa, true).unwrap();
    setup.remove(&setup_root.join("big.bin"), false).unwrap();
    assert!(names(setup, setup_root).is_empty(), "the suite leaves nothing behind");
}
