//! `FtpFs` against a small FTP server in this test process (127.0.0.1, an ephemeral port, plain FTP, passive mode), serving
//! a tree in memory. It answers like ProFTPD does by default where that is the harder case: MLSD shows a link as what it
//! points to, CWD into a link keeps the path asked for, LIST marks links with `l`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use leaf_vfs::{Fs, FsError, VPath, WriteOpts};

use crate::ftp::FtpFs;
use crate::prompt::{HostKeyAnswer, HostKeyQuestion, Prompter, Secret, SecretQuestion};

const USER: &str = "alice";
const PASSWORD: &str = "pw-Zq8!secret";

#[derive(Clone, Debug)]
enum Node {
    File(Vec<u8>),
    Dir(BTreeMap<String, Node>),
    /// An absolute path on the server.
    Link(String),
}

#[derive(Default)]
struct Knobs {
    /// STOR takes the bytes, then answers 451 and keeps nothing.
    fail_stor: bool,
    /// DELE requests seen.
    deles: usize,
}

struct State {
    root: Node,
    knobs: Knobs,
}

type Shared = Arc<Mutex<State>>;

fn parts(path: &str) -> Vec<String> {
    path.split('/').filter(|s| !s.is_empty() && *s != ".").map(str::to_string).collect()
}

fn abs(cwd: &str, arg: &str) -> String {
    let p = if arg.starts_with('/') { arg.to_string() } else { format!("{}/{arg}", cwd.trim_end_matches('/')) };
    let mut out: Vec<String> = Vec::new();
    for s in parts(&p) {
        if s == ".." {
            out.pop();
        } else {
            out.push(s);
        }
    }
    format!("/{}", out.join("/"))
}

impl State {
    /// The real path of `path`, links followed (all of them, or all but the last part).
    fn resolve(&self, path: &str, follow_last: bool, depth: usize) -> Option<Vec<String>> {
        if depth > 8 {
            return None;
        }
        let ps = parts(path);
        let mut real: Vec<String> = Vec::new();
        for (i, p) in ps.iter().enumerate() {
            let last = i + 1 == ps.len();
            real.push(p.clone());
            if let Some(Node::Link(t)) = self.node(&real) {
                if !last || follow_last {
                    let t = t.clone();
                    let mut r = self.resolve(&t, true, depth + 1)?;
                    r.extend(ps[i + 1..].iter().cloned());
                    return self.resolve(&format!("/{}", r.join("/")), follow_last, depth + 1);
                }
            }
        }
        Some(real)
    }

    fn node(&self, real: &[String]) -> Option<&Node> {
        let mut n = &self.root;
        for p in real {
            match n {
                Node::Dir(m) => n = m.get(p)?,
                _ => return None,
            }
        }
        Some(n)
    }

    fn dir_mut(&mut self, real: &[String]) -> Option<&mut BTreeMap<String, Node>> {
        let mut n = &mut self.root;
        for p in real {
            match n {
                Node::Dir(m) => n = m.get_mut(p)?,
                _ => return None,
            }
        }
        match n {
            Node::Dir(m) => Some(m),
            _ => None,
        }
    }

    fn get(&self, path: &str, follow: bool) -> Option<&Node> {
        let r = self.resolve(path, follow, 0)?;
        self.node(&r)
    }

    /// (the real parent, the name) of `path`, the last part not followed.
    fn parent_of(&self, path: &str) -> Option<(Vec<String>, String)> {
        let mut r = self.resolve(path, false, 0)?;
        let name = r.pop()?;
        Some((r, name))
    }
}

fn put(st: &mut State, path: &str, n: Node) {
    let (dir, name) = st.parent_of(path).expect("parent");
    st.dir_mut(&dir).expect("dir").insert(name, n);
}

fn ls_line(name: &str, n: &Node) -> String {
    match n {
        Node::File(b) => format!("-rw-r--r-- 1 1000 1000 {} Jan 02 2024 {name}", b.len()),
        Node::Dir(_) => format!("drwxr-xr-x 2 1000 1000 0 Jan 02 2024 {name}"),
        Node::Link(t) => format!("lrwxrwxrwx 1 1000 1000 0 Jan 02 2024 {name} -> {t}"),
    }
}

/// What MLSD/MLST say: a link as what it points to (ProFTPD without ShowSymlinks in MLSD).
fn mlsx_line(st: &State, name: &str, n: &Node, at: &str) -> String {
    let shown = match n {
        Node::Link(t) => st.get(t, true).cloned().unwrap_or(Node::File(Vec::new())),
        n => n.clone(),
    };
    match shown {
        Node::File(b) => format!("type=file;size={};modify=20240102030405; {name}", b.len()),
        _ => format!("type=dir;modify=20240102030405; {}", if at.is_empty() { name } else { at }),
    }
}

struct Server {
    port: u16,
    state: Shared,
}

fn start(root: Node) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let state: Shared = Arc::new(Mutex::new(State { root, knobs: Knobs::default() }));
    let st = state.clone();
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let st = st.clone();
            std::thread::spawn(move || {
                let _ = session(c, st);
            });
        }
    });
    Server { port, state }
}

fn session(c: TcpStream, st: Shared) -> std::io::Result<()> {
    let mut out = c.try_clone()?;
    let mut rd = BufReader::new(c);
    let say = |out: &mut TcpStream, s: &str| out.write_all(format!("{s}\r\n").as_bytes());
    say(&mut out, "220 fake ftp")?;
    let mut cwd = "/".to_string();
    let mut logged = false;
    let mut user = String::new();
    let mut pasv: Option<TcpListener> = None;
    let mut rest = 0usize;
    let mut rnfr: Option<String> = None;
    loop {
        let mut line = String::new();
        if rd.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        let (cmd, arg) = line.split_once(' ').map_or((line.as_str(), ""), |(a, b)| (a, b));
        let cmd = cmd.to_ascii_uppercase();
        let path = abs(&cwd, arg);
        let open_data = |pasv: &mut Option<TcpListener>| -> std::io::Result<TcpStream> {
            let l = pasv.take().ok_or_else(|| std::io::Error::other("no PASV"))?;
            Ok(l.accept()?.0)
        };
        match cmd.as_str() {
            "USER" => {
                user = arg.to_string();
                say(&mut out, "331 password please")?;
            }
            "PASS" => {
                if user == USER && arg == PASSWORD {
                    logged = true;
                    say(&mut out, "230 logged in")?;
                } else {
                    say(&mut out, "530 Login incorrect.")?;
                }
            }
            "QUIT" => {
                say(&mut out, "221 bye")?;
                return Ok(());
            }
            _ if !logged => say(&mut out, "530 Please login with USER and PASS.")?,
            "FEAT" => {
                out.write_all(b"211-Features:\r\n MLST type*;size*;modify*;\r\n UTF8\r\n211 End\r\n")?;
            }
            "OPTS" | "TYPE" | "NOOP" => say(&mut out, "200 ok")?,
            "PWD" => say(&mut out, &format!("257 \"{cwd}\" is the current directory"))?,
            "CWD" => {
                let ok = matches!(lock(&st).get(&path, true), Some(Node::Dir(_)));
                if ok {
                    // the path as asked for (ProFTPD: links are not resolved in PWD)
                    cwd = path;
                    say(&mut out, "250 ok")?;
                } else {
                    say(&mut out, "550 No such directory")?;
                }
            }
            "PASV" => {
                let l = TcpListener::bind("127.0.0.1:0")?;
                let p = l.local_addr()?.port();
                pasv = Some(l);
                say(&mut out, &format!("227 Entering Passive Mode (127,0,0,1,{},{})", p >> 8, p & 0xff))?;
            }
            "MLSD" | "LIST" => {
                let target = if arg.is_empty() { cwd.clone() } else { path.clone() };
                let lines: Option<Vec<String>> = {
                    let s = lock(&st);
                    match s.get(&target, true) {
                        Some(Node::Dir(m)) => Some(m.iter().map(|(k, n)| if cmd == "MLSD" { mlsx_line(&s, k, n, "") } else { ls_line(k, n) }).collect()),
                        _ => None,
                    }
                };
                match lines {
                    Some(lines) => {
                        say(&mut out, "150 here it comes")?;
                        let mut d = open_data(&mut pasv)?;
                        for l in lines {
                            d.write_all(format!("{l}\r\n").as_bytes())?;
                        }
                        drop(d);
                        say(&mut out, "226 done")?;
                    }
                    None => say(&mut out, "550 No such directory")?,
                }
            }
            "MLST" => {
                let s = lock(&st);
                match s.get(&path, false) {
                    Some(n) => {
                        let l = mlsx_line(&s, base(&path), n, &path);
                        drop(s);
                        out.write_all(format!("250-Listing\r\n {l}\r\n250 End\r\n").as_bytes())?;
                    }
                    None => {
                        drop(s);
                        say(&mut out, "550 No such file")?;
                    }
                }
            }
            "REST" => {
                rest = arg.parse().unwrap_or(0);
                say(&mut out, "350 restarting")?;
            }
            "RETR" => {
                let data = match lock(&st).get(&path, true) {
                    Some(Node::File(b)) => Some(b.clone()),
                    _ => None,
                };
                match data {
                    Some(b) => {
                        say(&mut out, "150 sending")?;
                        let mut d = open_data(&mut pasv)?;
                        let _ = d.write_all(&b[rest.min(b.len())..]);
                        drop(d);
                        rest = 0;
                        say(&mut out, "226 done")?;
                    }
                    None => say(&mut out, "550 No such file")?,
                }
            }
            "STOR" | "APPE" => {
                say(&mut out, "150 go ahead")?;
                let mut d = open_data(&mut pasv)?;
                let mut buf = Vec::new();
                let _ = d.read_to_end(&mut buf);
                let mut s = lock(&st);
                if s.knobs.fail_stor {
                    drop(s);
                    say(&mut out, "451 Local error in processing")?;
                    continue;
                }
                let mut body = match (cmd.as_str(), s.get(&path, true)) {
                    ("APPE", Some(Node::File(old))) => old.clone(),
                    _ => Vec::new(),
                };
                body.extend(buf);
                put(&mut s, &path, Node::File(body));
                drop(s);
                say(&mut out, "226 stored")?;
            }
            "DELE" => {
                let mut s = lock(&st);
                s.knobs.deles += 1;
                let r = s.parent_of(&path).and_then(|(dir, name)| {
                    let m = s.dir_mut(&dir)?;
                    match m.get(&name) {
                        // unlink: a link goes, never what it points to
                        Some(Node::File(_) | Node::Link(_)) => m.remove(&name),
                        _ => None,
                    }
                });
                drop(s);
                say(&mut out, if r.is_some() { "250 deleted" } else { "550 Could not delete" })?;
            }
            "RMD" => {
                let mut s = lock(&st);
                let r = s.parent_of(&path).and_then(|(dir, name)| {
                    let m = s.dir_mut(&dir)?;
                    match m.get(&name) {
                        Some(Node::Dir(c)) if c.is_empty() => m.remove(&name),
                        _ => None,
                    }
                });
                drop(s);
                say(&mut out, if r.is_some() { "250 removed" } else { "550 Directory not empty or not a directory" })?;
            }
            "MKD" => {
                let mut s = lock(&st);
                if s.get(&path, false).is_some() {
                    drop(s);
                    say(&mut out, "550 File exists")?;
                } else {
                    put(&mut s, &path, Node::Dir(BTreeMap::new()));
                    drop(s);
                    say(&mut out, &format!("257 \"{path}\" created"))?;
                }
            }
            "RNFR" => {
                if lock(&st).get(&path, false).is_some() {
                    rnfr = Some(path);
                    say(&mut out, "350 ready")?;
                } else {
                    say(&mut out, "550 No such file")?;
                }
            }
            "RNTO" => {
                let Some(from) = rnfr.take() else {
                    say(&mut out, "503 RNFR first")?;
                    continue;
                };
                let mut s = lock(&st);
                let (fd, fname) = s.parent_of(&from).unwrap();
                let n = s.dir_mut(&fd).unwrap().remove(&fname).unwrap();
                // (Unix rename: a file there is replaced)
                put(&mut s, &path, n);
                drop(s);
                say(&mut out, "250 renamed")?;
            }
            _ => say(&mut out, "502 Command not implemented")?,
        }
    }
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn dir(items: Vec<(&str, Node)>) -> Node {
    Node::Dir(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

fn file(s: &str) -> Node {
    Node::File(s.as_bytes().to_vec())
}

/// Answers the password question with the right password and counts the questions.
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

fn fs_for(answers: Arc<Answers>) -> FtpFs {
    let mut cfg = crate::Config::for_user(None);
    cfg.op_timeout = Duration::from_secs(5);
    cfg.connect_timeout = Duration::from_secs(5);
    FtpFs::new(cfg, answers)
}

fn addr(s: &Server, path: &str) -> VPath {
    VPath::parse(&format!("ftp://{USER}@127.0.0.1:{}{path}", s.port)).unwrap()
}

// -------------------------------------------------------------------------------- the log, as the app keeps it

static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, m: &log::Metadata) -> bool {
        // the app's cap: these targets at debug at most, whatever RUST_LOG says (see lib.rs of rf-app)
        !(crate::QUIET_LOG_TARGETS.iter().any(|t| m.target().starts_with(t)) && m.level() > log::Level::Debug)
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            lock(&LOG).push(format!("{} {}", r.target(), r.args()));
        }
    }
    fn flush(&self) {}
}

fn capture_log() {
    static ON: OnceLock<()> = OnceLock::new();
    ON.get_or_init(|| {
        if log::set_logger(&Capture).is_ok() {
            log::set_max_level(log::LevelFilter::Trace);
        }
    });
}

#[test]
fn ftp_password_never_in_address_or_log() {
    capture_log();
    let s = start(dir(vec![("pub", dir(vec![("a.txt", file("hello"))]))]));
    let answers = Arc::new(Answers::default());
    let fs = fs_for(answers.clone());
    // an address with the password in it is refused before anything connects
    assert!(VPath::parse(&format!("ftp://{USER}:{PASSWORD}@127.0.0.1:{}/", s.port)).is_err());
    let v = addr(&s, "/pub");
    let mut names = Vec::new();
    fs.list(&v, 100, &|| false, &mut |b| names.extend(b.into_iter().map(|e| e.name.to_string()))).unwrap();
    assert_eq!(names, vec!["a.txt"]);
    let mut body = String::new();
    fs.open_read(&v.join("a.txt"), 1).unwrap().read_to_string(&mut body).unwrap();
    assert_eq!(body, "ello");
    // asked once, kept in memory: a new connection does not ask again
    fs.drop_connections();
    assert!(fs.stat(&v.join("a.txt")).is_ok());
    let asked = lock(&answers.asked).clone();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].contains("unencrypted: true"), "plain FTP says so: {asked:?}");
    // every address shown and every error text is free of it
    let err = fs.stat(&v.join("missing")).unwrap_err();
    for text in [v.to_string(), v.join("a.txt").to_string(), err.to_string(), format!("{:?}", fs.home(&v).unwrap())] {
        assert!(!text.contains(PASSWORD), "{text}");
    }
    let log = lock(&LOG).clone();
    assert!(log.iter().any(|l| l.starts_with("leaf_remote")), "the log was captured: {log:?}");
    assert!(!log.iter().any(|l| l.contains(PASSWORD)), "the password is in the log");
}

#[test]
fn ftp_recursive_remove_does_not_follow_a_link() {
    let s = start(dir(vec![
        ("keep", dir(vec![("precious.txt", file("do not touch")), ("inner", dir(vec![("deep.txt", file("deep"))]))])),
        ("gone", dir(vec![("x.txt", file("x")), ("sub", dir(vec![("y.txt", file("y"))])), ("to-keep", Node::Link("/keep".into())), ("to-file", Node::Link("/keep/precious.txt".into()))])),
        ("top-link", Node::Link("/keep".into())),
    ]));
    let fs = fs_for(Arc::new(Answers::default()));
    // MLSD shows the link as a folder: the stat says so, and still nothing behind it is touched
    assert!(fs.stat(&addr(&s, "/gone/to-keep")).unwrap().is_dir());
    fs.remove(&addr(&s, "/gone"), true).unwrap();
    // a link given itself is unlinked too
    fs.remove(&addr(&s, "/top-link"), true).unwrap();
    let st = lock(&s.state);
    assert!(st.get("/gone", false).is_none());
    assert!(st.get("/top-link", false).is_none());
    assert!(matches!(st.get("/keep/precious.txt", false), Some(Node::File(b)) if b == b"do not touch"));
    assert!(matches!(st.get("/keep/inner/deep.txt", false), Some(Node::File(_))));
}

#[test]
fn ftp_failed_upload_leaves_the_original() {
    let s = start(dir(vec![("site", dir(vec![("index.html", file("the original"))]))]));
    let fs = fs_for(Arc::new(Answers::default()));
    let target = addr(&s, "/site/index.html");
    let only_original = |s: &Server| {
        let st = lock(&s.state);
        let Some(Node::Dir(m)) = st.get("/site", true) else { panic!("no folder") };
        let names: Vec<&String> = m.keys().collect();
        assert_eq!(names, vec!["index.html"], "nothing left beside it");
        assert!(matches!(m.get("index.html"), Some(Node::File(b)) if b == b"the original"));
    };
    // 1. the upload is cut off (dropped before finish)
    {
        let mut w = fs.open_write(&target, WriteOpts::REPLACE).unwrap();
        w.write_all(b"half of the new").unwrap();
        drop(w);
    }
    // (the temporary file is removed over another connection, off the thread)
    for _ in 0..250 {
        if lock(&s.state).knobs.deles > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    only_original(&s);
    // 2. the server refuses the upload at the end
    lock(&s.state).knobs.fail_stor = true;
    let mut w = fs.open_write(&target, WriteOpts::REPLACE).unwrap();
    w.write_all(b"the new version").unwrap();
    assert!(w.finish().is_err());
    only_original(&s);
    // 3. it works when the server takes it, and the old version is gone
    lock(&s.state).knobs.fail_stor = false;
    let mut w = fs.open_write(&target, WriteOpts::REPLACE).unwrap();
    w.write_all(b"the new version").unwrap();
    w.finish().unwrap();
    let st = lock(&s.state);
    let Some(Node::Dir(m)) = st.get("/site", true) else { panic!() };
    assert_eq!(m.len(), 1);
    assert!(matches!(m.get("index.html"), Some(Node::File(b)) if b == b"the new version"));
    drop(st);
    // a create-new over a name that is there is refused
    assert_eq!(fs.open_write(&target, WriteOpts::CREATE_NEW).err(), Some(FsError::Exists));
}
