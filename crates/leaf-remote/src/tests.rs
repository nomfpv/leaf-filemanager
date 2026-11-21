//! Tests against an SFTP server in this process (russh's server side + russh-sftp's), on 127.0.0.1 and an ephemeral
//! port, serving a temp folder. No network, no real host. Every key is made at run time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use leaf_vfs::copy::{Conflict, Copy, part_name};
use leaf_vfs::{Fs, FsError, LocalFs, VPath};
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version};

use crate::prompt::{HostKeyAnswer, HostKeyQuestion, Prompter, Secret, SecretQuestion};
use crate::{Config, SftpFs};

const USER: &str = "tester";

// ---------------------------------------------------------------- the server

#[derive(Clone)]
struct ServerSide {
    root: PathBuf,
    password: Arc<String>,
    client_key: PublicKey,
    logins: Arc<AtomicUsize>,
    /// The server lies (see `Disk::hostile`).
    hostile: bool,
}

struct Sess {
    s: ServerSide,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl russh::server::Handler for Sess {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if user == USER && password == self.s.password.as_str() {
            self.s.logins.fetch_add(1, Ordering::Relaxed);
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey_offered(&mut self, user: &str, offer: russh::server::PublicKeyOffer<'_>) -> Result<Auth, Self::Error> {
        Ok(if user == USER && offer.public_key.key_data() == self.s.client_key.key_data() { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_publickey(&mut self, user: &str, key: russh::server::VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        Ok(if user == USER && key.public_key.key_data() == self.s.client_key.key_data() {
            self.s.logins.fetch_add(1, Ordering::Relaxed);
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: russh::server::ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.close(channel)?;
        Ok(())
    }

    async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        match (name, self.channels.remove(&id)) {
            ("sftp", Some(ch)) => {
                session.channel_success(id)?;
                russh_sftp::server::run(ch.into_stream(), Disk { root: self.s.root.clone(), handles: HashMap::new(), next: 0, hostile: self.s.hostile }).await;
            }
            _ => session.channel_failure(id)?,
        }
        Ok(())
    }
}

enum Open {
    Dir(Vec<File>),
    File(std::fs::File),
}

/// A read-only SFTP server over a folder (directory parts of three names, to test the streaming).
struct Disk {
    root: PathBuf,
    handles: HashMap<String, Open>,
    next: u64,
    /// `/pics` lists names that try to leave the folder (and `invoice.pdf`, listed as a file, is a folder to `lstat`); every
    /// file in it reads as `<root>/payload`.
    hostile: bool,
}

/// What the hostile `/pics` lists: (name, is a folder).
const HOSTILE_PICS: &[(&str, bool)] = &[
    ("../../evil.txt", false),
    ("..\\..\\evil2.txt", false),
    ("a/b.txt", false),
    ("", true),
    ("..", true),
    ("CON", false),
    ("x:y.txt", false),
    ("D:evil.txt", false),
    ("trailing.", false),
    ("invoice.pdf", false),
    ("ok.txt", false),
];

fn hostile_attrs(dir: bool, size: u64) -> FileAttributes {
    FileAttributes { size: Some(size), permissions: Some(if dir { 0o040755 } else { 0o100644 }), mtime: Some(1_700_000_000), ..FileAttributes::default() }
}

fn attrs_of(m: &std::fs::Metadata) -> FileAttributes {
    let kind = if m.file_type().is_symlink() {
        0o120000
    } else if m.is_dir() {
        0o040000
    } else {
        0o100000
    };
    FileAttributes {
        size: Some(if m.is_dir() { 0 } else { m.len() }),
        permissions: Some(kind | 0o644),
        mtime: m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as u32),
        ..FileAttributes::default()
    }
}

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

fn io_status(e: std::io::Error) -> StatusCode {
    match e.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}

impl Disk {
    fn local(&self, p: &str) -> PathBuf {
        let mut out = self.root.clone();
        for seg in p.split('/').filter(|s| !s.is_empty() && *s != ".") {
            out.push(seg);
        }
        out
    }
    fn handle(&mut self, o: Open) -> String {
        self.next += 1;
        let h = format!("h{}", self.next);
        self.handles.insert(h.clone(), o);
        h
    }
}

impl russh_sftp::server::Handler for Disk {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _version: u32, _ext: HashMap<String, String>) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let p = if path == "." || path.is_empty() { "/home/tester".to_string() } else { path };
        Ok(Name { id, files: vec![File::dummy(p)] })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        if self.hostile && path == "/pics" {
            let files = HOSTILE_PICS.iter().map(|&(n, d)| File::new(n, hostile_attrs(d, 5))).collect();
            return Ok(Handle { id, handle: self.handle(Open::Dir(files)) });
        }
        let dir = self.local(&path);
        let mut files = Vec::new();
        for e in std::fs::read_dir(&dir).map_err(io_status)? {
            let e = e.map_err(io_status)?;
            let m = std::fs::symlink_metadata(e.path()).map_err(io_status)?;
            files.push(File::new(e.file_name().to_string_lossy().into_owned(), attrs_of(&m)));
        }
        files.push(File::new(".", FileAttributes::dummy()));
        Ok(Handle { id, handle: self.handle(Open::Dir(files)) })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir(files)) if !files.is_empty() => {
                let take = files.len().min(3);
                Ok(Name { id, files: files.drain(..take).collect() })
            }
            Some(Open::Dir(_)) => Err(StatusCode::Eof),
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(&mut self, id: u32, filename: String, pflags: OpenFlags, _attrs: FileAttributes) -> Result<Handle, Self::Error> {
        if pflags.intersects(OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::APPEND) {
            return Err(StatusCode::PermissionDenied);
        }
        let real = if self.hostile && filename.starts_with("/pics/") { self.root.join("payload") } else { self.local(&filename) };
        let f = std::fs::File::open(real).map_err(io_status)?;
        Ok(Handle { id, handle: self.handle(Open::File(f)) })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
        use std::io::{Read, Seek, SeekFrom};
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(SeekFrom::Start(offset)).map_err(io_status)?;
        // at most 20000 bytes per answer: the client must cope with short reads
        let mut buf = vec![0u8; (len as usize).min(20_000)];
        let n = f.read(&mut buf).map_err(io_status)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        if self.hostile && (path == "/pics" || path.starts_with("/pics/")) {
            // listed as a file, a folder now: the fetch must refuse it
            let dir = path == "/pics" || path == "/pics/invoice.pdf";
            return Ok(Attrs { id, attrs: hostile_attrs(dir, 5) });
        }
        let m = std::fs::symlink_metadata(self.local(&path)).map_err(io_status)?;
        Ok(Attrs { id, attrs: attrs_of(&m) })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let m = std::fs::metadata(self.local(&path)).map_err(io_status)?;
        Ok(Attrs { id, attrs: attrs_of(&m) })
    }
}

struct TestServer {
    port: u16,
    host_key: PrivateKey,
    logins: Arc<AtomicUsize>,
    /// The host keys the next connection is served with.
    keys: Arc<Mutex<Vec<PrivateKey>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    fn start(root: &Path, password: &str, client_key: PublicKey, host_key: PrivateKey) -> TestServer {
        TestServer::start_with(root, password, client_key, vec![host_key], false)
    }

    fn start_with(root: &Path, password: &str, client_key: PublicKey, host_keys: Vec<PrivateKey>, hostile: bool) -> TestServer {
        let side = ServerSide { root: root.to_path_buf(), password: Arc::new(password.into()), client_key, logins: Arc::default(), hostile };
        let logins = side.logins.clone();
        let host_key = host_keys[0].clone();
        let keys = Arc::new(Mutex::new(host_keys));
        let served_keys = keys.clone();
        let make_cfg = move || {
            Arc::new(russh::server::Config {
                auth_rejection_time: Duration::from_millis(5),
                auth_rejection_time_initial: Some(Duration::ZERO),
                keys: served_keys.lock().unwrap().clone(),
                ..Default::default()
            })
        };
        let (port_tx, port_rx) = std::sync::mpsc::channel();
        let (stop, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                port_tx.send(listener.local_addr().unwrap().port()).unwrap();
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        acc = listener.accept() => {
                            let Ok((sock, _)) = acc else { continue };
                            let h = Sess { s: side.clone(), channels: HashMap::new() };
                            let cfg = make_cfg();
                            tokio::spawn(async move {
                                if let Ok(s) = russh::server::run_stream(cfg, sock, h).await {
                                    let _ = s.await;
                                }
                            });
                        }
                    }
                }
            });
        });
        let port = port_rx.recv_timeout(Duration::from_secs(10)).expect("the test server started");
        TestServer { port, host_key, logins, keys, stop: Some(stop), thread: Some(thread) }
    }

    fn addr(&self, path: &str) -> VPath {
        VPath::parse(&format!("sftp://{USER}@127.0.0.1:{}{path}", self.port)).unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------------------------------------------------------- the user, played by the test

#[derive(Default)]
struct Answers {
    host_key: Mutex<Vec<HostKeyAnswer>>,
    secrets: Mutex<Vec<Option<String>>>,
    asked: Mutex<Vec<String>>,
}

impl Prompter for Answers {
    fn host_key(&self, q: HostKeyQuestion, reply: Box<dyn FnOnce(HostKeyAnswer) + Send>) {
        assert!(q.fingerprint.starts_with("SHA256:"), "{q:?}");
        self.asked.lock().unwrap().push(format!("host key {}", q.host));
        let a = self.host_key.lock().unwrap().pop().unwrap_or(HostKeyAnswer::Cancel);
        // answered from another thread, as a dialog would
        std::thread::spawn(move || reply(a));
    }
    fn secret(&self, q: SecretQuestion, reply: Box<dyn FnOnce(Option<Secret>) + Send>) {
        self.asked.lock().unwrap().push(match &q {
            SecretQuestion::Passphrase { retry, .. } => format!("passphrase retry={retry}"),
            SecretQuestion::Password { account, retry } => format!("password {account} retry={retry}"),
            SecretQuestion::Server { prompt, .. } => format!("server {prompt}"),
        });
        let a = self.secrets.lock().unwrap().pop().flatten().map(Secret::new);
        reply(a)
    }
}

impl Answers {
    fn with(host_key: &[HostKeyAnswer], secrets: &[Option<&str>]) -> Arc<Answers> {
        let a = Answers::default();
        // popped from the end: stored reversed
        *a.host_key.lock().unwrap() = host_key.iter().rev().copied().collect();
        *a.secrets.lock().unwrap() = secrets.iter().rev().map(|s| s.map(str::to_string)).collect();
        Arc::new(a)
    }
    fn asked(&self) -> Vec<String> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
}

struct Temp(PathBuf);

impl Temp {
    fn new(tag: &str) -> Temp {
        let p = std::env::temp_dir().join(format!("leaf-remote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Temp(p)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(dir: &Path) -> Config {
    Config {
        known_hosts: Some(dir.join("known_hosts")),
        extra_known_hosts: vec![],
        config_dir: Some(dir.to_path_buf()),
        ssh_dir: None,
        agent: false,
        op_timeout: Duration::from_secs(10),
        connect_timeout: Duration::from_secs(5),
        idle_after: Duration::from_secs(600),
    }
}

fn new_key() -> PrivateKey {
    PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ed25519).unwrap()
}

fn disk_link(at: &VPath, target: &VPath) -> bool {
    let (at, target) = (LocalFs::native(at), LocalFs::native(target));
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(&target, &at).is_ok() {
            return true;
        }
        std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(&at).arg(&target).output().is_ok_and(|o| o.status.success())
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, &at).is_ok()
    }
}

// ---------------------------------------------------------------- the tests

/// Host keys end to end: unknown asked (cancel refuses, "trust and remember" writes known_hosts), known not asked,
/// changed refused without a question.
#[test]
fn host_key_unknown_known_changed() {
    let tmp = Temp::new("hostkey");
    let served = Temp::new("hostkey-served");
    let password = format!("pw-{}", std::process::id());
    let server = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    let root = server.addr("/");

    // cancel: refused, nothing written
    let ans = Answers::with(&[HostKeyAnswer::Cancel], &[]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    assert_eq!(fs.stat(&root).err(), Some(FsError::Cancelled));
    assert_eq!(ans.asked(), [format!("host key [127.0.0.1]:{}", server.port)]);
    assert!(!tmp.0.join("known_hosts").exists());

    // trust and remember, then a password
    let ans = Answers::with(&[HostKeyAnswer::TrustAndRemember], &[Some(&password)]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    assert!(fs.stat(&root).unwrap().is_dir());
    assert_eq!(ans.asked(), [format!("host key [127.0.0.1]:{}", server.port), format!("password {USER}@127.0.0.1 retry=false")]);
    let kh = std::fs::read_to_string(tmp.0.join("known_hosts")).unwrap();
    assert!(kh.starts_with(&format!("[127.0.0.1]:{} ssh-ed25519 ", server.port)), "{kh}");
    // the connection is shared: no second login for the next call
    let logins = server.logins.load(Ordering::Relaxed);
    fs.stat(&root).unwrap();
    assert_eq!(server.logins.load(Ordering::Relaxed), logins);

    // a new run of leaf: the key is known now, only the password is asked
    let ans = Answers::with(&[], &[Some(&password)]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    fs.stat(&root).unwrap();
    assert_eq!(ans.asked(), [format!("password {USER}@127.0.0.1 retry=false")]);
    drop(server);

    // the same address with another host key: refused, and the user is not asked
    let other = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    std::fs::write(tmp.0.join("known_hosts"), kh.replace(&format!("]:{} ", root.port().unwrap()), &format!("]:{} ", other.port))).unwrap();
    let ans = Answers::with(&[HostKeyAnswer::TrustOnce], &[Some(&password)]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    let e = fs.stat(&other.addr("/")).unwrap_err();
    assert!(matches!(&e, FsError::Other(m) if m.contains("has changed") && m.contains("does not connect")), "{e:?}");
    assert!(ans.asked().is_empty(), "a changed key is never asked about");
    let _ = other.host_key.public_key();
}

/// Login with a key file from hosts.toml (passphrase asked, a wrong one asked again), a wrong password asked again;
/// then the read-only semantics suite, and a download through the generic copy with cancel.
#[test]
fn login_suite_and_download() {
    let tmp = Temp::new("login");
    let served = Temp::new("login-served");
    let password = format!("pw-{}-{}", std::process::id(), 7);
    let client_key = new_key();
    let server = TestServer::start(&served.0, &password, client_key.public_key().clone(), new_key());
    let trust = format!(
        "{}\n",
        crate::known_hosts::line_for("127.0.0.1", server.port, server.host_key.public_key()).unwrap()
    );
    std::fs::write(tmp.0.join("known_hosts"), &trust).unwrap();

    // a wrong password first, then the right one
    let ans = Answers::with(&[], &[Some("wrong"), Some(&password)]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    assert!(fs.stat(&server.addr("/")).unwrap().is_dir());
    assert_eq!(ans.asked(), [format!("password {USER}@127.0.0.1 retry=false"), format!("password {USER}@127.0.0.1 retry=true")]);
    // the password stays in memory for this run only: a dropped connection logs in again without asking
    assert_eq!(fs.home(&server.addr("/")).unwrap().path(), "/home/tester");

    // a password dialog cancelled: Cancelled
    let ans = Answers::with(&[], &[None]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    assert_eq!(fs.stat(&server.addr("/")).err(), Some(FsError::Cancelled));

    // the key file of a saved host, encrypted: a wrong passphrase, then the right one; no password asked
    let pass = format!("phrase-{}", std::process::id());
    let enc = client_key.encrypt(&mut russh::keys::key::safe_rng(), pass.as_bytes()).unwrap();
    let key_path = tmp.0.join("test_key");
    std::fs::write(&key_path, enc.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap().as_bytes()).unwrap();
    let mut h = rf_core::hosts::Host::new("test", "127.0.0.1");
    h.port = Some(server.port);
    h.user = Some(USER.into());
    h.key_file = Some(key_path.display().to_string());
    rf_core::hosts::Hosts { hosts: vec![h] }.save(&tmp.0).unwrap();
    let ans = Answers::with(&[], &[Some("not it"), Some(&pass)]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    // the saved host gives the user too
    let no_user = VPath::parse(&format!("sftp://127.0.0.1:{}/", server.port)).unwrap();
    assert!(fs.stat(&no_user).unwrap().is_dir());
    assert_eq!(ans.asked(), ["passphrase retry=false", "passphrase retry=true"]);

    // the shared read-only suite: the tree is made on the served folder through LocalFs
    let local_root = LocalFs::address(&served.0);
    leaf_vfs::suite::read_only(&fs, &server.addr("/"), &LocalFs, &local_root, &disk_link);
    assert_eq!(fs.free_space(&server.addr("/")), Ok(None), "no statvfs extension on this server");

    // download through the generic copy: cancel half way leaves no partial file; then the whole file
    std::fs::write(served.0.join("big.bin"), vec![5u8; 400_000]).unwrap();
    let dest = Temp::new("login-dest");
    let to = LocalFs::address(&dest.0).join("big.bin");
    let chunks = AtomicUsize::new(0);
    let cancelled = || chunks.load(Ordering::Relaxed) >= 3;
    let mut progress = |_: &leaf_vfs::copy::Progress| {
        chunks.fetch_add(1, Ordering::Relaxed);
    };
    let mut conflict = |_: &VPath, _: &leaf_vfs::Meta| Conflict::Cancel;
    let mut c = Copy::new(&fs, &LocalFs, &cancelled, &mut progress, &mut conflict);
    c.opts.chunk = 16 * 1024;
    assert_eq!(c.run(&server.addr("/big.bin"), &to), Err(FsError::Cancelled));
    assert!(!dest.0.join("big.bin").exists());
    assert!(!dest.0.join(part_name("big.bin")).exists(), "the partial file is removed");
    let mut progress = |_: &leaf_vfs::copy::Progress| {};
    let r = Copy::new(&fs, &LocalFs, &|| false, &mut progress, &mut conflict).run(&server.addr("/big.bin"), &to).unwrap();
    assert_eq!((r.files, r.bytes), (1, 400_000));
    assert_eq!(std::fs::read(dest.0.join("big.bin")).unwrap(), vec![5u8; 400_000]);

    // the server goes away: Offline, not a hang
    drop(server);
    assert!(matches!(fs.stat(&no_user), Err(FsError::Offline(_))), "{:?}", fs.stat(&no_user));
}

/// A logged-in `SftpFs` on a server whose key is in known_hosts (the password is answered).
fn trusted_fs(tmp: &Path, server: &TestServer, password: &str) -> (SftpFs, Arc<Answers>) {
    let line = crate::known_hosts::line_for("127.0.0.1", server.port, server.host_key.public_key()).unwrap();
    std::fs::write(tmp.join("known_hosts"), format!("{line}\n")).unwrap();
    let ans = Answers::with(&[], &[Some(password)]);
    (SftpFs::new(config(tmp), ans.clone()), ans)
}

/// Must-fix 1: a server's names never leave the target folder. The listing leaves out `../..`, `..\..`, `a/b`, `""` and
/// `..`; the copy refuses `CON`, `x:y`, `D:evil.txt`, `trailing.` (reported, never written) and writes only what is safe, all
/// inside the target; the fetch refuses an item listed as a file that is a folder when it is fetched.
#[test]
fn hostile_names_stay_inside() {
    let tmp = Temp::new("names");
    let served = Temp::new("names-served");
    std::fs::write(served.0.join("payload"), b"bytes").unwrap();
    let password = format!("pw-{}-names", std::process::id());
    let server = TestServer::start_with(&served.0, &password, new_key().public_key().clone(), vec![new_key()], true);
    let (fs, _) = trusted_fs(&tmp.0, &server, &password);
    let pics = server.addr("/pics");

    let mut listed = Vec::new();
    fs.list(&pics, 100, &|| false, &mut |b| listed.extend(b.into_iter().map(|e| e.name.to_string()))).unwrap();
    listed.sort();
    let mut want = vec!["CON", "D:evil.txt", "invoice.pdf", "ok.txt", "trailing.", "x:y.txt"];
    want.sort();
    assert_eq!(listed, want, "names that are not one part of an address are left out");

    // the download goes into dest/sub/pics; nothing may appear anywhere else under dest
    let dest = Temp::new("names-dest");
    let sub = dest.0.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let mut progress = |_: &leaf_vfs::copy::Progress| {};
    let mut conflict = |_: &VPath, _: &leaf_vfs::Meta| Conflict::Replace;
    let r = Copy::new(&fs, &LocalFs, &|| false, &mut progress, &mut conflict).run(&pics, &LocalFs::address(&sub.join("pics"))).unwrap();
    let mut refused: Vec<&str> = r.refused.iter().map(|(_, n, _)| n.as_str()).collect();
    refused.sort();
    let mut want_refused = vec!["D:evil.txt", "x:y.txt"];
    if cfg!(windows) {
        want_refused.extend(["CON", "trailing."]);
    }
    want_refused.sort();
    assert_eq!(refused, want_refused);
    let mut all = Vec::new();
    let mut stack = vec![dest.0.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            all.push(p.strip_prefix(&dest.0).unwrap().to_string_lossy().replace('\\', "/"));
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    all.sort();
    let mut want_files = vec!["sub".to_string(), "sub/pics".into(), "sub/pics/invoice.pdf".into(), "sub/pics/ok.txt".into()];
    if !cfg!(windows) {
        want_files.extend(["sub/pics/CON".to_string(), "sub/pics/trailing.".into()]);
    }
    want_files.sort();
    assert_eq!(all, want_files, "only the safe names arrive, all inside the target");
    assert_eq!(std::fs::read(sub.join("pics").join("ok.txt")).unwrap(), b"bytes");

    // the fetch (open, leaf-look) takes one regular file only: a "file" that is a folder now is refused, nothing is written
    let cache = tmp.0.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let e = leaf_vfs::copy::fetch_file(&fs, &pics.join("invoice.pdf"), &cache.join("invoice.pdf"), 1 << 20, None, &|| false, &mut |_, _| {}).unwrap_err();
    assert!(matches!(&e, FsError::Other(m) if m.contains("not a regular file")), "{e:?}");
    assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0);
    // a real file is fetched, and the cap counts the bytes that arrive
    leaf_vfs::copy::fetch_file(&fs, &pics.join("ok.txt"), &cache.join("ok.txt"), 1 << 20, None, &|| false, &mut |_, _| {}).unwrap();
    assert_eq!(std::fs::read(cache.join("ok.txt")).unwrap(), b"bytes");
}
