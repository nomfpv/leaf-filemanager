//! `WebDavFs` against a small WebDAV server in this test process (127.0.0.1, an ephemeral port, plain http, Basic sign-in),
//! serving a tree in memory. It records every request, so the tests can say what was sent where.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use leaf_vfs::{Fs, FsError, VPath, WriteOpts};

use crate::prompt::{HostKeyAnswer, HostKeyQuestion, Prompter, Secret, SecretQuestion};
use crate::webdav::WebDavFs;

const USER: &str = "alice";
const PASSWORD: &str = "pw-Zq8!secret";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One request the server saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    /// Percent-decoded.
    path: String,
    overwrite: Option<String>,
    if_none_match: Option<String>,
    destination: Option<String>,
}

#[derive(Default)]
struct State {
    files: BTreeMap<String, Vec<u8>>,
    dirs: BTreeSet<String>,
    /// MOVE answers 403 and moves nothing.
    fail_move: bool,
    seen: Vec<Seen>,
    /// Requests that carried the right sign-in.
    signed_in: usize,
}

struct Server {
    port: u16,
    state: Arc<Mutex<State>>,
}

impl Server {
    fn seen(&self) -> Vec<Seen> {
        lock(&self.state).seen.clone()
    }
    fn add_file(&self, p: &str, c: &str) {
        lock(&self.state).files.insert(p.to_string(), c.as_bytes().to_vec());
    }
    fn file(&self, p: &str) -> Option<Vec<u8>> {
        lock(&self.state).files.get(p).cloned()
    }
}

fn unenc(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 1 {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn norm(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() { "/".into() } else { t.into() }
}

fn parent(p: &str) -> String {
    match p.rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((d, _)) => d.into(),
    }
}

fn start(dirs: &[&str], files: &[(&str, &str)]) -> Server {
    let mut st = State::default();
    st.dirs.insert("/".into());
    st.dirs.extend(dirs.iter().map(|d| d.to_string()));
    for (p, c) in files {
        st.files.insert(p.to_string(), c.as_bytes().to_vec());
    }
    let state = Arc::new(Mutex::new(st));
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let st = state.clone();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let st = st.clone();
            std::thread::spawn(move || {
                let _ = serve(c, st, port);
            });
        }
    });
    Server { port, state }
}

fn serve(c: TcpStream, st: Arc<Mutex<State>>, port: u16) -> std::io::Result<()> {
    c.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut rd = BufReader::new(c.try_clone()?);
    let mut line = String::new();
    if rd.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut it = line.split_whitespace();
    let (method, target) = (it.next().unwrap_or("").to_string(), unenc(it.next().unwrap_or("/")));
    let mut h = BTreeMap::new();
    loop {
        line.clear();
        rd.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            h.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let mut body = Vec::new();
    if h.get("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        loop {
            line.clear();
            rd.read_line(&mut line)?;
            let n = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0"), 16).unwrap_or(0);
            if n == 0 {
                // (the trailer's end)
                loop {
                    line.clear();
                    rd.read_line(&mut line)?;
                    if line.trim().is_empty() {
                        break;
                    }
                }
                break;
            }
            let mut chunk = vec![0; n];
            rd.read_exact(&mut chunk)?;
            body.extend(chunk);
            let mut crlf = [0; 2];
            rd.read_exact(&mut crlf)?;
        }
    } else if let Some(n) = h.get("content-length").and_then(|v| v.parse::<usize>().ok()) {
        body = vec![0; n];
        rd.read_exact(&mut body)?;
    }
    let mut out = c;
    let path = norm(&target);
    let mut s = lock(&st);
    let ok = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{PASSWORD}")));
    if h.get("authorization") != Some(&ok) {
        s.seen.push(Seen { method: method.clone(), path: path.clone(), overwrite: None, if_none_match: None, destination: None });
        return reply(&mut out, 401, &[("WWW-Authenticate", "Basic realm=\"leaf test\"")], b"");
    }
    s.signed_in += 1;
    let dest = h.get("destination").map(|d| norm(&unenc(d.strip_prefix(&format!("http://127.0.0.1:{port}")).unwrap_or(d))));
    s.seen.push(Seen { method: method.clone(), path: path.clone(), overwrite: h.get("overwrite").cloned(), if_none_match: h.get("if-none-match").cloned(), destination: h.get("destination").cloned() });
    let is_dir = s.dirs.contains(&path);
    let exists = is_dir || s.files.contains_key(&path);
    match method.as_str() {
        "PROPFIND" => {
            if !exists {
                return reply(&mut out, 404, &[], b"");
            }
            let one = |p: &str, dir: bool, len: usize| {
                let href = if dir && p != "/" { format!("{}/", enc(p)) } else { enc(p) };
                let kind = if dir { "<d:resourcetype><d:collection/></d:resourcetype>" } else { "<d:resourcetype/>" };
                format!(
                    "<d:response><d:href>{href}</d:href><d:propstat><d:prop>{kind}<d:getcontentlength>{len}</d:getcontentlength>\
                     <d:getlastmodified>Sun, 06 Nov 1994 08:49:37 GMT</d:getlastmodified></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
                )
            };
            let mut xml = String::from("<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">");
            xml.push_str(&one(&path, is_dir, s.files.get(&path).map_or(0, Vec::len)));
            if is_dir && h.get("depth").is_some_and(|d| d == "1") {
                for d in s.dirs.iter().filter(|d| **d != "/" && parent(d) == path) {
                    xml.push_str(&one(d, true, 0));
                }
                for (f, c) in s.files.iter().filter(|(f, _)| parent(f) == path) {
                    xml.push_str(&one(f, false, c.len()));
                }
            }
            xml.push_str("</d:multistatus>");
            reply(&mut out, 207, &[("Content-Type", "application/xml")], xml.as_bytes())
        }
        "GET" => match s.files.get(&path) {
            None => reply(&mut out, 404, &[], b""),
            Some(c) => match h.get("range").and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.strip_suffix('-')).and_then(|r| r.parse::<usize>().ok()) {
                Some(from) if from < c.len() => reply(&mut out, 206, &[("Content-Range", &format!("bytes {from}-{}/{}", c.len() - 1, c.len()))], &c[from..]),
                Some(_) => reply(&mut out, 416, &[], b""),
                None => reply(&mut out, 200, &[], c),
            },
        },
        "PUT" => {
            if !s.dirs.contains(&parent(&path)) {
                return reply(&mut out, 409, &[], b"");
            }
            if h.get("if-none-match").is_some_and(|v| v == "*") && exists {
                return reply(&mut out, 412, &[], b"");
            }
            s.files.insert(path, body);
            reply(&mut out, 201, &[], b"")
        }
        "MKCOL" => {
            if exists {
                return reply(&mut out, 405, &[], b"");
            }
            if !s.dirs.contains(&parent(&path)) {
                return reply(&mut out, 409, &[], b"");
            }
            s.dirs.insert(path);
            reply(&mut out, 201, &[], b"")
        }
        "DELETE" => {
            if !exists {
                return reply(&mut out, 404, &[], b"");
            }
            let under = format!("{}/", path.trim_end_matches('/'));
            s.files.retain(|f, _| *f != path && !f.starts_with(&under));
            s.dirs.retain(|d| *d != path && !d.starts_with(&under));
            reply(&mut out, 204, &[], b"")
        }
        "MOVE" => {
            let Some(dest) = dest else { return reply(&mut out, 400, &[], b"") };
            if !exists {
                return reply(&mut out, 404, &[], b"");
            }
            if s.fail_move {
                return reply(&mut out, 403, &[], b"");
            }
            let taken = s.dirs.contains(&dest) || s.files.contains_key(&dest);
            if taken && h.get("overwrite").is_none_or(|o| o != "T") {
                return reply(&mut out, 412, &[], b"");
            }
            if let Some(c) = s.files.remove(&path) {
                s.files.insert(dest, c);
            } else {
                s.dirs.remove(&path);
                s.dirs.insert(dest);
            }
            reply(&mut out, if taken { 204 } else { 201 }, &[], b"")
        }
        _ => reply(&mut out, 405, &[], b""),
    }
}

fn reply(out: &mut TcpStream, code: u16, headers: &[(&str, &str)], body: &[u8]) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        412 => "Precondition Failed",
        416 => "Range Not Satisfiable",
        _ => "Other",
    };
    let mut head = format!("HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    out.write_all(body)?;
    out.flush()
}

/// Answers the password question with the right password and keeps the questions.
#[derive(Default)]
struct Answers {
    asked: Mutex<Vec<String>>,
}

impl Prompter for Answers {
    fn host_key(&self, _: HostKeyQuestion, reply: Box<dyn FnOnce(HostKeyAnswer) + Send>) {
        reply(HostKeyAnswer::Cancel)
    }
    fn secret(&self, q: SecretQuestion, reply: Box<dyn FnOnce(Option<Secret>) + Send>) {
        lock(&self.asked).push(format!("{q:?}"));
        reply(Some(Secret::new(PASSWORD.into())))
    }
}

fn fs_for(answers: Arc<Answers>) -> WebDavFs {
    let mut cfg = crate::Config::for_user(None);
    cfg.op_timeout = Duration::from_secs(5);
    cfg.connect_timeout = Duration::from_secs(5);
    WebDavFs::new(cfg, answers)
}

fn addr(s: &Server, path: &str) -> VPath {
    VPath::parse(&format!("dav://{USER}@127.0.0.1:{}{path}", s.port)).unwrap()
}

fn names(fs: &WebDavFs, v: &VPath) -> Vec<String> {
    let mut out = Vec::new();
    fs.list(v, 100, &|| false, &mut |b| out.extend(b.into_iter().map(|e| e.name.to_string()))).unwrap();
    out.sort();
    out
}

fn write(fs: &WebDavFs, v: &VPath, o: WriteOpts, data: &[u8]) -> Result<(), FsError> {
    let mut w = fs.open_write(v, o)?;
    w.write_all(data).unwrap();
    w.finish()
}

/// A PUT never targets the name the file will have, and the MOVE that gives it that name says `Overwrite: F` unless the caller
/// said replace.
#[test]
fn webdav_put_goes_to_a_temp_name_and_move_carries_overwrite() {
    let s = start(&["/docs"], &[("/docs/old.txt", "old")]);
    let fs = fs_for(Arc::new(Answers::default()));
    let v = addr(&s, "/docs");
    assert_eq!(names(&fs, &v), vec!["old.txt"]);
    let target = v.join("a b.txt");

    // a new file: PUT to a hidden temp name (create only), then MOVE with Overwrite: F
    write(&fs, &target, WriteOpts::CREATE_NEW, b"new").unwrap();
    assert_eq!(s.file("/docs/a b.txt").as_deref(), Some(&b"new"[..]));
    let seen = s.seen();
    let puts: Vec<_> = seen.iter().filter(|r| r.method == "PUT").collect();
    assert_eq!(puts.len(), 1);
    assert!(puts[0].path.starts_with("/docs/.a b.txt.leaf-up-"), "{}", puts[0].path);
    assert_eq!(puts[0].if_none_match.as_deref(), Some("*"));
    let mv = seen.iter().find(|r| r.method == "MOVE").expect("a MOVE");
    assert_eq!(mv.path, puts[0].path);
    assert_eq!(mv.overwrite.as_deref(), Some("F"));
    assert!(mv.destination.as_deref().is_some_and(|d| d.ends_with("/docs/a%20b.txt")), "{:?}", mv.destination);
    assert!(names(&fs, &v).iter().all(|n| !n.contains("leaf-up")));

    // the name is taken: refused before any byte is sent
    let before = s.seen().len();
    assert_eq!(fs.open_write(&target, WriteOpts::CREATE_NEW).err(), Some(FsError::Exists));
    assert!(s.seen()[before..].iter().all(|r| r.method == "PROPFIND"));

    // replace (the conflict dialog said so): still a temp name, and the MOVE may overwrite
    write(&fs, &target, WriteOpts::REPLACE, b"second").unwrap();
    assert_eq!(s.file("/docs/a b.txt").as_deref(), Some(&b"second"[..]));
    let moves: Vec<_> = s.seen().into_iter().filter(|r| r.method == "MOVE").collect();
    assert_eq!(moves.last().unwrap().overwrite.as_deref(), Some("T"));

    // a move that fails leaves the file as it was and the temp file goes
    lock(&s.state).fail_move = true;
    assert!(write(&fs, &target, WriteOpts::REPLACE, b"third").is_err());
    assert_eq!(s.file("/docs/a b.txt").as_deref(), Some(&b"second"[..]));
    let started = std::time::Instant::now();
    while lock(&s.state).files.keys().any(|f| f.contains("leaf-up")) && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(lock(&s.state).files.keys().all(|f| !f.contains("leaf-up")), "the temp file was left");
    lock(&s.state).fail_move = false;

    // a rename never replaces unless told to (and then only a file)
    let moves_before = s.seen().iter().filter(|r| r.method == "MOVE").count();
    assert_eq!(fs.rename(&v.join("old.txt"), &target, false), Err(FsError::Exists));
    assert_eq!(s.seen().iter().filter(|r| r.method == "MOVE").count(), moves_before);
    fs.rename(&v.join("old.txt"), &target, true).unwrap();
    assert_eq!(s.file("/docs/a b.txt").as_deref(), Some(&b"old"[..]));
    assert_eq!(s.seen().iter().rfind(|r| r.method == "MOVE").unwrap().overwrite.as_deref(), Some("T"));
    // a big file streams (chunked) and still goes the same way
    let big = vec![7u8; (9 << 20) + 5];
    write(&fs, &v.join("big.bin"), WriteOpts::CREATE_NEW, &big).unwrap();
    assert_eq!(s.file("/docs/big.bin").map(|f| f.len()), Some(big.len()));
    // no PUT in this whole run ever named a file that matters
    assert!(s.seen().iter().filter(|r| r.method == "PUT").all(|r| r.path.contains("/.a b.txt.leaf-up-") || r.path.contains("/.big.bin.leaf-up-")));
}

/// A WebDAV server has no login folder: its root is the share, and a delete with the trash on goes into `.leaf-trash` there (or,
/// where the server will not take it, asks "Delete permanently?" by `CannotMove`; never a plain refusal).
#[test]
fn webdav_delete_goes_to_the_trash_in_the_share_root() {
    let s = start(&["/docs"], &[("/docs/old.txt", "old")]);
    let fs = fs_for(Arc::new(Answers::default()));
    let item = addr(&s, "/docs/old.txt");
    let home = addr(&s, "/").root();
    let moved = crate::trash::move_to_trash(&fs, &home, &item, "2026-01-01_00-00-00.000").unwrap();
    assert_eq!(moved.path(), "/.leaf-trash/2026-01-01_00-00-00.000/docs/old.txt");
    assert!(s.file("/docs/old.txt").is_none() && s.file(moved.path()).is_some());
    // the share root itself is never moved
    assert_eq!(crate::trash::move_to_trash(&fs, &home, &home, "x"), Err(crate::trash::TrashError::Fs(FsError::Denied)));
    // a server that will not move into the trash: the caller is asked
    lock(&s.state).fail_move = true;
    s.add_file("/docs/b.txt", "b");
    assert!(matches!(crate::trash::move_to_trash(&fs, &home, &addr(&s, "/docs/b.txt"), "2026-01-01_00-00-01.000"), Err(crate::trash::TrashError::CannotMove(_))));
    assert!(s.file("/docs/b.txt").is_some());
}

/// The password is asked in the dialog (Basic over plain http says so), reaches the server, and is nowhere else: not in an
/// address, an error, a log line.
#[test]
fn webdav_password_never_in_address_log_or_error() {
    crate::ftp_tests::capture_log();
    let s = start(&["/pub"], &[("/pub/a.txt", "hello")]);
    let answers = Arc::new(Answers::default());
    let fs = fs_for(answers.clone());
    // an address with the password in it is refused before anything connects
    assert!(VPath::parse(&format!("dav://{USER}:{PASSWORD}@127.0.0.1:{}/", s.port)).is_err());
    let v = addr(&s, "/pub");
    assert_eq!(names(&fs, &v), vec!["a.txt"]);
    let mut body = String::new();
    fs.open_read(&v.join("a.txt"), 1).unwrap().read_to_string(&mut body).unwrap();
    assert_eq!(body, "ello");
    // asked once and kept in memory
    assert!(fs.stat(&v.join("a.txt")).is_ok());
    let asked = lock(&answers.asked).clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].contains("unencrypted: true"), "Basic over plain http says so: {asked:?}");
    assert!(lock(&s.state).signed_in >= 3, "the sign-in reached the server");
    // every address shown and every error text is free of it
    let err = fs.stat(&v.join("missing")).unwrap_err();
    for text in [v.to_string(), v.join("a.txt").to_string(), err.to_string(), format!("{err:?}")] {
        assert!(!text.contains(PASSWORD), "{text}");
    }
    // the HTTP client's log is dropped above warn, whatever RUST_LOG names; nothing up to debug carries the password or the header
    for target in ["ureq", "ureq::run", "ureq_proto", "ureq_proto::client::flow"] {
        assert!(crate::is_quiet(target, log::Level::Trace) && crate::is_quiet(target, log::Level::Debug), "{target}");
        assert!(!crate::is_quiet(target, log::Level::Warn), "{target}");
    }
    let sent = base64::engine::general_purpose::STANDARD.encode(format!("{USER}:{PASSWORD}"));
    let log = lock(&crate::ftp_tests::LOG).clone();
    assert!(log.iter().any(|l| l.starts_with("leaf_remote")), "the log was captured: {log:?}");
    assert!(!log.iter().any(|l| l.contains(PASSWORD) || l.contains(&sent)), "the password is in the log");
}
