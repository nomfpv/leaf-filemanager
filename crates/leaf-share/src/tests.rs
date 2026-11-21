//! Two (or three) leafs in one process, on 127.0.0.1.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rf_nearby::kit::Identity;

use crate::folder::{Folder, TRASH};
use crate::store::Invite;
use crate::transport::{Quic, Transport as _};
use crate::wire::{self, Open, OpenReply, PROTO};
use crate::{Config, ShareListener, ShareService, State};

#[derive(Default)]
struct L {
    codes: Mutex<Vec<(String, String)>>,
    joined: Mutex<Vec<(String, Result<(), String>)>>,
}

impl ShareListener for L {
    fn changed(&self) {}
    fn pairing_code(&self, share: &str, _peer: &str, code: &str) {
        self.codes.lock().unwrap().push((share.to_string(), code.to_string()));
    }
    fn joined(&self, share: &str, result: Result<(), String>) {
        self.joined.lock().unwrap().push((share.to_string(), result));
    }
    fn paused(&self, _share: &str, _name: &str, _reason: &str) {}
}

fn service(dir: &Path, name: &str) -> (Arc<ShareService>, Arc<L>) {
    let mut cfg = Config::new(name, dir.join("state"), dir.join("id"), 0);
    cfg.local_only = true;
    cfg.debounce = Duration::from_millis(200);
    let l = Arc::new(L::default());
    (ShareService::start(cfg, l.clone()).unwrap(), l)
}

fn wait(what: &str, secs: u64, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read(p: PathBuf) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).filter(|n| n != TRASH).collect();
    v.sort();
    v
}

fn set_mtime(p: &Path, secs_ago: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(std::time::SystemTime::now() - Duration::from_secs(secs_ago)).unwrap();
}

/// A host with a shared folder holding `a.txt`, a guest that joined it (the code confirmed on both sides), both in sync.
struct Pair {
    _dir: tempfile::TempDir,
    host: Arc<ShareService>,
    guest: Arc<ShareService>,
    hf: PathBuf,
    gf: PathBuf,
    id: String,
    invite: String,
    root: PathBuf,
}

fn joined_pair() -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let (host, hl) = service(&root.join("h"), "Host");
    let (guest, gl) = service(&root.join("g"), "Guest");
    let hf = root.join("shared");
    let gf = root.join("joined");
    std::fs::create_dir_all(&hf).unwrap();
    std::fs::write(hf.join("a.txt"), "one").unwrap();
    let id = host.create(&hf).unwrap();
    let invite = host.invite(&id, "127.0.0.1").unwrap();
    assert_eq!(guest.join(&invite, &gf).unwrap(), id);
    wait("both codes", 10, || hl.codes.lock().unwrap().len() == 1 && gl.codes.lock().unwrap().len() == 1);
    assert_eq!(hl.codes.lock().unwrap()[0].1, gl.codes.lock().unwrap()[0].1, "both PCs show the same code");
    host.confirm_pairing(&id, true);
    guest.confirm_pairing(&id, true);
    wait("the join", 10, || gl.joined.lock().unwrap().iter().any(|(_, r)| r.is_ok()));
    wait("the first sync", 20, || read(gf.join("a.txt")) == "one" && host.list().iter().any(|s| s.state == State::InSync && s.last_sync_ms > 0));
    Pair { _dir: dir, host, guest, hf, gf, id, invite, root }
}

/// What the other side says to one `Open`, from a device with the identity in `identity_dir` (`None`: nothing at all).
fn raw_open(identity_dir: &Path, port: u16, open: Open) -> Option<OpenReply> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let me = Identity::load_or_create(identity_dir).unwrap();
        let q = Quic::bind(&me, 0, true).unwrap();
        let link = q.connect(&format!("127.0.0.1:{port}")).await.ok()?;
        let (mut s, mut r) = link.open().await.ok()?;
        wire::write_frame(&mut s, &open).await.ok()?;
        tokio::time::timeout(Duration::from_secs(5), wire::read_frame::<_, OpenReply>(&mut r)).await.ok()?.ok()
    })
}

#[test]
fn a_path_out_of_the_folder_or_through_a_link_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("shared");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "no").unwrap();
    std::fs::write(root.join("sub").join("ok.txt"), "yes").unwrap();
    let f = Folder::new(&root).unwrap();
    assert!(f.resolve("sub/ok.txt", false).is_ok());
    for bad in ["..", "../outside/secret.txt", "sub/../../outside/secret.txt", "/etc/passwd", "C:/Windows", "C:\\Windows", "sub//ok.txt", "sub/./ok.txt", "a\u{1}b"] {
        assert!(f.resolve(bad, false).is_err(), "{bad:?} was let through");
        assert!(f.open_read(bad, 0).is_err() && f.list(bad).is_err(), "{bad:?}");
    }
    // a link inside the folder that points out of it: never followed, for reading, listing or writing
    let link = root.join("out");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
    #[cfg(windows)]
    let made = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&link).arg(&outside).output().is_ok_and(|o| o.status.success());
    assert!(made, "the link for the test could not be made");
    assert!(f.list("out").is_err());
    assert!(f.open_read("out/secret.txt", 0).is_err());
    assert!(f.create_part("out/.x.1.leaf-part").is_err());
    assert!(f.rename("sub/ok.txt", "out/ok.txt").is_err());
    assert!(!outside.join("ok.txt").exists() && root.join("sub").join("ok.txt").exists());
    // and only a temporary name may be written
    assert!(f.create_part("sub/new.txt").is_err());
}

#[test]
fn a_remove_request_moves_to_the_trash_folder_and_never_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("d").join("e")).unwrap();
    std::fs::write(root.join("a.txt"), "keep me").unwrap();
    std::fs::write(root.join("d").join("e").join("b.txt"), "me too").unwrap();
    let f = Folder::new(&root).unwrap();
    f.remove("a.txt", false).unwrap();
    assert_eq!(f.remove("d", false), Err(leaf_vfs::FsError::NotEmpty));
    f.remove("d", true).unwrap();
    assert!(!root.join("a.txt").exists() && !root.join("d").exists());
    // everything is in .leaf-sync-trash/<time>/, with its bytes
    let mut found = Vec::new();
    let mut stack = vec![root.join(TRASH)];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() { stack.push(p) } else { found.push(read(p)) }
        }
    }
    found.sort();
    assert_eq!(found, vec!["keep me".to_string(), "me too".to_string()]);
    // the trash itself is never touched from the other side
    assert!(f.remove(TRASH, true).is_err());
}

#[test]
fn a_file_never_gets_deleted_by_way_of_a_temporary_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("report.docx"), "keep me").unwrap();
    std::fs::write(root.join(".old.1.leaf-part"), "a part this connection did not make").unwrap();
    let f = Folder::new(&root).unwrap();
    // a real file cannot take a temporary name
    assert_eq!(f.rename("report.docx", ".report.docx.1.leaf-part"), Err(leaf_vfs::FsError::Denied));
    assert_eq!(read(root.join("report.docx")), "keep me");
    // a temporary file it did not make goes to the trash, not away
    f.remove(".old.1.leaf-part", false).unwrap();
    assert!(!root.join(".old.1.leaf-part").exists());
    let stamp_dir = std::fs::read_dir(root.join(TRASH)).unwrap().next().unwrap().unwrap().path();
    assert_eq!(read(stamp_dir.join(".old.1.leaf-part")), "a part this connection did not make");
    // its own temporary file is deleted outright, and only that
    let (file, _) = f.create_part(".new.2.leaf-part").unwrap();
    drop(file);
    f.remove(".new.2.leaf-part", false).unwrap();
    assert!(!root.join(".new.2.leaf-part").exists() && !stamp_dir.join(".new.2.leaf-part").exists());
    // the trash folder by another case of its name is refused too (Windows names are not case-sensitive)
    if cfg!(windows) {
        assert!(f.remove(&TRASH.to_uppercase(), true).is_err());
    }
}

#[test]
fn a_file_changed_on_both_sides_keeps_both() {
    let p = joined_pair();
    std::fs::write(p.hf.join("a.txt"), "changed here").unwrap();
    std::fs::write(p.gf.join("a.txt"), "changed on the friend's PC").unwrap();
    set_mtime(&p.hf.join("a.txt"), 120);
    set_mtime(&p.gf.join("a.txt"), 10);
    p.host.sync_now(&p.id);
    wait("both files on both sides", 30, || names_in(&p.hf).len() == 2 && names_in(&p.gf).len() == 2 && read(p.hf.join("a.txt")) == read(p.gf.join("a.txt")));
    // the newer one keeps the name, the older one is kept as a conflict copy named after its PC, on both sides
    for dir in [&p.hf, &p.gf] {
        let names = names_in(dir);
        assert_eq!(read(dir.join("a.txt")), "changed on the friend's PC");
        let copy = names.iter().find(|n| n.starts_with("a (conflict from Host ") && n.ends_with(").txt")).unwrap_or_else(|| panic!("{names:?}"));
        assert_eq!(read(dir.join(copy)), "changed here");
    }
    let _ = (&p.guest, &p.invite, &p.root);
}

#[test]
fn a_peer_not_stored_for_the_share_gets_nothing() {
    let p = joined_pair();
    let port = p.host.port();
    // a device that never joined
    let stranger = p.root.join("stranger");
    assert_eq!(raw_open(&stranger, port, Open::Resume { proto: PROTO, share: p.id.clone(), name: "x".into() }), None);
    // the friend itself, for a share it is not stored for
    let other = p.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other_id = p.host.create(&other).unwrap();
    let friend = p.root.join("g").join("id");
    assert_eq!(raw_open(&friend, port, Open::Resume { proto: PROTO, share: other_id, name: "x".into() }), None);
    // while the friend, for its share, is answered
    assert!(matches!(raw_open(&friend, port, Open::Resume { proto: PROTO, share: p.id.clone(), name: "x".into() }), Some(OpenReply::Welcome { .. })));
}

#[test]
fn an_expired_or_reused_invite_is_refused() {
    let p = joined_pair();
    let port = p.host.port();
    let stranger = p.root.join("stranger");
    // used once already (by the friend)
    let used = Invite::parse(&p.invite).unwrap();
    assert_eq!(raw_open(&stranger, port, Open::Join { proto: PROTO, share: used.share, secret: used.secret, name: "x".into() }), None);
    // ran out
    let other = p.root.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let other_id = p.host.create(&other).unwrap();
    let old = Invite::parse(&p.host.invite_until(&other_id, "127.0.0.1", crate::now_ms() - 1000).unwrap()).unwrap();
    assert!(p.guest.join(&old.to_text(), &p.root.join("j2")).is_err(), "the joining side refuses it too");
    assert_eq!(raw_open(&stranger, port, Open::Join { proto: PROTO, share: old.share.clone(), secret: old.secret.clone(), name: "x".into() }), None);
    // a good one for the same share is answered (the code is asked: the stranger never met this PC)
    let good = Invite::parse(&p.host.invite(&other_id, "127.0.0.1").unwrap()).unwrap();
    assert_eq!(raw_open(&stranger, port, Open::Join { proto: PROTO, share: good.share, secret: good.secret, name: "x".into() }), Some(OpenReply::Pair));
}
