//! Against an in-process HTTP server on 127.0.0.1 only: nothing here touches the internet.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;

fn data(len: usize) -> Vec<u8> {
    let mut x = 0x2545F4914F6CDD1Du64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

struct Server {
    port: u16,
    /// Bytes after which every ranged answer is cut off (usize::MAX = never).
    cut_after: Arc<AtomicUsize>,
}

fn serve(body: Vec<u8>) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let cut_after = Arc::new(AtomicUsize::new(usize::MAX));
    let hits = Arc::new(AtomicUsize::new(0));
    let cut = cut_after.clone();
    let body = Arc::new(body);
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (body, cut, hit) = (body.clone(), cut.clone(), hits.clone());
            std::thread::spawn(move || {
                let _ = handle(s, &body, &cut, &hit);
            });
        }
    });
    Server { port, cut_after }
}

fn handle(mut s: TcpStream, body: &[u8], cut: &AtomicUsize, hits: &AtomicUsize) -> std::io::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut first = String::new();
    r.read_line(&mut first)?;
    let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
    let mut range = None;
    loop {
        let mut line = String::new();
        r.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
            let (a, b) = v.trim().split_once('-').unwrap();
            range = Some((a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()));
        }
    }
    hits.fetch_add(1, Ordering::SeqCst);
    let disposition = path.strip_prefix("/cd/").map(|enc| format!("Content-Disposition: attachment; filename=\"{}\"\r\n", name::percent_decode(enc)));
    let extra = format!("ETag: \"v1\"\r\nAccept-Ranges: bytes\r\n{}", disposition.unwrap_or_default());
    let (status, from, to) = match range {
        Some((a, b)) => ("206 Partial Content", a, b.min(body.len() - 1)),
        None => ("200 OK", 0, body.len() - 1),
    };
    let slice = &body[from..=to];
    let cr = if range.is_some() { format!("Content-Range: bytes {from}-{to}/{}\r\n", body.len()) } else { String::new() };
    write!(s, "HTTP/1.1 {status}\r\n{extra}{cr}Content-Length: {}\r\nConnection: close\r\n\r\n", slice.len())?;
    // a cut line: only the first bytes of a long answer arrive, then the connection drops
    let limit = cut.load(Ordering::SeqCst);
    if slice.len() > 16 && limit != usize::MAX {
        s.write_all(&slice[..limit.min(slice.len())])?;
        s.flush()?;
        return Ok(());
    }
    s.write_all(slice)?;
    s.flush()
}

struct Quiet(Resolve, std::sync::Mutex<u64>);

impl Host for Quiet {
    fn progress(&self, p: &Progress) {
        *self.1.lock().unwrap() = p.resumed;
    }
    fn exists(&self, _: &Path, _: Option<u64>) -> Resolve {
        self.0
    }
}

fn host(r: Resolve) -> Quiet {
    Quiet(r, Default::default())
}

fn opts(retries: u32) -> Options {
    Options { segments: 4, retries, backoff: Duration::from_millis(20), ..Options::default() }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("leaf-download-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn url(s: &Server, p: &str) -> String {
    format!("http://127.0.0.1:{}{p}", s.port)
}

#[test]
fn resume_after_a_cut_is_byte_identical() {
    let body = data(6 << 20);
    let srv = serve(body.clone());
    let dir = tmp("resume");
    let ctl = Control::default();
    // the line is cut after 300 KiB of each answer and there are no retries: the first run fails and leaves a partial file
    srv.cut_after.store(300 << 10, Ordering::SeqCst);
    let first = download(&url(&srv, "/f.bin"), &dir, &opts(0), &ctl, &host(Resolve::KeepBoth));
    assert!(first.is_err(), "the cut must fail the first run");
    assert!(!dir.join("f.bin").exists(), "an incomplete file is never renamed");
    assert!(dir.join("f.bin.leaf-part").exists() && dir.join("f.bin.leaf-part.state").exists());
    // the line is whole again: the same address carries on from the state file
    srv.cut_after.store(usize::MAX, Ordering::SeqCst);
    let h = host(Resolve::KeepBoth);
    let out = download(&url(&srv, "/f.bin"), &dir, &opts(2), &ctl, &h).unwrap();
    assert!(matches!(out, Outcome::Saved(ref p) if p == &dir.join("f.bin")));
    assert!(*h.1.lock().unwrap() > 0, "the second run started from what the first had");
    assert_eq!(std::fs::read(dir.join("f.bin")).unwrap(), body);
    assert!(!dir.join("f.bin.leaf-part").exists() && !dir.join("f.bin.leaf-part.state").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_hostile_name_stays_in_the_folder() {
    let srv = serve(data(4096));
    let outer = tmp("names");
    let dir = outer.join("in");
    std::fs::create_dir(&dir).unwrap();
    let ctl = Control::default();
    for (path, want) in [("/cd/..%5C..%5Cx", "x"), ("/cd/CON", "_CON"), ("/cd/a%2Fb", "b"), ("/..%5C..%5Cy", "y"), ("/cd/..", "download"), ("/cd/nul.txt", "_nul.txt")] {
        let out = download(&url(&srv, path), &dir, &opts(0), &ctl, &host(Resolve::KeepBoth)).unwrap();
        let Outcome::Saved(p) = out else { panic!("{path} skipped") };
        assert_eq!(p.parent().unwrap(), dir, "{path} left the folder");
        assert_eq!(p.file_name().unwrap().to_string_lossy(), want, "{path}");
    }
    assert_eq!(std::fs::read_dir(&outer).unwrap().count(), 1, "something was written next to the folder");
    let _ = std::fs::remove_dir_all(&outer);
}

#[test]
fn an_existing_file_is_not_overwritten_without_the_choice() {
    let body = data(100_000);
    let srv = serve(body.clone());
    let dir = tmp("exists");
    let ctl = Control::default();
    std::fs::write(dir.join("f.bin"), b"old").unwrap();
    // skip: nothing is written
    let out = download(&url(&srv, "/f.bin"), &dir, &opts(0), &ctl, &host(Resolve::Skip)).unwrap();
    assert!(matches!(out, Outcome::Skipped));
    assert_eq!(std::fs::read(dir.join("f.bin")).unwrap(), b"old");
    assert!(!dir.join("f.bin.leaf-part").exists());
    // keep both: the old one stays, the new one has another name
    let out = download(&url(&srv, "/f.bin"), &dir, &opts(0), &ctl, &host(Resolve::KeepBoth)).unwrap();
    assert!(matches!(out, Outcome::Saved(ref p) if p == &dir.join("f (2).bin")));
    assert_eq!(std::fs::read(dir.join("f.bin")).unwrap(), b"old");
    assert_eq!(std::fs::read(dir.join("f (2).bin")).unwrap(), body);
    // replace: only now is the old one gone
    download(&url(&srv, "/f.bin"), &dir, &opts(0), &ctl, &host(Resolve::Replace)).unwrap();
    assert_eq!(std::fs::read(dir.join("f.bin")).unwrap(), body);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cancel_stops_and_removes_the_partial_file() {
    let srv = serve(data(2 << 20));
    let dir = tmp("cancel");
    let ctl = Control::default();
    ctl.cancel.store(true, Ordering::SeqCst);
    let r = download(&url(&srv, "/f.bin"), &dir, &opts(0), &ctl, &host(Resolve::KeepBoth));
    assert!(matches!(r, Err(Error::Cancelled)));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_saved_queue_comes_back_and_resumes_byte_identical() {
    let body = data(6 << 20);
    let srv = serve(body.clone());
    let dir = tmp("queue");
    let ctl = Control::default();
    // the first run is cut part-way (it stands for the app quitting with the download running)
    srv.cut_after.store(300 << 10, Ordering::SeqCst);
    assert!(download(&url(&srv, "/f.bin"), &dir, &opts(0), &ctl, &host(Resolve::KeepBoth)).is_err());
    let file = dir.join("downloads.json");
    let mut store = store::Store::default();
    store.active.push(store::Saved { key: 7, url: url(&srv, "/f.bin"), dest: dir.clone(), sha256: None, verify: false, paused: true, browser: false, sort: false, subfolder: None });
    for i in 0..60 {
        store.remember(store::Past { url: format!("http://h/{i}"), dest: dir.clone(), ok: i % 2 == 0, when: i });
    }
    store.save(&file);
    // the next start: the list is read back, the history holds the last 50, and the saved download carries on from its part
    let back = store::Store::load(&file);
    assert_eq!(back, store);
    assert_eq!(back.history.len(), store::HISTORY);
    assert_eq!(back.history[0].when, 59, "newest first");
    srv.cut_after.store(usize::MAX, Ordering::SeqCst);
    let saved = &back.active[0];
    let h = host(Resolve::KeepBoth);
    let out = download(&saved.url, &saved.dest, &opts(2), &ctl, &h).unwrap();
    assert!(matches!(out, Outcome::Saved(ref p) if p == &dir.join("f.bin")));
    assert!(*h.1.lock().unwrap() > 0, "it started from the part file");
    assert_eq!(std::fs::read(dir.join("f.bin")).unwrap(), body);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_checksum_mismatch_keeps_the_part_file_and_makes_no_file() {
    let body = data(300_000);
    let srv = serve(body.clone());
    let dir = tmp("sha");
    let ctl = Control::default();
    let wrong = Options { sha256: Some("0".repeat(64)), ..opts(0) };
    let r = download(&url(&srv, "/f.bin"), &dir, &wrong, &ctl, &host(Resolve::KeepBoth));
    assert!(matches!(r, Err(Error::Checksum { .. })), "{r:?}");
    assert!(!dir.join("f.bin").exists());
    assert_eq!(std::fs::read(dir.join("f.bin.leaf-part")).unwrap(), body, "the part file stays");
    // the right digest (given in capitals) finishes the same download
    let right = sha256_of(&dir.join("f.bin.leaf-part"), &ctl.cancel).unwrap();
    let good = Options { sha256: Some(right.to_ascii_uppercase()), ..opts(0) };
    download(&url(&srv, "/f.bin"), &dir, &good, &ctl, &host(Resolve::KeepBoth)).unwrap();
    assert_eq!(std::fs::read(dir.join("f.bin")).unwrap(), body);
    assert_eq!(sidecar_url("http://h/a/f.zip?x=1"), "http://h/a/f.zip.sha256?x=1");
    assert_eq!(parse_sha256(&format!("{right}  f.bin\n")), Some(right));
    assert!(has_password("https://u:p@h/x") && !has_password("https://u@h/x?a=b:c@d"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_finished_download_leaves_no_part_files() {
    let body = data(9 << 20);
    let srv = serve(body.clone());
    let dir = tmp("nostate");
    let out = download(&url(&srv, "/f.bin"), &dir, &opts(1), &Control::default(), &host(Resolve::KeepBoth)).unwrap();
    assert!(matches!(out, Outcome::Saved(_)));
    let left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, vec!["f.bin".to_string()], "nothing but the file: {left:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn browser_credentials_never_reach_a_file() {
    const COOKIE: &str = "session=cookie-secret-7f3a";
    const AUTH: &str = "Bearer auth-secret-91bc";
    let body = data(4 << 20);
    let srv = serve(body);
    let dir = tmp("credentials");
    let mut headers = Headers::default();
    headers.set("Cookie", COOKIE);
    headers.set("Authorization", AUTH);
    headers.set("Referer", "https://example.com/page");
    let o = Options { headers: headers.clone(), ..opts(0) };
    // cut part-way with no retries: the part file and its state stay behind, as after a crash
    srv.cut_after.store(200 << 10, Ordering::SeqCst);
    let u = url(&srv, "/c.bin");
    assert!(download(&u, &dir, &o, &Control::default(), &host(Resolve::KeepBoth)).is_err());
    let state = std::fs::read_to_string(dir.join("c.bin.leaf-part.state")).expect("the part state is written");
    // the saved list entry leaf writes for the same download
    let saved = store::Store { active: vec![store::Saved { key: 1, url: u.clone(), dest: dir.clone(), sha256: None, verify: false, paused: false, browser: true, sort: false, subfolder: None }], history: Vec::new() };
    let saved = serde_json::to_string(&saved).unwrap();
    let shown = format!("{headers:?} {headers} {:?}", o.headers);
    for text in [&state, &saved, &shown] {
        assert!(!text.contains("cookie-secret") && !text.contains("auth-secret"), "a credential leaked into: {text}");
    }
    assert!(shown.contains("Cookie") && shown.contains("Authorization"), "the names are still shown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn credentials_stay_on_the_first_origin() {
    let first = http::origin_of("https://files.example.com/dl/a.zip");
    // https to http on the same host: Cookie and Authorization would go out in clear text
    assert_ne!(first, http::origin_of("http://files.example.com/dl/a.zip"));
    assert_ne!(first, http::origin_of("https://files.example.com:8443/a.zip"));
    assert_ne!(first, http::origin_of("https://cdn.example.com/a.zip"));
    assert_eq!(first, http::origin_of("HTTPS://Files.Example.com:443/other?x=1"));
    // a redirect's target resolved against the address it came from
    assert_eq!(http::resolve("https://h/dir/file?a=1", "?x=1"), "https://h/dir/file?x=1");
    assert_eq!(http::resolve("https://h/dir/file", "other"), "https://h/dir/other");
    assert_eq!(http::resolve("https://h/dir/file", "/root"), "https://h/root");
    assert_eq!(http::resolve("https://h/dir/file", "//g/x"), "https://g/x");
    assert_eq!(http::resolve("https://h", "?x=1"), "https://h/?x=1");
}

/// A CONNECT proxy on 127.0.0.1: it keeps the request head it was sent and tunnels to the target.
fn proxy() -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let heads: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let seen = heads.clone();
    std::thread::spawn(move || {
        for client in l.incoming().flatten() {
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(client.try_clone().unwrap());
                let (mut head, mut target) = (String::new(), String::new());
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    if target.is_empty() {
                        target = line.split_whitespace().nth(1).unwrap_or_default().to_string();
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                    head.push_str(&line);
                }
                seen.lock().unwrap().push(head);
                let Ok(up) = TcpStream::connect(&target) else { return };
                let mut c = client.try_clone().unwrap();
                let _ = c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                let (mut up2, mut c2) = (up.try_clone().unwrap(), client);
                let t = std::thread::spawn(move || {
                    let n = std::io::copy(&mut up2, &mut c2).ok();
                    let _ = c2.shutdown(std::net::Shutdown::Both);
                    n
                });
                let _ = std::io::copy(&mut r, &mut &up);
                let _ = t.join();
            });
        }
    });
    (port, heads)
}

#[test]
fn proxy_password_is_sent_to_the_proxy_and_never_reaches_a_file() {
    const PASSWORD: &str = "proxy-secret-5d2e";
    let srv = serve(data(3 << 20));
    let (pport, heads) = proxy();
    let dir = tmp("proxy-secret");
    let server = ProxyServer { kind: ProxyKind::Http, host: "127.0.0.1".into(), port: pport, user: Some("alice".into()), password: Some(PASSWORD.into()) };
    let o = Options { proxy: ProxyMode::Manual(server.clone()), ..opts(0) };
    // cut part-way: the part state stays behind, as after a crash
    srv.cut_after.store(100 << 10, Ordering::SeqCst);
    let u = url(&srv, "/p.bin");
    assert!(download(&u, &dir, &o, &Control::default(), &host(Resolve::KeepBoth)).is_err());
    let state = std::fs::read_to_string(dir.join("p.bin.leaf-part.state")).expect("the part state is written");
    let saved = store::Store { active: vec![store::Saved { key: 1, url: u.clone(), dest: dir.clone(), sha256: None, verify: false, paused: false, browser: false, sort: false, subfolder: None }], history: Vec::new() };
    let saved = serde_json::to_string(&saved).unwrap();
    let shown = format!("{server:?} {:?}", ProxyMode::Manual(server.clone()));
    for text in [&state, &saved, &shown] {
        assert!(!text.contains(PASSWORD), "the proxy password leaked into: {text}");
    }
    // the proxy itself did get the login (Basic: alice:password)
    let sent = heads.lock().unwrap().join("\n").to_ascii_lowercase();
    assert!(sent.contains("proxy-authorization: basic"), "the proxy was not signed in to: {sent}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A server that wants `Authorization: Basic` (the header's exact value) for everything; without it: 401 and a challenge.
fn serve_basic(body: Vec<u8>, want: String) -> Server {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let cut_after = Arc::new(AtomicUsize::new(usize::MAX));
    let cut = cut_after.clone();
    let body = Arc::new(body);
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (body, cut, want) = (body.clone(), cut.clone(), want.clone());
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    head.push_str(&line);
                }
                let mut s = s;
                if !head.to_ascii_lowercase().contains(&format!("authorization: {}", want.to_ascii_lowercase())) {
                    let _ = s.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"Private area\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    return;
                }
                let (from, to) = head
                    .to_ascii_lowercase()
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes=").map(str::to_string))
                    .and_then(|v| v.trim().split_once('-').map(|(a, b)| (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap().min(body.len() - 1))))
                    .unwrap_or((0, body.len() - 1));
                let slice = &body[from..=to];
                let _ = write!(s, "HTTP/1.1 206 Partial Content\r\nETag: \"v1\"\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {from}-{to}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len(), slice.len());
                let limit = cut.load(Ordering::SeqCst);
                let _ = s.write_all(&slice[..if slice.len() > 16 { limit.min(slice.len()) } else { slice.len() }]);
            });
        }
    });
    Server { port, cut_after }
}

#[test]
fn web_sign_in_is_asked_for_and_never_reaches_a_file() {
    const USER: &str = "alice";
    const PASSWORD: &str = "web-secret-c41f";
    // Basic: base64 of "alice:web-secret-c41f"
    let sent = WebAuth::new(&Challenge::parse(&["Basic realm=\"x\"".to_string()]).unwrap(), USER, PASSWORD).header("http://h/f");
    let srv = serve_basic(data(3 << 20), sent.clone());
    let dir = tmp("web-secret");
    let u = url(&srv, "/w.bin");
    // without a sign-in the answer is the challenge, with its realm
    let Err(Error::Auth(ch)) = download(&u, &dir, &opts(0), &Control::default(), &host(Resolve::KeepBoth)) else { panic!("expected a challenge") };
    assert_eq!((ch.scheme, ch.realm.as_str()), (AuthScheme::Basic, "Private area"));
    // signed in, then cut part-way: the part state stays behind, as after a crash
    let mut o = opts(0);
    o.headers.set_web_auth(WebAuth::new(&ch, USER, PASSWORD));
    srv.cut_after.store(100 << 10, Ordering::SeqCst);
    assert!(matches!(download(&u, &dir, &o, &Control::default(), &host(Resolve::KeepBoth)), Err(e) if !matches!(e, Error::Auth(_))), "the server took the sign-in");
    let state = std::fs::read_to_string(dir.join("w.bin.leaf-part.state")).expect("the part state is written");
    let saved = store::Store { active: vec![store::Saved { key: 1, url: u.clone(), dest: dir.clone(), sha256: None, verify: false, paused: false, browser: false, sort: false, subfolder: None }], history: Vec::new() };
    let saved = serde_json::to_string(&saved).unwrap();
    let b64 = sent.trim_start_matches("Basic ");
    let shown = format!("{:?} {} {:?} {:?}", o.headers, o.headers, WebAuth::new(&ch, USER, PASSWORD), Error::Auth(ch.clone()));
    for text in [&state, &saved, &shown] {
        assert!(!text.contains(PASSWORD) && !text.contains(b64) && !text.contains(USER), "the sign-in leaked into: {text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_sign_in_goes_to_the_server_that_asked_and_not_to_the_first_address() {
    let hop = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sent = WebAuth::new(&Challenge::parse(&["Basic realm=\"x\"".to_string()]).unwrap(), "bob", "pw-1").header("http://h/f");
    let target = serve_basic(data(1 << 20), sent);
    // the first address only redirects to the server that wants the sign-in, and keeps every request head it sees
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let aport = l.local_addr().unwrap().port();
    let (seen, to) = (hop.clone(), target.port);
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut head = String::new();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                head.push_str(&line);
            }
            seen.lock().unwrap().push(head.to_ascii_lowercase());
            let mut s = s;
            let _ = write!(s, "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{to}/r.bin\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    let dir = tmp("web-redirect");
    let u = format!("http://127.0.0.1:{aport}/short");
    let Err(Error::Auth(ch)) = download(&u, &dir, &opts(0), &Control::default(), &host(Resolve::KeepBoth)) else { panic!("expected a challenge") };
    assert_eq!(ch.origin.2, target.port, "the challenge names the server that answered, not the first address");
    let mut o = opts(0);
    o.headers.set_web_auth(WebAuth::new(&ch, "bob", "pw-1"));
    let out = download(&u, &dir, &o, &Control::default(), &host(Resolve::KeepBoth)).expect("the sign-in is accepted where it was asked for");
    assert!(matches!(out, Outcome::Saved(_)));
    assert!(hop.lock().unwrap().iter().all(|h| !h.contains("authorization")), "the first address saw the sign-in");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn digest_is_chosen_over_basic_when_both_are_offered() {
    let one_line = ["Basic realm=\"a\", Digest realm=\"a\", nonce=\"n1\", qop=\"auth\"".to_string()];
    assert_eq!(Challenge::parse(&one_line).map(|c| c.scheme), Some(AuthScheme::Digest));
    let two_lines = ["Basic realm=\"a\"".to_string(), "Digest realm=\"a\", nonce=\"n1\"".to_string()];
    assert_eq!(Challenge::parse(&two_lines).map(|c| c.scheme), Some(AuthScheme::Digest));
    // Basic only; and nothing leaf can answer
    assert_eq!(Challenge::parse(&["Basic realm=\"a\"".to_string()]).map(|c| c.scheme), Some(AuthScheme::Basic));
    assert!(Challenge::parse(&["Negotiate".to_string(), "Bearer realm=\"a\"".to_string()]).is_none());
    // the realm and the stale mark are read from the right challenge
    let c = Challenge::parse(&["Basic realm=\"b\", Digest realm=\"d\", nonce=\"n\", stale=true".to_string()]).unwrap();
    assert_eq!(c.realm, "d");
    assert!(c.is_stale());
}
