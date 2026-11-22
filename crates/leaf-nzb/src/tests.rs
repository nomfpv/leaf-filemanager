use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::dial::{self, Conn, DialError, Dialer};
use crate::engine::{self, Control, FileJob, State};
use crate::nzb::Segment;
use crate::server::{Password, Priority, Server};
use crate::yenc;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("leaf-nzb-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn data(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ (i >> 3) as u8).collect()
}

#[test]
fn yenc_decode_checks_crc_and_places_parts() {
    // a whole file in one article, with every byte value (escapes included)
    let all: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
    let p = yenc::decode(&yenc::encode("all bytes.bin", 1000, 0, &all, None)).unwrap();
    assert_eq!((p.name.as_str(), p.file_size, p.offset, p.crc_ok), ("all bytes.bin", 1000, 0, true));
    assert_eq!(p.data, all);

    // the second part of a split file: it says where it goes
    let file = data(3000, 7);
    let body = yenc::encode("film.mkv", 3000, 1200, &file[1200..2400], Some((2, 3)));
    let p = yenc::decode(&body).unwrap();
    assert_eq!((p.offset, p.file_size, p.crc_ok), (1200, 3000, true));
    assert_eq!(p.data, &file[1200..2400]);

    // one byte changed on the way: the part's CRC says so
    let mut bad = body.clone();
    let i = bad.iter().position(|b| *b == b'\n').unwrap() + 1;
    let j = bad[i..].iter().position(|b| *b == b'\n').unwrap() + i + 5;
    bad[j] = if bad[j] == b'a' { b'b' } else { b'a' };
    assert!(!yenc::decode(&bad).unwrap().crc_ok);

    // cut before =yend; not yEnc at all
    let cut = &body[..body.len() / 2];
    assert_eq!(yenc::decode(cut), Err(yenc::Error::Truncated));
    assert_eq!(yenc::decode(b"hello\r\n"), Err(yenc::Error::NotYenc));
}

#[test]
fn nzb_parse_reads_files_segments_and_password() {
    let xml = r#"<?xml version="1.0" encoding="iso-8859-1" ?>
<!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd">
<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb">
 <head>
   <meta type="title">Some Holiday Film</meta>
   <meta type="password">open sesame</meta>
   <meta type="category">Movies</meta>
 </head>
 <file poster="poster@example.invalid" date="1700000000" subject="[1/2] - &quot;holiday.part1.rar&quot; yEnc (1/2)">
  <groups><group>alt.binaries.example</group></groups>
  <segments>
   <segment bytes="1000" number="2">part2of2@example.invalid</segment>
   <segment bytes="1200" number="1">&lt;part1of2@example.invalid&gt;</segment>
  </segments>
 </file>
 <file poster="poster@example.invalid" date="1700000000" subject="holiday.vol03+04.par2 yEnc (1/1)">
  <groups><group>alt.binaries.example</group></groups>
  <segments><segment bytes="500" number="1">par@example.invalid</segment></segments>
 </file>
</nzb>"#;
    let n = crate::parse(xml, "ignored.nzb").unwrap();
    assert_eq!(n.title, "Some Holiday Film");
    assert_eq!(n.password.as_deref(), Some("open sesame"));
    assert_eq!(n.category.as_deref(), Some("Movies"));
    assert_eq!(n.files.len(), 2);
    let f = &n.files[0];
    assert_eq!(f.name, "holiday.part1.rar");
    assert_eq!(f.date, 1_700_000_000);
    assert_eq!(f.groups, vec!["alt.binaries.example"]);
    assert_eq!(f.segments.iter().map(|s| (s.number, s.message_id.as_str())).collect::<Vec<_>>(), vec![(1, "part1of2@example.invalid"), (2, "part2of2@example.invalid")]);
    assert_eq!(n.files[1].name, "holiday.vol03+04.par2");
    assert_eq!(n.files[1].par2_blocks(), Some(4));
    assert_eq!(n.bytes(), 2700);
    // the password never goes into the saved form of the NZB
    assert!(!serde_json::to_string(&n).unwrap().contains("open sesame"));

    // a password in the NZB's own name, no meta tag
    let plain = xml.replace(r#"<meta type="password">open sesame</meta>"#, "").replace(r#"<meta type="title">Some Holiday Film</meta>"#, "");
    let n = crate::parse(&plain, "Holiday {{name pw}}.nzb").unwrap();
    assert_eq!((n.title.as_str(), n.password.as_deref()), ("Holiday", Some("name pw")));
    assert!(crate::parse("<html/>", "x.nzb").is_err());
}

// ------------------------------------------------------------------------------------------------ a fake NNTP server

/// Serves `articles` (message id → body); with `login`, `AUTHINFO` must give it first.
fn fake_server(articles: Vec<(String, Vec<u8>)>, login: Option<(&'static str, &'static str)>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let articles = Arc::new(articles);
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let articles = articles.clone();
            std::thread::spawn(move || {
                let mut w = s.try_clone().unwrap();
                let mut r = BufReader::new(s);
                let _ = w.write_all(b"200 fake news ready\r\n");
                let mut user_ok = false;
                let mut authed = login.is_none();
                let mut line = String::new();
                while r.read_line(&mut line).is_ok_and(|n| n > 0) {
                    let cmd = line.trim_end().to_string();
                    line.clear();
                    let reply: Vec<u8> = if let Some(u) = cmd.strip_prefix("AUTHINFO USER ") {
                        user_ok = login.is_some_and(|(lu, _)| lu == u);
                        b"381 password please\r\n".to_vec()
                    } else if let Some(p) = cmd.strip_prefix("AUTHINFO PASS ") {
                        authed = user_ok && login.is_some_and(|(_, lp)| lp == p);
                        if authed { b"281 welcome\r\n".to_vec() } else { b"481 no\r\n".to_vec() }
                    } else if let Some(id) = cmd.strip_prefix("BODY <").and_then(|r| r.strip_suffix('>')) {
                        if !authed {
                            b"480 sign in first\r\n".to_vec()
                        } else if let Some((_, body)) = articles.iter().find(|(m, _)| m == id) {
                            let mut out = format!("222 0 <{id}>\r\n").into_bytes();
                            for l in body.split_inclusive(|b| *b == b'\n') {
                                if l.starts_with(b".") {
                                    out.push(b'.');
                                }
                                out.extend_from_slice(l);
                            }
                            out.extend_from_slice(b".\r\n");
                            out
                        } else {
                            b"430 no such article\r\n".to_vec()
                        }
                    } else if cmd == "QUIT" {
                        let _ = w.write_all(b"205 bye\r\n");
                        return;
                    } else {
                        b"500 what\r\n".to_vec()
                    };
                    if w.write_all(&reply).is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

struct Direct;

impl Dialer for Direct {
    fn dial(&self, host: &str, port: u16, timeout: Duration) -> Result<Box<dyn Conn>, DialError> {
        dial::connect_direct(host, port, timeout).map_err(DialError::Io)
    }
}

struct Quiet;
impl engine::Host for Quiet {
    fn progress(&self, _: &engine::Progress) {}
}

static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());
struct Capture;
impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        LOGGED.lock().unwrap().push(format!("{}", r.args()));
    }
    fn flush(&self) {}
}

fn all_text_under(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        out.extend(p.to_string_lossy().as_bytes());
        if p.is_file() {
            out.extend(std::fs::read(&p).unwrap());
        }
    }
    out
}

#[test]
fn missing_article_goes_to_backup_and_password_stays_secret() {
    let _ = log::set_logger(&Capture).map(|()| log::set_max_level(log::LevelFilter::Trace));
    const PW: &str = "pw-that-must-not-leak-7731";
    let file = data(10_000, 3);
    let parts: Vec<(String, Vec<u8>)> = (0..4).map(|i| (format!("seg{i}@test.invalid"), yenc::encode("video.mkv", 10_000, i * 2500, &file[i as usize * 2500..(i as usize + 1) * 2500], Some((i as u32 + 1, 4))))).collect();
    // the primary misses part 3; the backup has everything
    let primary: Vec<_> = parts.iter().filter(|(id, _)| !id.starts_with("seg2")).cloned().collect();
    let p1 = fake_server(primary, Some(("reader", PW)));
    let p2 = fake_server(parts.clone(), None);
    let server = |name: &str, port, priority, user: &str, pw: Option<&str>| Server {
        name: name.into(),
        host: "127.0.0.1".into(),
        port,
        tls: false,
        user: user.into(),
        password: pw.map(Password::new),
        connections: 2,
        priority,
        retention_days: 0,
    };
    let servers = vec![server("main", p1, Priority::Primary, "reader", Some(PW)), server("spare", p2, Priority::Backup, "", None)];
    // the segments in a shuffled order: the parts are written where their yEnc header says, whatever order they arrive in
    let order = [3usize, 0, 2, 1];
    let segments: Vec<Segment> = order.iter().map(|&i| Segment { number: i as u32 + 1, bytes: 2600, message_id: parts[i].0.clone() }).collect();
    let dir = temp_dir("fallover");
    let jobs = vec![FileJob { index: 0, name: "video.mkv".into(), date: 0, segments }];
    let mut state = State::default();
    let got = engine::download(&dir, &jobs, &servers, &Direct, &Control::default(), &Quiet, &mut state).unwrap();
    assert_eq!(got[0].failed, 0, "the backup server had the article the primary missed");
    let path = got[0].path.clone().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), file);

    // the password: not in any Debug text, the log, or a file the job left
    let shown = format!("{servers:?}{state:?}");
    assert!(!shown.contains(PW), "password in Debug");
    assert!(LOGGED.lock().unwrap().iter().all(|l| !l.contains(PW)), "password in the log");
    assert!(!String::from_utf8_lossy(&all_text_under(&dir)).contains(PW), "password in a file");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------ par2

fn md5(b: &[u8]) -> [u8; 16] {
    use md5::Digest;
    md5::Md5::digest(b).into()
}

fn packet(set: &[u8; 16], kind: &[u8; 16], body: &[u8]) -> Vec<u8> {
    let mut hashed = set.to_vec();
    hashed.extend_from_slice(kind);
    hashed.extend_from_slice(body);
    let mut p = b"PAR2\0PKT".to_vec();
    p.extend_from_slice(&((64 + body.len()) as u64).to_le_bytes());
    p.extend_from_slice(&md5(&hashed));
    p.extend_from_slice(&hashed);
    p
}

/// A par2 set for `files` with one recovery block of exponent 0 (the XOR of every slice: enough to repair one slice).
fn make_par2(dir: &Path, files: &[(&str, &[u8])], slice: usize) {
    let mut descs = Vec::new();
    for (name, content) in files {
        let h16k = md5(&content[..content.len().min(16384)]);
        let mut idsrc = h16k.to_vec();
        idsrc.extend_from_slice(&(content.len() as u64).to_le_bytes());
        idsrc.extend_from_slice(name.as_bytes());
        descs.push((md5(&idsrc), *name, *content, h16k));
    }
    descs.sort_by_key(|d| d.0);
    let mut main = (slice as u64).to_le_bytes().to_vec();
    main.extend_from_slice(&(descs.len() as u32).to_le_bytes());
    for d in &descs {
        main.extend_from_slice(&d.0);
    }
    let set = md5(&main);
    let mut out = packet(&set, b"PAR 2.0\0Main\0\0\0\0", &main);
    let mut parity = vec![0u8; slice];
    for (id, name, content, h16k) in &descs {
        let mut fd = id.to_vec();
        fd.extend_from_slice(&md5(content));
        fd.extend_from_slice(h16k);
        fd.extend_from_slice(&(content.len() as u64).to_le_bytes());
        let mut n = name.as_bytes().to_vec();
        while n.len() % 4 != 0 {
            n.push(0);
        }
        fd.extend_from_slice(&n);
        out.extend(packet(&set, b"PAR 2.0\0FileDesc", &fd));
        let mut ifsc = id.to_vec();
        for chunk in content.chunks(slice) {
            let mut s = chunk.to_vec();
            s.resize(slice, 0);
            ifsc.extend_from_slice(&md5(&s));
            ifsc.extend_from_slice(&crc32fast::hash(&s).to_le_bytes());
            for (p, b) in parity.iter_mut().zip(&s) {
                *p ^= b;
            }
        }
        out.extend(packet(&set, b"PAR 2.0\0IFSC\0\0\0\0", &ifsc));
    }
    std::fs::write(dir.join("set.par2"), &out).unwrap();
    let mut rec = 0u32.to_le_bytes().to_vec();
    rec.extend_from_slice(&parity);
    std::fs::write(dir.join("set.vol00+01.par2"), packet(&set, b"PAR 2.0\0RecvSlic", &rec)).unwrap();
}

#[test]
fn par2_repairs_a_damaged_small_set() {
    let dir = temp_dir("par2");
    let a = data(150, 1);
    let b = data(100, 9);
    make_par2(&dir, &[("a.bin", &a), ("b.bin", &b)], 64);
    let mut damaged = a.clone();
    damaged[70] ^= 0x5a;
    std::fs::write(dir.join("a.bin"), &damaged).unwrap();
    std::fs::write(dir.join("b.bin"), &b).unwrap();

    let set = crate::par::parse(&crate::par::index_file(&dir).unwrap()).unwrap();
    assert!(!crate::par::quick_ok(&set, &dir, false));
    let v = crate::par::verify(&set, &dir);
    assert!(!v.all_correct());
    assert_eq!(v.blocks_needed(), 1);
    assert!(crate::par::blocks_here(&set, &dir) >= 1);
    assert!(matches!(crate::par::repair(&set, &dir, &v), crate::par::Repaired::Ok(_)));
    assert_eq!(std::fs::read(dir.join("a.bin")).unwrap(), a);
    assert_eq!(std::fs::read(dir.join("b.bin")).unwrap(), b);
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------------------------------------ unpack and place

#[test]
fn unpack_never_writes_outside_the_job_folder() {
    let root = temp_dir("unpack");
    let job = root.join("job");
    std::fs::create_dir_all(&job).unwrap();
    let archive = job.join("evil.zip");
    {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("fine.txt", o).unwrap();
        z.write_all(b"fine").unwrap();
        z.start_file("../escaped.txt", o).unwrap();
        z.write_all(b"escaped").unwrap();
        z.start_file("sub/../../escaped2.txt", o).unwrap();
        z.write_all(b"escaped").unwrap();
        z.finish().unwrap();
    }
    let _ = crate::post::unpack(&archive, &job, None, &mut |_, _| {}, &|| false);
    assert!(!root.join("escaped.txt").exists() && !root.join("escaped2.txt").exists(), "an archive entry got out of the job folder");
    for e in std::fs::read_dir(&root).unwrap().flatten() {
        assert_eq!(e.file_name(), "job", "something was written next to the job folder");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn place_never_replaces_an_existing_folder() {
    let root = temp_dir("place");
    let job = root.join("incomplete").join("Holiday");
    std::fs::create_dir_all(job.join(crate::post::UNPACK_DIR)).unwrap();
    std::fs::write(job.join(crate::post::UNPACK_DIR).join("film.mkv"), b"new").unwrap();
    std::fs::write(job.join(engine::STATE_FILE), b"{}").unwrap();
    let dest = root.join("Movies");
    std::fs::create_dir_all(dest.join("Holiday")).unwrap();
    std::fs::write(dest.join("Holiday").join("film.mkv"), b"old").unwrap();

    // skipped: nothing moves, the old one is as it was
    assert!(crate::post::place(&job, &dest, "Holiday", &|_| crate::post::Taken::Skip).unwrap().is_none());
    assert!(job.join(crate::post::UNPACK_DIR).join("film.mkv").exists());
    // both kept: a new folder beside it
    let to = crate::post::place(&job, &dest, "Holiday", &|_| crate::post::Taken::KeepBoth).unwrap().unwrap();
    assert_eq!(to, dest.join("Holiday (2)"));
    assert_eq!(std::fs::read(to.join("film.mkv")).unwrap(), b"new");
    assert_eq!(std::fs::read(dest.join("Holiday").join("film.mkv")).unwrap(), b"old");
    assert!(!to.join(engine::STATE_FILE).exists(), "leaf's state file is not part of the result");
    let _ = std::fs::remove_dir_all(&root);
}

// ------------------------------------------------------------------------------------------------ the VPN rule

struct NoDirect(dial::Tunnel);

impl dial::Net for NoDirect {
    fn tunnel(&self) -> dial::Tunnel {
        self.0
    }
    fn dial_tunnel(&self, _: &str, _: u16, _: Duration) -> std::io::Result<Box<dyn Conn>> {
        panic!("the tunnel is not up: nothing may be dialed through it")
    }
    fn dial_direct(&self, _: &str, _: u16, _: Duration) -> std::io::Result<Box<dyn Conn>> {
        panic!("a direct NNTP connection was opened while Usenet may only use the VPN")
    }
    fn kill_switch(&self) -> bool {
        false
    }
    fn generation(&self) -> u64 {
        0
    }
    fn waiting_text(&self) -> String {
        "Waiting for the VPN".into()
    }
}

#[test]
fn usenet_only_through_vpn_never_dials_directly() {
    let server = Server { name: "s".into(), host: "news.example.invalid".into(), port: 563, tls: true, user: String::new(), password: None, connections: 8, priority: Priority::Primary, retention_days: 0 };
    for tunnel in [dial::Tunnel::Down, dial::Tunnel::Connecting] {
        // the general settings say "direct" and no kill switch: the Usenet rule still wins
        let d = dial::RouteDialer { net: NoDirect(tunnel), route: dial::Route { vpn_only: true, through_vpn: false } };
        assert!(matches!(d.dial("news.example.invalid", 563, Duration::from_secs(1)), Err(DialError::Wait(_))));
        // the Test button and the download go through the same dialer
        assert!(matches!(crate::nntp::test(&server, &d), Err(crate::nntp::Error::Wait(_))));
        let dir = temp_dir("vpnrule");
        let ctl = Control::default();
        let cancel = ctl.cancel.clone();
        let jobs = vec![FileJob { index: 0, name: "x.bin".into(), date: 0, segments: vec![Segment { number: 1, bytes: 10, message_id: "a@b".into() }] }];
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        // the job waits (it neither fails nor goes direct) until it is cancelled
        assert!(matches!(engine::download(&dir, &jobs, &[server.clone()], &d, &ctl, &Quiet, &mut State::default()), Err(engine::Error::Cancelled)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ------------------------------------------------------------------------------------------------ hostile input

#[test]
fn par2_set_naming_files_outside_the_job_folder_is_refused() {
    let root = temp_dir("par2escape");
    let a = data(150, 1);
    let outside = root.join("abs.bin");
    let absolute = outside.to_string_lossy().into_owned();
    for (tag, name) in [("dots", r"..\x.bin"), ("slash", "../x.bin"), ("abs", absolute.as_str())] {
        let job = root.join(tag);
        std::fs::create_dir_all(&job).unwrap();
        make_par2(&job, &[(name, &a)], 64);
        let set = crate::par::parse(&crate::par::index_file(&job).unwrap()).unwrap();
        assert!(crate::par::check_names(&set, &job).is_err(), "{tag}: the set was taken");
        // the repair itself refuses too, before anything is written
        let v = crate::par::verify(&set, &job);
        assert!(matches!(crate::par::repair(&set, &job, &v), crate::par::Repaired::Failed(_)), "{tag}: repaired");
    }
    assert!(!root.join("x.bin").exists() && !outside.exists(), "a file was written outside the job folder");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn message_id_that_could_carry_commands_is_never_sent() {
    let xml = r#"<nzb><file subject="&quot;a.bin&quot; yEnc (1/3)"><segments>
   <segment bytes="10" number="1">good@example.invalid</segment>
   <segment bytes="10" number="2">evil@example.invalid&#13;&#10;POST</segment>
   <segment bytes="10" number="3">sp ace@example.invalid</segment>
  </segments></file></nzb>"#;
    let n = crate::parse(xml, "x.nzb").unwrap();
    let ids: Vec<&str> = n.files[0].segments.iter().map(|s| s.message_id.as_str()).collect();
    // (kept as segments, so they count as missing; their ids are dropped)
    assert_eq!(ids, vec!["good@example.invalid", "", ""]);
    assert_eq!(crate::nntp::body_command("good@example.invalid").unwrap(), b"BODY <good@example.invalid>\r\n");
    for bad in ["evil@x\r\nPOST", "a\nb", "a>\r\nQUIT\r\n<b", "", "a b", "a\0b", "a\tb"] {
        assert!(matches!(crate::nntp::body_command(bad), Err(crate::nntp::Error::BadId)), "{bad:?} would be sent");
    }
}

struct JobHost;
impl engine::Host for JobHost {
    fn progress(&self, _: &engine::Progress) {}
}
impl crate::job::Host for JobHost {
    fn step(&self, _: crate::job::Step) {}
    fn ask_password(&self, _: &str, _: bool) -> Option<String> {
        None
    }
    fn taken(&self, _: &Path) -> crate::post::Taken {
        crate::post::Taken::Skip
    }
}

#[test]
fn second_job_with_the_same_title_gets_its_own_folder() {
    let inc = temp_dir("sametitle");
    let nzb = |id: &str| crate::parse(&format!(r#"<nzb><head><meta type="title">Holiday</meta></head><file subject="&quot;film.mkv&quot; yEnc (1/1)"><segments><segment bytes="10" number="1">{id}</segment></segments></file></nzb>"#), "Holiday.nzb").unwrap();
    let (first, second) = (nzb("one@example.invalid"), nzb("two@example.invalid"));
    assert_eq!(first.title, second.title);

    // the first job's folder, with its state and a file it keeps
    let a = crate::job::job_dir(&inc, "Holiday", &first);
    std::fs::create_dir_all(&a).unwrap();
    State { nzb: first.fingerprint(), ..Default::default() }.save(&a);
    std::fs::write(a.join("film.mkv"), b"first job's film").unwrap();

    let b = crate::job::job_dir(&inc, "Holiday", &second);
    assert_ne!(a, b, "the second job shares the first one's folder");
    assert_eq!(crate::job::job_dir(&inc, "Holiday", &first), a, "the first job finds its own folder again");

    // run on the first one's folder by mistake: refused, nothing of it adopted or removed
    let spec = |nzb: crate::Nzb, dir: PathBuf| crate::Spec { nzb, job_dir: dir, dest_dir: inc.join("done"), name: "Holiday".into(), unpack: false, cleanup: true };
    assert!(crate::run(&spec(second.clone(), a.clone()), &[], &Direct, &Control::default(), &JobHost).is_err());
    // the second job in its own folder ends (no server) without touching the first's
    let _ = crate::run(&spec(second, b.clone()), &[], &Direct, &Control::default(), &JobHost);
    crate::post::cleanup(&b, true);
    assert_eq!(std::fs::read(a.join("film.mkv")).unwrap(), b"first job's film");
    assert_eq!(State::load(&a).nzb, first.fingerprint());
    let _ = std::fs::remove_dir_all(&inc);
}
