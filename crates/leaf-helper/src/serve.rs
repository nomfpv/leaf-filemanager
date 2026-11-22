//! The conversation with leaf, the same on every system once the per-system part has checked who is on the other end: the
//! greeting with the token, then one request at a time (a cancel and the answer to a question may come while one runs).
//! Anything out of order (a second greeting, a request while one runs, a frame that does not parse) ends the conversation, and
//! with it the helper.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use leaf_common::admin::{FRAME_DATA, MAX_REQUEST, PROTOCOL, Refusal, Reply, Request, recv, same_token, send, write_frame};

use crate::exec::{self, Job};

/// When the last request came and whether one runs (the watchdog ends an idle helper).
pub struct Activity {
    last: Mutex<Instant>,
    busy: AtomicBool,
    /// leaf has connected and greeted (until then the watchdog allows only a short wait).
    greeted: AtomicBool,
    started: Instant,
}

impl Default for Activity {
    fn default() -> Self {
        Self { last: Mutex::new(Instant::now()), busy: AtomicBool::new(false), greeted: AtomicBool::new(false), started: Instant::now() }
    }
}

impl Activity {
    fn touch(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    /// The helper should end now: idle too long with nothing running, or leaf did not greet in time.
    pub fn expired(&self) -> bool {
        if !self.greeted.load(Ordering::Relaxed) {
            return self.started.elapsed() > std::time::Duration::from_secs(60);
        }
        !self.busy.load(Ordering::Relaxed) && self.last.lock().unwrap_or_else(|e| e.into_inner()).elapsed() > leaf_common::admin::IDLE_LIMIT
    }
}

struct Running {
    cancel: Arc<AtomicBool>,
    answers: std::sync::mpsc::Sender<Option<Vec<leaf_common::admin::Choice>>>,
    thread: std::thread::JoinHandle<()>,
}

/// Serves one leaf until it says goodbye or its end closes. Returns when the helper should end.
pub fn serve(mut reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>, token: &str, activity: Arc<Activity>) {
    let out = Arc::new(Mutex::new(writer));
    let reply: Arc<dyn Fn(&Reply) -> bool + Send + Sync> = {
        let out = out.clone();
        Arc::new(move |r: &Reply| send(&mut *out.lock().unwrap_or_else(|e| e.into_inner()), r).is_ok())
    };
    let data: Arc<dyn Fn(&[u8]) -> bool + Send + Sync> = {
        let out = out.clone();
        Arc::new(move |b: &[u8]| write_frame(&mut *out.lock().unwrap_or_else(|e| e.into_inner()), FRAME_DATA, b).is_ok())
    };
    // the greeting: the token from the command line, and the same protocol
    match recv::<Request>(&mut reader, MAX_REQUEST) {
        Ok(Request::Hello { token: t, protocol }) if same_token(&t, token) => {
            if protocol != PROTOCOL {
                reply(&Reply::Refused { why: Refusal::Protocol });
                return;
            }
            activity.greeted.store(true, Ordering::Relaxed);
            activity.touch();
            if !reply(&Reply::Welcome { pid: std::process::id() }) {
                return;
            }
        }
        _ => {
            reply(&Reply::Refused { why: Refusal::NotGreeted });
            return;
        }
    }
    let mut running: Option<Running> = None;
    loop {
        let req = match recv::<Request>(&mut reader, MAX_REQUEST) {
            Ok(r) => r,
            // leaf's end closed, or sent something that is not a request
            Err(_) => break,
        };
        activity.touch();
        if running.as_ref().is_some_and(|r| r.thread.is_finished()) {
            if let Some(r) = running.take() {
                let _ = r.thread.join();
            }
        }
        match req {
            Request::Cancel => {
                if let Some(r) = &running {
                    r.cancel.store(true, Ordering::Relaxed);
                }
            }
            Request::Answer { choices } => {
                if let Some(r) = &running {
                    let _ = r.answers.send(choices);
                }
            }
            Request::Bye | Request::Hello { .. } => break,
            op => {
                if running.is_some() {
                    // one at a time: leaf never sends a second request while one runs, so this is not leaf
                    break;
                }
                let cancel = Arc::new(AtomicBool::new(false));
                let (tx, rx) = std::sync::mpsc::channel();
                let job = Job { cancel: cancel.clone(), send: reply.clone(), send_data: data.clone(), answers: Arc::new(Mutex::new(rx)) };
                let (reply, activity) = (reply.clone(), activity.clone());
                activity.busy.store(true, Ordering::Relaxed);
                let thread = std::thread::Builder::new().name(format!("helper-{}", op.label())).spawn(move || {
                    crate::os::thread_start();
                    let r = exec::run(&op, &job);
                    reply(&r);
                    activity.busy.store(false, Ordering::Relaxed);
                    activity.touch();
                });
                match thread {
                    Ok(thread) => running = Some(Running { cancel, answers: tx, thread }),
                    Err(_) => break,
                }
            }
        }
    }
    // the end: what runs is stopped (the engines look at the cancel flag between files)
    if let Some(r) = running {
        r.cancel.store(true, Ordering::Relaxed);
        let _ = r.thread.join();
    }
}
