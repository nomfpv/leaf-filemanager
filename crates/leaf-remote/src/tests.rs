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

/// An SFTP server over a folder (writes as OpenSSH does them; read-only when hostile) (directory parts of three names, to test the streaming).
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
        let mut v = Version::new();
        v.extensions.insert(crate::conn::POSIX_RENAME.into(), "1".into());
        Ok(v)
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
        if self.hostile && (path == "/loop" || path.starts_with("/loop/")) {
            // a folder that contains itself, twice over: `a` without end, and `""` (the same folder again)
            let files = vec![File::new("a", hostile_attrs(true, 0)), File::new("", hostile_attrs(true, 0))];
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
        let writes = pflags.intersects(OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::APPEND);
        if writes && self.hostile {
            return Err(StatusCode::PermissionDenied);
        }
        let real = if self.hostile && filename.starts_with("/pics/") { self.root.join("payload") } else { self.local(&filename) };
        let mut o = std::fs::OpenOptions::new();
        o.read(pflags.contains(OpenFlags::READ) || !writes).write(pflags.contains(OpenFlags::WRITE)).append(pflags.contains(OpenFlags::APPEND));
        if pflags.contains(OpenFlags::CREATE) {
            if pflags.contains(OpenFlags::EXCLUDE) {
                o.create_new(true);
            } else {
                o.create(true);
            }
        }
        o.truncate(pflags.contains(OpenFlags::TRUNCATE));
        let f = o.open(real).map_err(io_status)?;
        Ok(Handle { id, handle: self.handle(Open::File(f)) })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        use std::io::{Seek, SeekFrom, Write};
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        f.seek(SeekFrom::Start(offset)).map_err(io_status)?;
        f.write_all(&data).map_err(io_status)?;
        Ok(ok(id))
    }

    async fn fsetstat(&mut self, id: u32, handle: String, attrs: FileAttributes) -> Result<Status, Self::Error> {
        let Some(Open::File(f)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        if let Some(size) = attrs.size {
            f.set_len(size).map_err(io_status)?;
        }
        Ok(ok(id))
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> Result<Status, Self::Error> {
        if let Some(t) = attrs.mtime {
            let f = std::fs::OpenOptions::new().write(true).open(self.local(&path)).map_err(io_status)?;
            f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(t as u64)).map_err(io_status)?;
        }
        Ok(ok(id))
    }

    /// As OpenSSH: a link is unlinked (a folder link too), a folder is refused.
    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let p = self.local(&filename);
        let m = std::fs::symlink_metadata(&p).map_err(io_status)?;
        if m.file_type().is_symlink() {
            std::fs::remove_file(&p).or_else(|_| std::fs::remove_dir(&p)).map_err(io_status)?;
        } else if m.is_dir() {
            return Err(StatusCode::Failure);
        } else {
            std::fs::remove_file(&p).map_err(io_status)?;
        }
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let p = self.local(&path);
        if std::fs::symlink_metadata(&p).map_err(io_status)?.file_type().is_symlink() {
            return Err(StatusCode::Failure);
        }
        std::fs::remove_dir(&p).map_err(|_| StatusCode::Failure)?;
        Ok(ok(id))
    }

    async fn mkdir(&mut self, id: u32, path: String, _attrs: FileAttributes) -> Result<Status, Self::Error> {
        std::fs::create_dir(self.local(&path)).map_err(|_| StatusCode::Failure)?;
        Ok(ok(id))
    }

    /// As OpenSSH: a plain rename never replaces.
    async fn rename(&mut self, id: u32, oldpath: String, newpath: String) -> Result<Status, Self::Error> {
        let (a, b) = (self.local(&oldpath), self.local(&newpath));
        if std::fs::symlink_metadata(&b).is_ok() {
            return Err(StatusCode::Failure);
        }
        std::fs::rename(a, b).map_err(io_status)?;
        Ok(ok(id))
    }

    /// `posix-rename@openssh.com`: replaces a file at the new name.
    async fn extended(&mut self, id: u32, request: String, data: Vec<u8>) -> Result<russh_sftp::protocol::Packet, Self::Error> {
        if request != crate::conn::POSIX_RENAME {
            return Err(StatusCode::OpUnsupported);
        }
        let mut parts = Vec::new();
        let mut d = &data[..];
        while d.len() >= 4 {
            let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) as usize;
            let s = d.get(4..4 + n).ok_or(StatusCode::BadMessage)?;
            parts.push(String::from_utf8(s.to_vec()).map_err(|_| StatusCode::BadMessage)?);
            d = &d[4 + n..];
        }
        let [a, b] = parts.as_slice() else { return Err(StatusCode::BadMessage) };
        std::fs::rename(self.local(a), self.local(b)).map_err(io_status)?;
        Ok(russh_sftp::protocol::Packet::Status(ok(id)))
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
        if self.hostile && (path == "/loop" || path.starts_with("/loop/")) {
            return Ok(Attrs { id, attrs: hostile_attrs(true, 0) });
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
    /// The "Remember" tick of the secrets typed.
    remember: std::sync::atomic::AtomicBool,
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
            SecretQuestion::Password { account, retry, .. } => format!("password {account} retry={retry}"),
            SecretQuestion::Server { prompt, .. } => format!("server {prompt}"),
        });
        let remember = self.remember.load(Ordering::Relaxed);
        let a = self.secrets.lock().unwrap().pop().flatten().map(|s| Secret::new(s).remembered(remember));
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
        vault: crate::secrets::Vault::none(),
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
    assert!(!names_in(&dest.0).iter().any(|n| n.ends_with(".leaf-part")), "the partial file is removed");
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

/// Must-fix 3: a folder that lists itself (`""`) or a child without end (`a/a/a/...`) ends in an error at the depth cap, not in
/// a stack overflow; the listing never hands on the `""` entry.
#[test]
fn self_containing_folder_ends_in_an_error() {
    let tmp = Temp::new("loop");
    let served = Temp::new("loop-served");
    let password = format!("pw-{}-loop", std::process::id());
    let server = TestServer::start_with(&served.0, &password, new_key().public_key().clone(), vec![new_key()], true);
    let (fs, _) = trusted_fs(&tmp.0, &server, &password);
    let lp = server.addr("/loop");
    let mut listed = Vec::new();
    fs.list(&lp, 10, &|| false, &mut |b| listed.extend(b.into_iter().map(|e| e.name.to_string()))).unwrap();
    assert_eq!(listed, ["a"]);
    let dest = Temp::new("loop-dest");
    let mut progress = |_: &leaf_vfs::copy::Progress| {};
    let mut conflict = |_: &VPath, _: &leaf_vfs::Meta| Conflict::Skip;
    // on a thread with a small stack: the cap must hold long before the stack would run out
    let to = LocalFs::address(&dest.0.join("loop"));
    let r = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn_scoped(s, || Copy::new(&fs, &LocalFs, &|| false, &mut progress, &mut conflict).run(&lp, &to))
            .unwrap()
            .join()
            .unwrap()
    });
    assert_eq!(r, Err(leaf_vfs::copy::too_deep()));
}

/// Must-fix 4: no downgrade by key type. known_hosts holds the server's ECDSA key only: leaf asks for ECDSA first, so a
/// server that also has Ed25519 (russh's first choice) connects without a question. A server that then shows only an Ed25519
/// key (an attacker without the known key) is refused like a changed key, and nobody is asked.
#[test]
fn other_key_type_is_refused_not_asked() {
    let tmp = Temp::new("ktype");
    let served = Temp::new("ktype-served");
    let password = format!("pw-{}-ktype", std::process::id());
    let ec = PrivateKey::random(&mut russh::keys::key::safe_rng(), Algorithm::Ecdsa { curve: russh::keys::EcdsaCurve::NistP256 }).unwrap();
    let ed = new_key();
    let server = TestServer::start_with(&served.0, &password, new_key().public_key().clone(), vec![ed.clone(), ec.clone()], false);
    let line = crate::known_hosts::line_for("127.0.0.1", server.port, ec.public_key()).unwrap();
    std::fs::write(tmp.0.join("known_hosts"), format!("{line}\n")).unwrap();

    let ans = Answers::with(&[HostKeyAnswer::TrustAndRemember], &[Some(&password)]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    assert!(fs.stat(&server.addr("/")).unwrap().is_dir());
    assert_eq!(ans.asked(), [format!("password {USER}@127.0.0.1 retry=false")], "the known ECDSA key was negotiated");

    *server.keys.lock().unwrap() = vec![ed];
    let ans = Answers::with(&[HostKeyAnswer::TrustAndRemember], &[Some(&password)]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    let e = fs.stat(&server.addr("/")).unwrap_err();
    assert!(matches!(&e, FsError::Other(m) if m.contains("another type") && m.contains("does not connect")), "{e:?}");
    assert!(ans.asked().is_empty(), "no trust dialog, no password");
    assert_eq!(std::fs::read_to_string(tmp.0.join("known_hosts")).unwrap(), format!("{line}\n"), "nothing written");
}

/// Must-fix 5: secrets are bound to the host key they were typed for. After "Trust once" and a password, a reconnect to the
/// same key asks nothing; a reconnect that shows another key is refused as changed (no dialog, the password not sent) and
/// the password is forgotten: back on the trusted key, it is asked again.
#[test]
fn another_key_after_trust_once_is_refused_and_secrets_forgotten() {
    let tmp = Temp::new("bound");
    let served = Temp::new("bound-served");
    let password = format!("pw-{}-bound", std::process::id());
    let k1 = new_key();
    let server = TestServer::start_with(&served.0, &password, new_key().public_key().clone(), vec![k1.clone()], false);
    let root = server.addr("/");
    let ans = Answers::with(&[HostKeyAnswer::TrustOnce, HostKeyAnswer::TrustOnce], &[Some(&password), Some(&password)]);
    let fs = SftpFs::new(config(&tmp.0), ans.clone());
    fs.stat(&root).unwrap();
    assert_eq!(ans.asked(), [format!("host key [127.0.0.1]:{}", server.port), format!("password {USER}@127.0.0.1 retry=false")]);

    // the line drops; the same key again: nothing asked, the kept password logs in
    fs.drop_connections();
    let logins = server.logins.load(Ordering::Relaxed);
    fs.stat(&root).unwrap();
    assert!(ans.asked().is_empty());
    assert_eq!(server.logins.load(Ordering::Relaxed), logins + 1);

    // someone else answers with another key: refused without a question, and no login happens
    *server.keys.lock().unwrap() = vec![new_key()];
    fs.drop_connections();
    let e = fs.stat(&root).unwrap_err();
    assert!(matches!(&e, FsError::Other(m) if m.contains("has changed") && m.contains("does not connect")), "{e:?}");
    assert!(ans.asked().is_empty(), "no trust dialog, no password dialog");
    assert_eq!(server.logins.load(Ordering::Relaxed), logins + 1, "the password was not sent");

    // the real key again: still trusted for this run, but the password was forgotten and is asked again
    *server.keys.lock().unwrap() = vec![k1];
    fs.drop_connections();
    fs.stat(&root).unwrap();
    assert_eq!(ans.asked(), [format!("password {USER}@127.0.0.1 retry=false")]);
}

/// The files directly in `dir` (names only, sorted).
fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

/// A recursive remove on the server unlinks a link to a folder outside (inside the tree and as the item itself) and never
/// goes through it: what the link points to is untouched.
#[test]
fn recursive_remove_does_not_follow_a_link() {
    let tmp = Temp::new("rmlink");
    let served = Temp::new("rmlink-served");
    let password = format!("pw-{}-rm", std::process::id());
    let server = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    let (fs, _) = trusted_fs(&tmp.0, &server, &password);
    std::fs::create_dir_all(served.0.join("outside")).unwrap();
    std::fs::write(served.0.join("outside").join("keep.txt"), b"keep").unwrap();
    std::fs::create_dir_all(served.0.join("tree").join("sub")).unwrap();
    std::fs::write(served.0.join("tree").join("a.txt"), b"a").unwrap();
    std::fs::write(served.0.join("tree").join("sub").join("b.txt"), b"b").unwrap();
    let local = LocalFs::address(&served.0);
    assert!(disk_link(&local.join("tree").join("sub").join("out"), &local.join("outside")), "a link on this PC");
    assert!(disk_link(&local.join("top"), &local.join("outside")));
    assert_eq!(fs.stat(&server.addr("/tree/sub/out")).unwrap().kind, leaf_vfs::Kind::Link);

    assert_eq!(fs.remove(&server.addr("/tree"), false), Err(FsError::NotEmpty));
    fs.remove(&server.addr("/tree"), true).unwrap();
    fs.remove(&server.addr("/top"), true).unwrap();
    assert_eq!(names_in(&served.0), ["outside"]);
    assert_eq!(names_in(&served.0.join("outside")), ["keep.txt"]);
    assert_eq!(std::fs::read(served.0.join("outside").join("keep.txt")).unwrap(), b"keep");
    assert_eq!(fs.remove(&server.addr("/"), true), Err(FsError::Denied));
}

/// An upload goes to a temporary name and takes the real one only at the end: a whole one replaces the file (keeping
/// nothing else behind); one cut off half way (the server gone) leaves the original file exactly as it was.
#[test]
fn a_cut_upload_leaves_the_original_intact() {
    use std::io::Write;
    let tmp = Temp::new("cut");
    let served = Temp::new("cut-served");
    let password = format!("pw-{}-cut", std::process::id());
    let server = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    let (fs, _) = trusted_fs(&tmp.0, &server, &password);
    let doc = served.0.join("doc.txt");
    std::fs::write(&doc, b"version one").unwrap();

    // a whole upload replaces it
    let mut w = fs.open_write(&server.addr("/doc.txt"), leaf_vfs::WriteOpts::REPLACE).unwrap();
    w.write_all(&vec![2u8; 150_000]).unwrap();
    w.finish().unwrap();
    assert_eq!(std::fs::read(&doc).unwrap(), vec![2u8; 150_000]);
    assert_eq!(names_in(&served.0), ["doc.txt"], "no temporary file left");
    assert!(matches!(fs.open_write(&server.addr("/doc.txt"), leaf_vfs::WriteOpts::CREATE_NEW), Err(FsError::Exists)));

    // cut off: the server goes away in the middle
    let mut w = fs.open_write(&server.addr("/doc.txt"), leaf_vfs::WriteOpts::REPLACE).unwrap();
    w.write_all(&vec![3u8; 200_000]).unwrap();
    drop(server);
    let r = w.write_all(&vec![3u8; 600_000]).map_err(FsError::from).and_then(|_| w.finish());
    assert!(r.is_err(), "{r:?}");
    assert_eq!(std::fs::read(&doc).unwrap(), vec![2u8; 150_000], "the original is intact");
    assert!(names_in(&served.0).iter().all(|n| n == "doc.txt" || n.contains(".leaf-up-")), "{:?}", names_in(&served.0));
}

/// H10: a save is refused (nothing sent, nothing renamed) when the server's file changed since it was fetched; a save over
/// the fetched version keeps that version as `name.leaf-bak` (one backup, replaced by the next save); "save as copy" never
/// writes over anything.
#[test]
fn edit_refuses_when_the_server_copy_changed() {
    use crate::edit::{Fetched, SaveError, save_as_copy_with, save_back_with};
    let tmp = Temp::new("edit");
    let served = Temp::new("edit-served");
    let password = format!("pw-{}-edit", std::process::id());
    let server = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    let (fs, _) = trusted_fs(&tmp.0, &server, &password);
    let at = server.addr("/notes.txt");
    std::fs::write(served.0.join("notes.txt"), b"one").unwrap();
    let fetched = Fetched::of(&fs.stat(&at).unwrap());
    let local = tmp.0.join("notes.txt");
    std::fs::write(&local, b"my edit").unwrap();

    // someone else changes it on the server: refused, the server's file and folder untouched
    std::fs::write(served.0.join("notes.txt"), b"their change").unwrap();
    let e = save_back_with(&fs, &at, &local, &fetched, false, &|| false).unwrap_err();
    assert!(matches!(e, SaveError::Changed(f) if f.size == 12), "{e:?}");
    assert_eq!(std::fs::read(served.0.join("notes.txt")).unwrap(), b"their change");
    assert_eq!(names_in(&served.0), ["notes.txt"]);

    // saved as a copy beside it instead
    let copy = save_as_copy_with(&fs, &at, &local, &|| false).unwrap();
    assert_eq!(copy.name(), "notes (edited).txt");
    assert_eq!(std::fs::read(served.0.join("notes (edited).txt")).unwrap(), b"my edit");

    // against the version that is there now: uploaded, the old one kept as the backup; the next save replaces the backup
    let now = Fetched::of(&fs.stat(&at).unwrap());
    let after = save_back_with(&fs, &at, &local, &now, false, &|| false).unwrap();
    assert_eq!(std::fs::read(served.0.join("notes.txt")).unwrap(), b"my edit");
    assert_eq!(std::fs::read(served.0.join("notes.txt.leaf-bak")).unwrap(), b"their change");
    std::fs::write(&local, b"second edit").unwrap();
    save_back_with(&fs, &at, &local, &after, false, &|| false).unwrap();
    assert_eq!(std::fs::read(served.0.join("notes.txt")).unwrap(), b"second edit");
    assert_eq!(std::fs::read(served.0.join("notes.txt.leaf-bak")).unwrap(), b"my edit");
    assert_eq!(names_in(&served.0), ["notes (edited).txt", "notes.txt", "notes.txt.leaf-bak"]);
}

/// H10 after a restart: an editing copy that differs from the server's file is moved aside (never overwritten or cut); one
/// that matches the server's size and time is used as it is.
#[test]
fn unsent_edit_copy_is_set_aside() {
    use crate::edit::{Leftover, set_aside_unsent};
    let tmp = Temp::new("unsent");
    let local = tmp.0.join("notes.txt");
    let server = leaf_vfs::Meta { kind: leaf_vfs::Kind::File, size: 3, modified_ms: 1_700_000_000_000, created_ms: 0, mode: None };
    assert_eq!(set_aside_unsent(&local, &server).unwrap(), Leftover::None);
    std::fs::write(&local, b"one").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&local).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)).unwrap();
    drop(f);
    assert_eq!(set_aside_unsent(&local, &server).unwrap(), Leftover::Same);
    std::fs::write(&local, b"my unsent edit").unwrap();
    let Leftover::SetAside(p) = set_aside_unsent(&local, &server).unwrap() else { panic!("not set aside") };
    assert!(!local.exists());
    assert_eq!(std::fs::read(&p).unwrap(), b"my unsent edit");
    let n = p.file_name().unwrap().to_string_lossy().into_owned();
    assert!(n.starts_with("notes (unsent ") && n.ends_with(").txt"), "{n}");
}

/// Two uploads of the same file at once never share a part file: each gets its own name, created exclusively; a part name
/// that is there already (another job's, or something planted) is refused, never truncated.
#[test]
fn two_writers_never_share_a_part_file() {
    use std::io::Write;
    let tmp = Temp::new("parts");
    let served = Temp::new("parts-served");
    let password = format!("pw-{}-parts", std::process::id());
    let server = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    let (fs, _) = trusted_fs(&tmp.0, &server, &password);
    let (a, b) = (server.addr("/").join(&part_name("x.bin")), server.addr("/").join(&part_name("x.bin")));
    assert_ne!(a, b);
    let mut wa = fs.open_write(&a, leaf_vfs::WriteOpts::REPLACE).unwrap();
    let mut wb = fs.open_write(&b, leaf_vfs::WriteOpts::REPLACE).unwrap();
    wa.write_all(&vec![1u8; 100_000]).unwrap();
    wb.write_all(&vec![2u8; 70_000]).unwrap();
    wa.finish().unwrap();
    wb.finish().unwrap();
    assert_eq!(std::fs::read(served.0.join(a.name())).unwrap(), vec![1u8; 100_000]);
    assert_eq!(std::fs::read(served.0.join(b.name())).unwrap(), vec![2u8; 70_000]);
    assert!(matches!(fs.open_write(&a, leaf_vfs::WriteOpts::REPLACE), Err(FsError::Exists)), "an existing part file is not reused");
    assert_eq!(std::fs::read(served.0.join(a.name())).unwrap(), vec![1u8; 100_000], "and not truncated");
}

/// A saved password the server refuses is deleted from the keyring and asked again (a fake keyring in memory: the real
/// one is never touched); one typed with "Remember" is saved after the login worked and used without a question next time.
#[test]
fn a_refused_saved_password_is_deleted_and_asked_again() {
    use crate::secrets::{MemoryStore, SecretId, Vault};
    let tmp = Temp::new("saved");
    let served = Temp::new("saved-served");
    let password = format!("pw-{}-saved", std::process::id());
    let server = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    let line = crate::known_hosts::line_for("127.0.0.1", server.port, server.host_key.public_key()).unwrap();
    std::fs::write(tmp.0.join("known_hosts"), format!("{line}\n")).unwrap();
    let store = Arc::new(MemoryStore::default());
    let id = SecretId::password("sftp", USER, "127.0.0.1", server.port);
    store.saved.lock().unwrap().insert(id.clone(), "an-old-password".into());
    let mut cfg = config(&tmp.0);
    cfg.vault = Vault::new(store.clone());
    let saved = || store.saved.lock().unwrap().get(&id).cloned();

    // the saved one is tried first (no question), refused, deleted; then asked; typed without "Remember": nothing saved
    let ans = Answers::with(&[], &[Some(&password)]);
    let fs = SftpFs::new(cfg.clone(), ans.clone());
    assert!(fs.stat(&server.addr("/")).unwrap().is_dir());
    assert_eq!(ans.asked(), [format!("password {USER}@127.0.0.1 retry=true")]);
    assert_eq!(saved(), None, "the refused password is gone from the keyring");

    // typed with "Remember": saved once the server took it
    let ans = Answers::with(&[], &[Some(&password)]);
    ans.remember.store(true, Ordering::Relaxed);
    let fs = SftpFs::new(cfg.clone(), ans.clone());
    fs.stat(&server.addr("/")).unwrap();
    assert_eq!(ans.asked(), [format!("password {USER}@127.0.0.1 retry=false")]);
    assert_eq!(saved().as_deref(), Some(password.as_str()));

    // a new run of leaf: used without asking
    let ans = Answers::with(&[], &[]);
    let fs = SftpFs::new(cfg, ans.clone());
    fs.stat(&server.addr("/")).unwrap();
    assert!(ans.asked().is_empty(), "{:?}", ans.asked());
}

/// B7: a delete on the server moves the item into `.leaf-trash/<time>/<its path below the login folder>` (a rename: the
/// bytes stay), Undo moves it back to where it was; the login folder itself is refused; something already in the trash is
/// not moved again.
#[test]
fn remote_delete_lands_in_leaf_trash_and_undo_restores_it() {
    use crate::trash::{self, TrashError, Trashed};
    let tmp = Temp::new("trash");
    let served = Temp::new("trash-served");
    let password = format!("pw-{}-trash", std::process::id());
    let server = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    let (fs, _) = trusted_fs(&tmp.0, &server, &password);
    let home_dir = served.0.join("home").join("tester");
    std::fs::create_dir_all(home_dir.join("docs").join("sub")).unwrap();
    std::fs::write(home_dir.join("docs").join("a.txt"), b"alpha").unwrap();
    std::fs::write(home_dir.join("docs").join("sub").join("b.txt"), b"beta").unwrap();
    let home = fs.home(&server.addr("/")).unwrap();
    assert_eq!(home.path(), "/home/tester");

    // a file (by its /~ address) and a folder, in one delete
    let stamp = "2026-11-21_03-00-00.000";
    let a = trash::move_to_trash(&fs, &home, &server.addr("/~/docs/a.txt"), stamp).unwrap();
    let sub = trash::move_to_trash(&fs, &home, &server.addr("/home/tester/docs/sub"), stamp).unwrap();
    assert_eq!(a.path(), format!("/home/tester/.leaf-trash/{stamp}/docs/a.txt"));
    let in_trash = home_dir.join(".leaf-trash").join(stamp).join("docs");
    assert_eq!(std::fs::read(in_trash.join("a.txt")).unwrap(), b"alpha");
    assert_eq!(std::fs::read(in_trash.join("sub").join("b.txt")).unwrap(), b"beta");
    assert_eq!(names_in(&home_dir.join("docs")), Vec::<String>::new(), "gone from where they were");
    assert_eq!(trash::usage(&fs, &home, &|| false).unwrap(), (2, 9));

    // Undo: both back, the trash's folders of this delete gone
    trash::restore(&fs, &home, &Trashed { original: server.addr("/home/tester/docs/sub"), in_trash: sub }).unwrap();
    trash::restore(&fs, &home, &Trashed { original: server.addr("/~/docs/a.txt"), in_trash: a.clone() }).unwrap();
    assert_eq!(std::fs::read(home_dir.join("docs").join("a.txt")).unwrap(), b"alpha");
    assert_eq!(std::fs::read(home_dir.join("docs").join("sub").join("b.txt")).unwrap(), b"beta");
    assert_eq!(names_in(&home_dir.join(".leaf-trash")), Vec::<String>::new());
    // undoing twice: it is not in the trash any more, nothing is touched
    assert!(trash::restore(&fs, &home, &Trashed { original: server.addr("/~/docs/a.txt"), in_trash: a }).is_err());
    assert_eq!(std::fs::read(home_dir.join("docs").join("a.txt")).unwrap(), b"alpha");

    // never the login folder or above; something in the trash is deleted for good instead (after asking)
    assert_eq!(trash::move_to_trash(&fs, &home, &server.addr("/~"), stamp), Err(TrashError::Fs(FsError::Denied)));
    assert_eq!(trash::move_to_trash(&fs, &home, &server.addr("/home"), stamp), Err(TrashError::Fs(FsError::Denied)));
    let t = trash::move_to_trash(&fs, &home, &server.addr("/~/docs/a.txt"), stamp).unwrap();
    assert!(matches!(trash::move_to_trash(&fs, &home, &t, "later"), Err(TrashError::CannotMove(_))));
}

/// "Empty server trash" removes what is in `.leaf-trash` and nothing else: the files beside it stay, and a link inside the
/// trash is unlinked without touching what it points to.
#[test]
fn empty_removes_only_inside_leaf_trash() {
    use crate::trash;
    let tmp = Temp::new("empty");
    let served = Temp::new("empty-served");
    let password = format!("pw-{}-empty", std::process::id());
    let server = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    let (fs, _) = trusted_fs(&tmp.0, &server, &password);
    let home_dir = served.0.join("home").join("tester");
    std::fs::create_dir_all(home_dir.join("keep")).unwrap();
    std::fs::write(home_dir.join("keep").join("k.txt"), b"keep").unwrap();
    std::fs::write(home_dir.join("x.txt"), b"x").unwrap();
    std::fs::create_dir_all(home_dir.join("old")).unwrap();
    std::fs::write(home_dir.join("old").join("o.txt"), b"old").unwrap();
    let home = fs.home(&server.addr("/")).unwrap();
    trash::move_to_trash(&fs, &home, &server.addr("/~/x.txt"), "s1").unwrap();
    trash::move_to_trash(&fs, &home, &server.addr("/~/old"), "s2").unwrap();
    // a link in the trash to a folder outside it
    let local = LocalFs::address(&home_dir);
    let linked = disk_link(&local.join(".leaf-trash").join("s2").join("out"), &local.join("keep"));
    assert_eq!(trash::usage(&fs, &home, &|| false).unwrap().0, 2 + u64::from(linked));

    trash::empty(&fs, &home, &|| false).unwrap();
    assert!(!home_dir.join(".leaf-trash").exists(), "the trash is gone");
    assert_eq!(names_in(&home_dir), ["keep"]);
    assert_eq!(std::fs::read(home_dir.join("keep").join("k.txt")).unwrap(), b"keep", "what the link pointed to is untouched");
    assert_eq!(trash::usage(&fs, &home, &|| false).unwrap(), (0, 0));
    // an empty (absent) trash: nothing to do
    trash::empty(&fs, &home, &|| false).unwrap();
}

/// A password saved in the keyring goes only to a host key known_hosts has: a key trusted once (on this run, any key the
/// user accepts at the unknown-key question) gets the question, not the saved password; once the key is known, it is used.
#[test]
fn a_saved_password_is_not_sent_to_a_key_trusted_once() {
    use crate::secrets::{MemoryStore, SecretId, Vault};
    let tmp = Temp::new("saved-unknown");
    let served = Temp::new("saved-unknown-served");
    let password = format!("pw-{}-saved-unknown", std::process::id());
    let server = TestServer::start(&served.0, &password, new_key().public_key().clone(), new_key());
    let store = Arc::new(MemoryStore::default());
    let id = SecretId::password("sftp", USER, "127.0.0.1", server.port);
    store.saved.lock().unwrap().insert(id.clone(), password.clone());
    let mut cfg = config(&tmp.0);
    cfg.vault = Vault::new(store.clone());

    // the key is not in known_hosts: trusted once, and the password is asked although one is saved
    let ans = Answers::with(&[HostKeyAnswer::TrustOnce], &[Some(&password)]);
    let fs = SftpFs::new(cfg.clone(), ans.clone());
    assert!(fs.stat(&server.addr("/")).unwrap().is_dir());
    assert_eq!(ans.asked(), [format!("host key [127.0.0.1]:{}", server.port), format!("password {USER}@127.0.0.1 retry=false")]);
    assert_eq!(store.saved.lock().unwrap().get(&id), Some(&password), "the saved one stays");

    // known_hosts has the key now: the saved password is used without a question
    let line = crate::known_hosts::line_for("127.0.0.1", server.port, server.host_key.public_key()).unwrap();
    std::fs::write(tmp.0.join("known_hosts"), format!("{line}\n")).unwrap();
    let ans = Answers::with(&[], &[]);
    let fs = SftpFs::new(cfg, ans.clone());
    fs.stat(&server.addr("/")).unwrap();
    assert!(ans.asked().is_empty(), "{:?}", ans.asked());
}

/// "Forget" of a saved passphrase also drops the key file this run unlocked with it: the next login with that key asks for
/// the passphrase again (not logged in silently with the key kept in memory).
#[test]
fn forgetting_a_passphrase_drops_the_unlocked_key() {
    use crate::secrets::{MemoryStore, SecretId, Vault};
    let tmp = Temp::new("forget-key");
    let served = Temp::new("forget-key-served");
    let client_key = new_key();
    let server = TestServer::start(&served.0, &format!("pw-{}-unused", std::process::id()), client_key.public_key().clone(), new_key());
    let line = crate::known_hosts::line_for("127.0.0.1", server.port, server.host_key.public_key()).unwrap();
    std::fs::write(tmp.0.join("known_hosts"), format!("{line}\n")).unwrap();
    let pass = format!("phrase-{}-forget", std::process::id());
    let enc = client_key.encrypt(&mut russh::keys::key::safe_rng(), pass.as_bytes()).unwrap();
    let key_path = tmp.0.join("forget_key");
    std::fs::write(&key_path, enc.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap().as_bytes()).unwrap();
    let mut h = rf_core::hosts::Host::new("test", "127.0.0.1");
    h.port = Some(server.port);
    h.user = Some(USER.into());
    h.key_file = Some(key_path.display().to_string());
    rf_core::hosts::Hosts { hosts: vec![h] }.save(&tmp.0).unwrap();
    let store = Arc::new(MemoryStore::default());
    let mut cfg = config(&tmp.0);
    cfg.vault = Vault::new(store.clone());
    let id = SecretId::passphrase(&key_path);
    let addr = VPath::parse(&format!("sftp://127.0.0.1:{}/", server.port)).unwrap();

    // asked, ticked "Remember": saved, and the unlocked key is kept for this run
    let ans = Answers::with(&[], &[Some(&pass), Some(&pass)]);
    ans.remember.store(true, Ordering::Relaxed);
    let fs = SftpFs::new(cfg, ans.clone());
    fs.stat(&addr).unwrap();
    assert_eq!(ans.asked(), ["passphrase retry=false"]);
    assert!(store.saved.lock().unwrap().contains_key(&id));

    // Forget: out of the keyring and out of memory; the next login asks again
    cfg_forget(&fs, &store, &id);
    fs.drop_connections();
    fs.stat(&addr).unwrap();
    assert_eq!(ans.asked(), ["passphrase retry=false"]);
}

/// What `crate::forget_saved` does for a passphrase, on this test's own backend and keyring.
fn cfg_forget(fs: &SftpFs, store: &Arc<crate::secrets::MemoryStore>, id: &crate::secrets::SecretId) {
    let crate::secrets::SecretId::Passphrase { fingerprint, .. } = id else { panic!("not a passphrase") };
    fs.forget_key(fingerprint);
    crate::secrets::Vault::new(store.clone()).forget(id).unwrap();
    assert!(!store.saved.lock().unwrap().contains_key(id));
}
