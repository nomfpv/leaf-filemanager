//! The kill switch and the secrets (the brief's tests); no live tunnel (no Mullvad account here).

use super::*;

#[test]
fn kill_switch_refuses_a_direct_connection_while_the_tunnel_is_down() {
    // the routing rule: down + kill switch = refused; LAN always direct; down without it = direct
    assert_eq!(route_with("example.com", false, true, false), Route::Blocked);
    assert_eq!(route_with("example.com", false, false, false), Route::Direct);
    assert_eq!(route_with("example.com", true, true, false), Route::Tunnel);
    assert_eq!(route_with("192.168.1.5", false, true, false), Route::Direct);
    assert_eq!(route_with("nas.local", true, true, false), Route::Direct);
    // connecting: held even without the kill switch (the user asked for the VPN)
    assert_eq!(route_with("example.com", false, false, true), Route::Blocked);

    // and the real call, with no runtime given at all: the refusal comes before any socket is opened (the address is a
    // documentation one the test never reaches)
    set_kill_switch(true);
    let e = dial_blocking("203.0.113.7", 443, Duration::from_secs(1)).unwrap_err();
    assert!(is_blocked(&e), "{e}");
    let wrapped: Box<dyn std::error::Error> = Box::new(e);
    assert!(is_blocked(wrapped.as_ref()));
    set_kill_switch(false);
}

#[test]
fn a_reset_through_the_tunnel_is_an_error_not_the_end_of_the_stream() {
    use smoltcp::socket::tcp::State as S;
    // closed without the other side's FIN: a reset or the stack's timeout
    let e = tunnel::read_end(S::Closed, false).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::ConnectionReset);
    // the other side's FIN, seen now or before the socket went on to Closed: the normal end
    for s in [S::CloseWait, S::LastAck, S::TimeWait, S::Closing] {
        assert!(tunnel::read_end(s, false).is_ok(), "{s:?}");
    }
    assert!(tunnel::read_end(S::Closed, true).is_ok());
}

#[test]
fn an_idle_tunnel_is_not_declared_lost() {
    use std::time::Instant;
    use tunnel::{Liveness, Tick};
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    // ten idle minutes: a ping every 30 s, each one answered: never lost
    let mut live = Liveness::new(t0);
    let mut probes = 0;
    for s in (1..=600).step_by(1) {
        match live.tick(at(s)) {
            Tick::Probe => {
                probes += 1;
                live.on_rx(at(s) + Duration::from_millis(80));
            }
            Tick::Dead => panic!("an answered idle tunnel was declared lost at {s} s"),
            Tick::Fine => {}
        }
    }
    assert!((15..=21).contains(&probes), "{probes} pings in ten minutes");
    // nothing answers any more: lost after three unanswered pings, not before
    let mut live = Liveness::new(t0);
    let mut dead_at = None;
    for s in 1..=300 {
        if live.tick(at(s)) == Tick::Dead {
            dead_at = Some(s);
            break;
        }
    }
    let d = dead_at.expect("a silent server is noticed");
    assert!((100..=130).contains(&d), "lost after {d} s");
}

/// A runtime for the tests, driven on a thread of its own (as leaf-remote's is in the app).
fn test_runtime() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let h = rt.handle().clone();
        std::thread::spawn(move || rt.block_on(std::future::pending::<()>()));
        init(move || h.clone());
    });
}

/// An echo server on 127.0.0.1 (a LAN address: the bridge joins it directly, no tunnel needed).
fn echo() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut r = s.try_clone().unwrap();
                let mut w = s;
                let _ = std::io::copy(&mut r, &mut w);
            });
        }
    });
    port
}

fn round_trip(mut s: std::net::TcpStream) -> Vec<u8> {
    use std::io::{Read, Write};
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"leaf").unwrap();
    let mut b = [0u8; 4];
    s.read_exact(&mut b).unwrap();
    b.to_vec()
}

#[test]
fn the_bridge_serves_implicit_ftps_connection_and_only_its_own_caller() {
    test_runtime();
    let port = echo();
    // implicit FTPS: the client connects by itself to the address (no test connection takes the bridge's one accept first)
    let b = loopback_bridge("127.0.0.1", port, Duration::from_secs(5)).unwrap();
    let addr = b.any_local();
    let s = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
    assert_eq!(round_trip(s), b"leaf");
    // the caller's own socket: another program's connection that comes first is dropped, the caller's is served
    let b = loopback_bridge("127.0.0.1", port, Duration::from_secs(5)).unwrap();
    let addr = b.addr;
    let mut stray = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(5)).unwrap();
    let mine = b.connect(Duration::from_secs(5)).unwrap();
    assert_eq!(round_trip(mine), b"leaf");
    use std::io::Read;
    stray.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut buf = [0u8; 1];
    assert!(matches!(stray.read(&mut buf), Ok(0) | Err(_)), "the stray connection was not served");
}
