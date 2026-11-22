//! Usenet (NZB) downloads as Status Center jobs next to the web downloads: an NZB opened (double click, "Download with
//! leaf", a drop, `leaf download x.nzb`, the browser extension, the watch folder) becomes a job card with its name, size,
//! speed, time left, the share of failed articles and the step it is at; the queue runs one job at a time by priority, follows
//! the download schedule and the speed limit, and can be paused as a whole. The engine is `leaf_nzb`; this file is leaf's
//! face for it: wording, settings, dialogs, the keyring.
//!
//! The owner's rule: with "Usenet only while the VPN is on" (on by default) every NNTP connection goes through leaf's VPN
//! and nowhere else; while the tunnel is down the jobs wait as "Waiting for the VPN" and go on by themselves.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use gpui_kit::assets::IconName as L;
use gpui_kit::component::h_flex;
use gpui_kit::*;
use leaf_nzb::queue::JobPriority;
use leaf_nzb::{Priority, Step};
use leaf_sys::fastcopy::{Conflict, Resolve};
use leaf_sys::fileops::Tracker;
use serde::{Deserialize, Serialize};

use crate::prompt::{self, Button, Kind};
use crate::settings::{self, Settings};
use crate::settings_view::{button, card_body, shell, sw, switch};
use crate::t;
use crate::theme::Palette;

// ---------------------------------------------------------------------------------------------------------------------------------
// the servers in the settings

/// A Usenet server as the settings keep it. The password is never here: it is in the system's keyring ([`secret_id`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsenetServer {
    pub host: String,
    pub port: u16,
    /// NNTPS; plain NNTP only when the user ticked it (with the warning).
    pub tls: bool,
    pub user: String,
    pub connections: u32,
    pub priority: Priority,
    /// Days the server keeps articles (0: unknown).
    pub retention_days: u32,
    pub enabled: bool,
}

impl Default for UsenetServer {
    fn default() -> Self {
        Self { host: String::new(), port: 563, tls: true, user: String::new(), connections: leaf_nzb::server::DEFAULT_CONNECTIONS, priority: Priority::Primary, retention_days: 0, enabled: true }
    }
}

fn priority_word(p: Priority) -> &'static str {
    match p {
        Priority::Primary => "primary",
        Priority::Backup => "backup",
        Priority::Fill => "fill",
    }
}

fn priority_text(p: Priority) -> String {
    match p {
        Priority::Primary => t!("Primary").to_string(),
        Priority::Backup => t!("Backup").to_string(),
        Priority::Fill => t!("Fill").to_string(),
    }
}

impl UsenetServer {
    /// "host:port; user; connections; priority; retention days" as the edit box shows it.
    pub fn to_line(&self) -> String {
        format!("{}:{}; {}; {}; {}; {}", self.host, self.port, self.user, self.connections, priority_word(self.priority), self.retention_days)
    }

    /// The edit box's text back (`tls` from the dialog's tick). `None` when the host is missing or a number is not one.
    pub fn from_line(line: &str, tls: bool) -> Option<Self> {
        let mut parts = line.split(';').map(str::trim);
        let hostport = parts.next().filter(|h| !h.is_empty())?;
        let (host, port) = match hostport.rsplit_once(':') {
            Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h.to_string(), p.parse().ok().filter(|p| *p > 0)?),
            _ => (hostport.to_string(), leaf_nzb::Server::default_port(tls)),
        };
        if host.is_empty() || host.contains(['/', ' ', '@']) {
            return None;
        }
        let user = parts.next().unwrap_or("").to_string();
        let connections = match parts.next().filter(|c| !c.is_empty()) {
            Some(c) => c.parse::<u32>().ok().filter(|c| (1..=leaf_nzb::server::MAX_CONNECTIONS).contains(c))?,
            None => leaf_nzb::server::DEFAULT_CONNECTIONS,
        };
        let priority = match parts.next().map(str::to_ascii_lowercase).as_deref() {
            None | Some("") | Some("primary") | Some("1") => Priority::Primary,
            Some("backup") | Some("2") => Priority::Backup,
            Some("fill") | Some("3") => Priority::Fill,
            _ => return None,
        };
        let retention_days = match parts.next().filter(|r| !r.is_empty()) {
            Some(r) => r.parse().ok()?,
            None => 0,
        };
        Some(Self { host, port, tls, user, connections, priority, retention_days, enabled: true })
    }
}

/// Where a server's password is kept.
pub fn secret_id(s: &UsenetServer) -> leaf_remote::SecretId {
    leaf_remote::SecretId::password("nntp", &s.user, &s.host, s.port)
}

/// The engine's server, with the password read from the keyring now (on the job's own thread: a keyring can wait for the user).
fn engine_server(s: &UsenetServer) -> leaf_nzb::Server {
    let password = (!s.user.is_empty()).then(|| leaf_remote::vault().get(&secret_id(s))).flatten().map(|p| leaf_nzb::Password::new(p.as_str()));
    leaf_nzb::Server { name: s.host.clone(), host: s.host.clone(), port: s.port, tls: s.tls, user: s.user.clone(), password, connections: s.connections.clamp(1, leaf_nzb::server::MAX_CONNECTIONS), priority: s.priority, retention_days: s.retention_days }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the route: leaf's VPN

struct VpnConn(leaf_vpn::BlockingStream);

impl std::io::Read for VpnConn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl std::io::Write for VpnConn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl leaf_nzb::dial::Conn for VpnConn {
    fn set_read_timeout(&mut self, t: Option<Duration>) {
        self.0.set_read_timeout(t);
    }
    fn set_write_timeout(&mut self, t: Option<Duration>) {
        self.0.set_write_timeout(t);
    }
}

/// leaf's VPN as the Usenet route rule sees it.
pub struct VpnNet;

impl leaf_nzb::dial::Net for VpnNet {
    fn tunnel(&self) -> leaf_nzb::dial::Tunnel {
        if leaf_vpn::tunnel().is_some() {
            leaf_nzb::dial::Tunnel::Up
        } else if matches!(leaf_vpn::state(), leaf_vpn::State::Connecting { .. }) {
            leaf_nzb::dial::Tunnel::Connecting
        } else {
            leaf_nzb::dial::Tunnel::Down
        }
    }
    fn dial_tunnel(&self, host: &str, port: u16, timeout: Duration) -> std::io::Result<Box<dyn leaf_nzb::dial::Conn>> {
        leaf_vpn::dial_tunnel_blocking(host, port, timeout).map(|s| Box::new(VpnConn(s)) as Box<dyn leaf_nzb::dial::Conn>)
    }
    fn dial_direct(&self, host: &str, port: u16, timeout: Duration) -> std::io::Result<Box<dyn leaf_nzb::dial::Conn>> {
        leaf_nzb::dial::connect_direct(host, port, timeout)
    }
    fn kill_switch(&self) -> bool {
        leaf_vpn::kill_switch()
    }
    fn generation(&self) -> u64 {
        leaf_vpn::generation()
    }
    fn waiting_text(&self) -> String {
        t!("Waiting for the VPN").to_string()
    }
}

/// The route settings of now, for every dial of every job (a change applies to the next connection; [`set_route`] also
/// closes the open ones).
static VPN_ONLY: AtomicBool = AtomicBool::new(true);
static THROUGH_VPN: AtomicBool = AtomicBool::new(false);
static ROUTE_GEN: AtomicU64 = AtomicU64::new(0);

/// Takes the settings' route; when it changed, the running jobs' connections are closed and dialled again under it.
fn set_route(s: &Settings) {
    let a = VPN_ONLY.swap(s.usenet_vpn_only, Ordering::AcqRel);
    let b = THROUGH_VPN.swap(s.vpn_downloads, Ordering::AcqRel);
    if a != s.usenet_vpn_only || b != s.vpn_downloads {
        ROUTE_GEN.fetch_add(1, Ordering::AcqRel);
    }
}

pub struct LiveRoute;

impl leaf_nzb::dial::RouteSource for LiveRoute {
    fn route(&self) -> leaf_nzb::dial::Route {
        leaf_nzb::dial::Route { vpn_only: VPN_ONLY.load(Ordering::Acquire), through_vpn: THROUGH_VPN.load(Ordering::Acquire) }
    }
    fn generation(&self) -> u64 {
        ROUTE_GEN.load(Ordering::Acquire)
    }
}

fn dialer(s: &Settings) -> leaf_nzb::dial::RouteDialer<VpnNet, LiveRoute> {
    set_route(s);
    leaf_nzb::dial::RouteDialer { net: VpnNet, route: LiveRoute }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the saved queue and history

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedJob {
    key: u64,
    /// leaf's own copy of the NZB (named without the `{{password}}` its name may carry: that one is in the keyring).
    nzb: PathBuf,
    /// The job's own folder in the incomplete folder, once it has one (two jobs with one title never share it).
    folder: Option<PathBuf>,
    /// Not written to `usenet.json` (leaf could not make its own copy of an NZB whose name holds a password).
    #[serde(skip)]
    transient: bool,
    title: String,
    /// The folder the result goes into.
    dest: PathBuf,
    /// The category from the NZB sorts it into a subfolder.
    sort: bool,
    priority: JobPriority,
    paused: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct PastJob {
    /// The job's key: its archive password (when it had one) waits in the keyring under it until the entry goes.
    key: u64,
    title: String,
    ok: bool,
    when: u64,
    folder: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Store {
    active: Vec<SavedJob>,
    history: Vec<PastJob>,
}

const HISTORY: usize = 100;

static STORE: LazyLock<Mutex<Store>> = LazyLock::new(|| Mutex::new(store_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()));
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

fn store_path() -> Option<PathBuf> {
    settings::app_dir().map(|d| d.join("usenet.json"))
}

fn nzb_dir() -> Option<PathBuf> {
    settings::app_dir().map(|d| d.join("usenet"))
}

fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

fn persist() {
    let snapshot = {
        let live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
        let mut st = store();
        for s in &mut st.active {
            if let Some(l) = live.iter().find(|l| l.key == s.key) {
                s.paused = l.paused.load(Ordering::Relaxed) && !l.by_schedule;
                s.priority = l.priority;
            }
        }
        let mut snapshot = st.clone();
        snapshot.active.retain(|a| !a.transient);
        snapshot
    };
    if let Some(path) = store_path() {
        std::thread::spawn(move || {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(b) = serde_json::to_vec_pretty(&snapshot) {
                let tmp = path.with_extension("json.tmp");
                if std::fs::write(&tmp, b).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
        });
    }
}

pub fn history_len() -> usize {
    store().history.len()
}

pub fn clear_history() {
    let gone = std::mem::take(&mut store().history);
    drop_passwords(&gone);
    persist();
}

// ---------------------------------------------------------------------------------------------------------------------------------
// running jobs

struct Live {
    key: u64,
    title: String,
    paused: Arc<AtomicBool>,
    limit: Arc<AtomicU64>,
    /// The schedule (or "pause all") paused it, not the user's own pause button.
    by_schedule: bool,
    priority: JobPriority,
}

static LIVE: Mutex<Vec<Live>> = Mutex::new(Vec::new());

pub fn live_count() -> usize {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Whether `p` is an NZB file.
pub fn is_nzb(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("nzb")) && p.is_file()
}

/// The schedule opens or closes the queue: running jobs are paused while it is closed (the ones the user paused stay paused).
pub fn schedule_gate(open: bool) {
    gate(open && !leaf_nzb::queue::all_paused());
}

fn gate(open: bool) {
    for l in LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
        if open {
            if l.by_schedule {
                l.paused.store(false, Ordering::Relaxed);
                l.by_schedule = false;
            }
        } else if !l.paused.load(Ordering::Relaxed) {
            l.paused.store(true, Ordering::Relaxed);
            l.by_schedule = true;
        }
    }
}

/// "Pause all" / "Resume all" for the Usenet queue.
pub fn pause_all(on: bool) {
    leaf_nzb::queue::pause_all(on);
    gate(!on && leaf_download::queue::is_open());
}

/// The speed limit or the route changed in the settings: running jobs take it at once.
pub fn settings_changed(cx: &App) {
    set_route(cx.global::<Settings>());
    let limit = u64::from(cx.global::<Settings>().download_speed_limit) << 20;
    for l in LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        l.limit.store(limit, Ordering::Relaxed);
    }
}

/// Opens an NZB as a Usenet job into the download folder (sorted by the NZB's category when sorting is on).
pub fn open_nzb(path: PathBuf, cx: &mut App) {
    let dest = crate::downloads::default_folder(cx.global::<Settings>());
    start(path, dest, true, cx);
}

/// Opens an NZB as a Usenet job whose result goes into `dest`.
pub fn start(path: PathBuf, dest: PathBuf, sort: bool, cx: &mut App) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "job.nzb".into());
    start_named(path, name, dest, sort, cx);
}

/// Where the archive password from an NZB's file name waits for its job (a restart included).
fn job_secret(key: u64) -> leaf_remote::SecretId {
    leaf_remote::SecretId::password("nzb", "archive", &format!("job-{key}"), 0)
}

/// The archive passwords of the jobs of this run (the keyring keeps them across a restart).
static PASSWORDS: LazyLock<Mutex<std::collections::HashMap<u64, String>>> = LazyLock::new(Default::default);

fn job_password(key: u64) -> Option<String> {
    if let Some(p) = PASSWORDS.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Some(p.clone());
    }
    leaf_remote::vault().get(&job_secret(key)).map(|p| p.as_str().to_string())
}

fn forget_job_password(key: u64) {
    if PASSWORDS.lock().unwrap_or_else(|e| e.into_inner()).remove(&key).is_some() {
        std::thread::spawn(move || {
            let _ = leaf_remote::vault().forget(&job_secret(key));
        });
    }
}

/// Entries that left the history: a job that failed and was never retried keeps its archive password in the keyring, and nothing
/// would ever ask for it again.
fn drop_passwords(gone: &[PastJob]) {
    // (not a job that was retried and runs again: its password is still needed)
    let live: Vec<u64> = LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|l| l.key).collect();
    let keys: Vec<u64> = gone.iter().map(|g| g.key).filter(|k| *k != 0 && !live.contains(k)).collect();
    if keys.is_empty() {
        return;
    }
    for k in &keys {
        PASSWORDS.lock().unwrap_or_else(|e| e.into_inner()).remove(k);
    }
    // (on its own thread: a keyring can wait for the user)
    std::thread::spawn(move || {
        for k in keys {
            let _ = leaf_remote::vault().forget(&job_secret(k));
        }
    });
}

/// [`start`] for an NZB whose file name is `name` (the watch folder renames what it takes).
fn start_named(path: PathBuf, name: String, dest: PathBuf, sort: bool, cx: &mut App) {
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    // (leaf keeps its own copy: the job survives a restart even when the NZB was a temporary download. The copy's name
    // has no `{{password}}`: that goes to the keyring, never into usenet.json)
    let (clean, password) = leaf_nzb::nzb::split_password(&name);
    let clean = if clean.is_empty() { "job.nzb".to_string() } else { clean };
    let copy = nzb_dir().map(|d| d.join(key.to_string()).join(&clean));
    let copied = copy.as_ref().filter(|c| {
        let _ = std::fs::create_dir_all(c.parent().unwrap_or(c));
        std::fs::copy(&path, c).is_ok()
    });
    let nzb = copied.cloned().unwrap_or_else(|| path.clone());
    let transient = copied.is_none() && password.is_some();
    if let Some(pw) = password {
        PASSWORDS.lock().unwrap_or_else(|e| e.into_inner()).insert(key, pw.clone());
        // (on its own thread: a keyring can wait for the user)
        std::thread::spawn(move || {
            let _ = leaf_remote::vault().keep(&job_secret(key), &leaf_remote::Secret::new(pw));
        });
    }
    let title = clean.strip_suffix(".nzb").or_else(|| clean.strip_suffix(".NZB")).unwrap_or(&clean).trim().to_string();
    let job = SavedJob { key, nzb, folder: None, transient, title, dest, sort, priority: JobPriority::Normal, paused: false };
    store().active.push(job.clone());
    persist();
    run_job(job, cx);
}

/// The jobs that were not finished when leaf was closed, started again (each resumes from its folder).
pub fn restore(cx: &mut App) {
    let saved = std::mem::take(&mut store().active);
    for s in &saved {
        NEXT_KEY.fetch_max(s.key + 1, Ordering::Relaxed);
    }
    store().active = saved.clone();
    for s in saved.into_iter().filter(|s| s.nzb.exists()) {
        run_job(s, cx);
    }
}

fn run_job(job: SavedJob, cx: &mut App) {
    crate::download_schedule::note_started();
    let s = cx.global::<Settings>().clone();
    let key = job.key;
    let live = Live { key, title: job.title.clone(), paused: Arc::default(), limit: Arc::new(AtomicU64::new(u64::from(s.download_speed_limit) << 20)), by_schedule: false, priority: job.priority };
    let limit = live.limit.clone();
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).push(live);
    leaf_nzb::queue::add(key, job.priority);
    let title = format!("{} {}", t!("Usenet:"), job.title);
    let done_folder: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let failed = Arc::new(AtomicBool::new(false));
    let id_cell = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let (work_job, got, fail_flag) = (job.clone(), done_folder.clone(), failed.clone());
    let on_done: crate::status_center::OnDone = {
        let id_cell = id_cell.clone();
        let job = job.clone();
        Box::new(move |ok, cx| {
            let id = id_cell.get();
            let folder = done_folder.lock().unwrap_or_else(|e| e.into_inner()).take();
            let was_failed = failed.load(Ordering::Relaxed);
            LIVE.lock().unwrap_or_else(|e| e.into_inner()).retain(|l| l.key != key);
            {
                let mut st = store();
                st.active.retain(|a| a.key != key);
                if ok || was_failed {
                    let when = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                    st.history.insert(0, PastJob { key, title: job.title.clone(), ok, when, folder: folder.clone() });
                    let keep = st.history.len().min(HISTORY);
                    let dropped = st.history.split_off(keep);
                    drop_passwords(&dropped);
                }
            }
            persist();
            if ok {
                forget_job_password(key);
                // (leaf's copy of the NZB is not needed any more)
                if job.nzb.starts_with(nzb_dir().unwrap_or_default()) {
                    let _ = std::fs::remove_file(&job.nzb);
                    if let Some(d) = job.nzb.parent() {
                        let _ = std::fs::remove_dir(d);
                    }
                }
                crate::download_schedule::note_finished();
                crate::status_center::set_result(cx, id, Some(format!("{} {}", t!("Downloaded"), job.title)), None);
                if let Some(f) = &folder {
                    crate::status_center::set_extras(cx, id, None, Some(f.clone()), None);
                    crate::workspace::select_in_open_folder(f, cx);
                }
            } else if was_failed {
                let retry: crate::status_center::ResultAction = (
                    t!("Retry").to_string(),
                    std::rc::Rc::new(move |cx: &mut App| {
                        crate::status_center::remove(cx, id);
                        store().active.push(SavedJob { paused: false, ..job.clone() });
                        persist();
                        run_job(SavedJob { paused: false, ..job.clone() }, cx);
                    }),
                );
                crate::status_center::set_result(cx, id, None, Some(retry));
            }
        })
    };
    let id = crate::status_center::start_with_id(cx, title, move |tr| work(&tr, &work_job, &s, &limit, &got, &fail_flag), Some(on_done));
    id_cell.set(id);
    if id != 0 {
        let queue: crate::status_center::ResultAction = (t!("Queue…").to_string(), std::rc::Rc::new(move |cx: &mut App| queue_dialog(key, cx)));
        crate::status_center::set_live_action(cx, id, queue);
    }
}

/// The job card's "Queue…": its place and priority, and "pause all".
fn queue_dialog(key: u64, cx: &mut App) {
    let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) else { return };
    let (title, priority) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|l| l.key == key).map(|l| (l.title.clone(), l.priority)).unwrap_or_default();
    let place = leaf_nzb::queue::place(key);
    let set_priority = move |p: JobPriority| {
        leaf_nzb::queue::set_priority(key, p);
        if let Some(l) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|l| l.key == key) {
            l.priority = p;
        }
        persist();
    };
    let _ = w.update(cx, move |_, window, cx| {
        let prio = match priority {
            JobPriority::High => t!("High"),
            JobPriority::Normal => t!("Normal"),
            JobPriority::Low => t!("Low"),
        };
        let mut lines = vec![format!("{} {prio}", t!("Priority:"))];
        lines.push(match place {
            Some(n) => t!("Place in the queue: {n}").replace("{n}", &n.to_string()),
            None => t!("Downloading now, or not waiting in the queue.").to_string(),
        });
        let all = leaf_nzb::queue::all_paused();
        let buttons = vec![
            Button::new(t!("Move up"), Kind::Normal, move |_, _, _| {
                leaf_nzb::queue::move_by(key, true);
                Ok(())
            }),
            Button::new(t!("Move down"), Kind::Normal, move |_, _, _| {
                leaf_nzb::queue::move_by(key, false);
                Ok(())
            }),
            Button::new(t!("High priority"), Kind::Normal, move |_, _, _| {
                set_priority(JobPriority::High);
                Ok(())
            }),
            Button::new(t!("Low priority"), Kind::Normal, move |_, _, _| {
                set_priority(if priority == JobPriority::Low { JobPriority::Normal } else { JobPriority::Low });
                Ok(())
            }),
            Button::new(if all { t!("Resume all") } else { t!("Pause all") }, Kind::Normal, move |_, _, _| {
                pause_all(!all);
                Ok(())
            }),
            Button::cancel(),
        ];
        prompt::show(title, Some(lines.join("\n")), None, buttons, window, cx);
    });
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the job's thread

fn step_text(s: Step) -> String {
    match s {
        Step::Download => t!("Downloading"),
        Step::QuickCheck => t!("Quick check"),
        Step::Verify => t!("Verifying"),
        Step::Recovery => t!("Fetching recovery blocks"),
        Step::Repair => t!("Repairing"),
        Step::Unpack => t!("Unpacking"),
        Step::Join => t!("Joining"),
        Step::Cleanup => t!("Cleaning up"),
        Step::Move => t!("Moving into place"),
    }
    .to_string()
}

fn size_text(b: u64) -> String {
    let (v, u) = if b >= 1 << 30 { (b as f64 / (1u64 << 30) as f64, "GB") } else { (b as f64 / (1u64 << 20) as f64, "MB") };
    format!("{v:.1} {u}")
}

struct Link<'a> {
    tr: &'a Tracker,
    step: Mutex<Step>,
    label: Mutex<String>,
    total: AtomicU64,
}

impl Link<'_> {
    fn say(&self, label: String) {
        let mut last = self.label.lock().unwrap_or_else(|e| e.into_inner());
        if *last != label {
            *last = label.clone();
            drop(last);
            (self.tr.on_progress)(0, 0, Some(label));
        }
    }
}

impl leaf_nzb::engine::Host for Link<'_> {
    fn progress(&self, p: &leaf_nzb::engine::Progress) {
        self.total.store(p.total_bytes, Ordering::Relaxed);
        self.tr.bytes.set(p.done_bytes, p.total_bytes);
        let permille = if p.total_bytes > 0 { (p.done_bytes as f64 / p.total_bytes as f64 * 1000.0) as u32 } else { 0 };
        let step = step_text(*self.step.lock().unwrap_or_else(|e| e.into_inner()));
        let mut parts = vec![size_text(p.total_bytes), step];
        if p.articles_failed > 0 && p.articles_total > 0 {
            parts.push(format!("{:.1} % {}", p.articles_failed as f64 * 100.0 / p.articles_total as f64, t!("articles failed")));
        }
        if p.connections > 0 {
            parts.push(format!("{} {}", p.connections, t!("connections")));
        }
        let label = match &p.waiting {
            Some(w) if p.connections == 0 => w.clone(),
            _ => parts.join(" · "),
        };
        let mut last = self.label.lock().unwrap_or_else(|e| e.into_inner());
        let name = (*last != label).then(|| label.clone());
        *last = label;
        drop(last);
        (self.tr.on_progress)(1000, permille, name);
    }

    fn room(&self, need: u64, dir: &Path) -> bool {
        let free = leaf_sys::fileattr::free_space(dir);
        free == 0 || need <= free.saturating_sub(leaf_vfs::copy::KEEP_FREE)
    }
}

impl leaf_nzb::Host for Link<'_> {
    fn step(&self, step: Step) {
        *self.step.lock().unwrap_or_else(|e| e.into_inner()) = step;
        let total = self.total.load(Ordering::Relaxed);
        self.say(format!("{} · {}", size_text(total), step_text(step)));
    }

    fn ask_password(&self, archive: &str, wrong: bool) -> Option<String> {
        ask_password(self.tr, archive, wrong)
    }

    fn taken(&self, path: &Path) -> leaf_nzb::post::Taken {
        let conflict = Conflict {
            src: PathBuf::from(path.file_name().unwrap_or_default()),
            dst: path.to_path_buf(),
            src_dir: true,
            dst_dir: true,
            src_size: 0,
            dst_size: 0,
            src_time: None,
            dst_time: std::fs::metadata(path).ok().and_then(|m| m.modified().ok()),
        };
        // (a finished job never merges into or replaces what is there: "replace" keeps both too)
        match self.tr.ask.as_ref().and_then(|ask| ask(std::slice::from_ref(&conflict))).and_then(|v| v.first().copied()) {
            Some(Resolve::Skip) | None => leaf_nzb::post::Taken::Skip,
            Some(_) => leaf_nzb::post::Taken::KeepBoth,
        }
    }

    fn step_progress(&self, done: u64, total: u64) {
        if total > 0 {
            (self.tr.on_progress)(1000, (done.min(total) * 1000 / total) as u32, None);
        }
    }
}

/// Where a job's files wait until they are complete.
fn incomplete_dir(s: &Settings) -> PathBuf {
    let custom = s.usenet_incomplete_folder.trim();
    if custom.is_empty() { crate::downloads::default_folder(s).join("incomplete") } else { PathBuf::from(custom) }
}

fn work(tr: &Tracker, job: &SavedJob, s: &Settings, limit: &Arc<AtomicU64>, folder: &Arc<Mutex<Option<PathBuf>>>, failed: &AtomicBool) -> anyhow::Result<bool> {
    if let Some(l) = LIVE.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|l| l.key == job.key) {
        l.paused = tr.paused.clone();
    }
    if job.paused {
        tr.paused.store(true, Ordering::Relaxed);
        (tr.on_progress)(0, 0, Some(t!("Paused").to_string()));
        while tr.paused.load(Ordering::Relaxed) && !tr.cancel.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(150));
        }
    }
    let Some(_turn) = leaf_nzb::queue::wait_turn(job.key, &tr.cancel, &leaf_download::queue::is_open, |place| {
        let text = match place {
            Some(n) => format!("{} ({n})", t!("Waiting for the Usenet downloads before it")),
            None if leaf_nzb::queue::all_paused() => t!("Paused (all Usenet downloads)").to_string(),
            None => t!("Waiting for the scheduled time").to_string(),
        };
        (tr.on_progress)(0, 0, Some(text))
    }) else {
        return Ok(false);
    };
    let fail = |msg: String| -> anyhow::Result<bool> {
        failed.store(true, Ordering::Relaxed);
        Err(anyhow::anyhow!(msg))
    };
    let mut nzb = match leaf_nzb::read(&job.nzb) {
        Ok(n) => n,
        Err(e) => return fail(format!("{}\n{e}", t!("This is not an NZB file leaf can read"))),
    };
    if nzb.password.is_none() {
        nzb.password = job_password(job.key);
    }
    let servers: Vec<leaf_nzb::Server> = s.usenet_servers.iter().filter(|v| v.enabled && !v.host.trim().is_empty()).map(engine_server).collect();
    if servers.is_empty() {
        return fail(t!("No Usenet server is set up. Add one in Settings > Downloads > Usenet servers.").to_string());
    }
    let name = leaf_nzb::nzb::safe_name(if nzb.title.is_empty() { &job.title } else { &nzb.title });
    let name = if name.is_empty() { format!("usenet-{}", job.key) } else { name };
    let category = nzb.category.as_deref().map(leaf_nzb::nzb::safe_name).filter(|c| !c.is_empty() && job.sort && s.download_sort);
    let dest_dir = match category {
        Some(c) => job.dest.join(c),
        None => job.dest.clone(),
    };
    // (its own folder: another NZB with the same title never shares it; kept for the job so a restart finds it)
    let job_dir = job.folder.clone().unwrap_or_else(|| leaf_nzb::job::job_dir(&incomplete_dir(s), &name, &nzb));
    if job.folder.is_none() {
        if let Some(a) = store().active.iter_mut().find(|a| a.key == job.key) {
            a.folder = Some(job_dir.clone());
        }
        persist();
    }
    let spec = leaf_nzb::Spec { nzb, job_dir: job_dir.clone(), dest_dir, name, unpack: s.usenet_unpack, cleanup: s.usenet_cleanup };
    let ctl = leaf_nzb::engine::Control { cancel: tr.cancel.clone(), paused: tr.paused.clone(), limit: limit.clone() };
    let link = Link { tr, step: Mutex::new(Step::Download), label: Mutex::new(String::new()), total: AtomicU64::new(spec.nzb.bytes()) };
    (tr.on_progress)(0, 0, Some(t!("Connecting").to_string()));
    let d = dialer(s);
    match leaf_nzb::run(&spec, &servers, &d, &ctl, &link) {
        Ok(done) => {
            match done.folder {
                Some(f) => *folder.lock().unwrap_or_else(|e| e.into_inner()) = Some(f),
                None => (tr.on_progress)(0, 0, Some(format!("{} {}", t!("Kept in"), job_dir.display()))),
            }
            Ok(true)
        }
        Err(leaf_nzb::Error::Cancelled) => Ok(false),
        Err(e) => {
            log::warn!("usenet: job {} failed: {e}", job.key);
            fail(fail_text(&e, &job_dir))
        }
    }
}

/// leaf's words for a failed job; the engine's own text only on the second line.
fn fail_text(e: &leaf_nzb::Error, kept: &Path) -> String {
    use leaf_nzb::engine::Error as D;
    let kept_line = format!("{} {}", t!("What was downloaded is kept in"), kept.display());
    let (what, detail) = match e {
        leaf_nzb::Error::Download(D::NoServers) => (t!("No Usenet server is set up. Add one in Settings > Downloads > Usenet servers.").to_string(), None),
        leaf_nzb::Error::Download(D::Crashed) => (t!("The download stopped because of an error in leaf").to_string(), None),
        leaf_nzb::Error::Download(D::NoRoom(_)) => (t!("There is not enough free space on this PC for this download").to_string(), None),
        leaf_nzb::Error::Download(D::Unreachable(d)) => (t!("No Usenet server could be used: the sign-in was refused or the server did not answer").to_string(), Some(d.clone())),
        leaf_nzb::Error::Download(D::Io(d)) => (t!("The files could not be written in the incomplete folder (is the disk full?)").to_string(), Some(d.to_string())),
        leaf_nzb::Error::Download(D::Cancelled) | leaf_nzb::Error::Cancelled => (t!("Cancelled").to_string(), None),
        leaf_nzb::Error::Damaged { failed, detail } => (t!("{n} articles are on no server and the par2 files could not repair what they miss.").replace("{n}", &failed.to_string()), Some(detail.clone())),
        leaf_nzb::Error::Password { wrong: true } => (t!("The archive's password is wrong").to_string(), None),
        leaf_nzb::Error::Password { wrong: false } => (t!("The archive needs a password").to_string(), None),
        leaf_nzb::Error::Unpack(d) => (t!("The archive could not be unpacked").to_string(), Some(d.clone())),
        leaf_nzb::Error::Io(d) => (t!("The result could not be moved into its folder").to_string(), Some(d.to_string())),
    };
    match detail {
        Some(d) if !d.trim().is_empty() => format!("{what} {kept_line}\n{d}"),
        _ => format!("{what} {kept_line}"),
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// questions on the UI thread (the archive's password)

type Ask = Box<dyn FnOnce(&mut Window, &mut App) + Send>;

static ASKS: LazyLock<(futures::channel::mpsc::UnboundedSender<Ask>, Mutex<Option<futures::channel::mpsc::UnboundedReceiver<Ask>>>)> = LazyLock::new(|| {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    (tx, Mutex::new(Some(rx)))
});

/// Asks for the archive's password in leaf's password dialog and waits for it (never logged, never kept).
fn ask_password(tr: &Tracker, archive: &str, wrong: bool) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel::<Option<String>>();
    let archive = archive.to_string();
    (tr.on_progress)(0, 0, Some(t!("Waiting for the archive's password").to_string()));
    let ask: Ask = Box::new(move |window, cx| {
        let tx = std::rc::Rc::new(std::cell::Cell::new(Some(tx)));
        let first = if wrong { t!("The password from the NZB does not open this archive.") } else { t!("This archive is password protected.") };
        let (ok_tx, no_tx) = (tx.clone(), tx);
        prompt::show_secret(
            t!("Archive password"),
            Some(format!("{first}\n{archive}")),
            t!("Password").to_string(),
            vec![
                Button::new(t!("Unpack"), Kind::Primary, move |pw, _, _| {
                    if let Some(t) = ok_tx.take() {
                        let _ = t.send(pw.filter(|p| !p.is_empty()));
                    }
                    Ok(())
                }),
                Button::new(t!("Cancel"), Kind::Normal, move |_, _, _| {
                    if let Some(t) = no_tx.take() {
                        let _ = t.send(None);
                    }
                    Ok(())
                }),
            ],
            window,
            cx,
        );
    });
    ASKS.0.unbounded_send(ask).ok()?;
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(answer) => return answer,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !tr.cancel.load(Ordering::Relaxed) => {}
            Err(_) => return None,
        }
    }
}

/// Starts the dialogs' loop and the watch folder (once, with the window), and restores the saved queue.
pub fn init(cx: &mut App) {
    use futures::StreamExt as _;
    restore(cx);
    if let Some(mut rx) = ASKS.1.lock().unwrap_or_else(|e| e.into_inner()).take() {
        cx.spawn(async move |cx| {
            while let Some(q) = rx.next().await {
                let mut pending = Some(q);
                loop {
                    let _ = cx.update(|cx| {
                        let mains = crate::workspace::main_windows(cx);
                        let Some(w) = cx.active_window().filter(|a| mains.contains(a)).or_else(|| mains.first().copied()) else {
                            // no window to ask in: dropped, which the job reads as "no answer"
                            pending.take();
                            return;
                        };
                        if w.update(cx, |_, window, cx| prompt::current(window, cx).is_some()).unwrap_or(false) {
                            return;
                        }
                        if let Some(q) = pending.take() {
                            let _ = w.update(cx, move |_, window, cx| {
                                window.activate_window();
                                q(window, cx);
                            });
                        }
                    });
                    if pending.is_none() {
                        break;
                    }
                    cx.background_executor().timer(Duration::from_millis(300)).await;
                }
            }
        })
        .detach();
    }
    // the watch folder: NZB files put there are taken over (and renamed `.nzb.taken`, so they are taken once)
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(10)).await;
            let folder = cx.update(|cx| cx.global::<Settings>().usenet_watch_folder.trim().to_string());
            if folder.is_empty() {
                continue;
            }
            let found = cx.background_spawn(async move { take_from_watch_folder(Path::new(&folder)) }).await;
            if !found.is_empty() {
                let _ = cx.update(|cx| {
                    for (p, name) in found {
                        let dest = crate::downloads::default_folder(cx.global::<Settings>());
                        start_named(p, name, dest, true, cx);
                    }
                });
            }
        }
    })
    .detach();
}

/// The NZB files in the watch folder, each renamed so it is not taken again (`name.nzb` → `name.nzb.taken`); the renamed
/// paths with the names they had (a `{{password}}` in it is read from there; no copy is left anywhere else).
fn take_from_watch_folder(dir: &Path) -> Vec<(PathBuf, String)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if !is_nzb(&p) {
            continue;
        }
        // (a file still being written by the browser is left for the next look)
        if e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age < Duration::from_secs(3)) {
            continue;
        }
        let taken = PathBuf::from(format!("{}.taken", p.display()));
        if !taken.exists() && std::fs::rename(&p, &taken).is_ok() {
            out.push((taken, p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------------------------
// opening NZB files from the file list, a drop, the command line

/// "Download with leaf" on the selected files: the NZB files among them become Usenet jobs.
pub fn download_selected(paths: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
    let nzbs: Vec<PathBuf> = paths.into_iter().filter(|p| is_nzb(p)).collect();
    if nzbs.is_empty() {
        prompt::notice(t!("Download with leaf"), t!("Select one or more NZB files first.").to_string(), window, cx);
        return;
    }
    for p in nzbs {
        open_nzb(p, cx);
    }
}

/// NZB files dropped on a folder: asked whether to download them (into that folder) or copy them as files.
pub fn ask_dropped(nzbs: Vec<PathBuf>, dest: PathBuf, copy: Box<dyn FnOnce(&mut App)>, window: &mut Window, cx: &mut App) {
    let copy = std::rc::Rc::new(std::cell::Cell::new(Some(copy)));
    let n = nzbs.len();
    let msg = if n == 1 { t!("Download what this NZB file lists into this folder, or copy the NZB file itself?").to_string() } else { t!("Download what these {n} NZB files list into this folder, or copy the NZB files themselves?").replace("{n}", &n.to_string()) };
    prompt::show(
        t!("NZB file"),
        Some(msg),
        None,
        vec![
            Button::new(t!("Download"), Kind::Primary, move |_, _, cx| {
                for p in &nzbs {
                    start(p.clone(), dest.clone(), false, cx);
                }
                Ok(())
            }),
            Button::new(t!("Copy the file"), Kind::Normal, move |_, _, cx| {
                if let Some(c) = copy.take() {
                    c(cx);
                }
                Ok(())
            }),
            Button::cancel(),
        ],
        window,
        cx,
    );
}

// ---------------------------------------------------------------------------------------------------------------------------------
// Settings > Downloads: the "Usenet servers" card

static TESTS: LazyLock<Mutex<std::collections::HashMap<String, String>>> = LazyLock::new(Default::default);

fn test_key(s: &UsenetServer) -> String {
    format!("{}:{}:{}", s.host, s.port, s.user)
}

/// The Test button: connects (by the same route as the downloads: the VPN rule holds) and signs in.
fn test_server(server: UsenetServer, cx: &mut App) {
    let s = cx.global::<Settings>().clone();
    let key = test_key(&server);
    TESTS.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), t!("Testing…").to_string());
    cx.refresh_windows();
    let task = cx.background_spawn(async move {
        let engine = engine_server(&server);
        match leaf_nzb::nntp::test(&engine, &dialer(&s)) {
            Ok(()) => t!("Connected and signed in.").to_string(),
            Err(leaf_nzb::nntp::Error::Wait(_)) => t!("Not tested: Usenet waits for the VPN, which is not connected.").to_string(),
            Err(leaf_nzb::nntp::Error::Auth(_)) => t!("The server refused the user name or password.").to_string(),
            Err(leaf_nzb::nntp::Error::Busy(_)) => t!("The server is busy or has no connection free for this account.").to_string(),
            Err(leaf_nzb::nntp::Error::Tls(d)) => format!("{} ({d})", t!("The server's secure connection could not be trusted")),
            Err(e) => format!("{} ({e})", t!("The server did not answer")),
        }
    });
    cx.spawn(async move |cx| {
        let text = task.await;
        TESTS.lock().unwrap_or_else(|e| e.into_inner()).insert(key, text);
        let _ = cx.update(|cx| cx.refresh_windows());
    })
    .detach();
}

/// The edit dialog: the line, the "without encryption" tick (with its warning), then the password.
fn edit_server(index: Option<usize>, window: &mut Window, cx: &mut App) {
    let current = index.and_then(|i| cx.global::<Settings>().usenet_servers.get(i).cloned());
    let initial = current.as_ref().map(UsenetServer::to_line).unwrap_or_default();
    let plain = std::rc::Rc::new(std::cell::Cell::new(current.as_ref().is_some_and(|c| !c.tls)));
    let plain_on = plain.clone();
    let mut buttons = vec![Button::new(t!("Save"), Kind::Primary, move |value, _, cx| {
        let Some(mut srv) = UsenetServer::from_line(&value.unwrap_or_default(), !plain_on.get()) else {
            return Err(t!("Write the server and port, the user name, the connections (1 to 50), primary, backup or fill, and the retention in days, separated by semicolons").to_string());
        };
        if let Some(c) = &current {
            srv.enabled = c.enabled;
        }
        let keep = srv.clone();
        settings::update(cx, move |s| match index.filter(|i| *i < s.usenet_servers.len()) {
            Some(i) => s.usenet_servers[i] = keep,
            None => s.usenet_servers.push(keep),
        });
        if !srv.user.is_empty() {
            cx.defer({
                let w = cx.active_window();
                move |cx| {
                    let Some(w) = w else { return };
                    let _ = w.update(cx, move |_, window, cx| ask_server_password(srv, window, cx));
                }
            });
        }
        Ok(())
    })];
    if let Some(i) = index {
        buttons.push(Button::new(t!("Remove"), Kind::Danger, move |_, _, cx| {
            settings::update(cx, move |s| {
                if i < s.usenet_servers.len() {
                    let gone = s.usenet_servers.remove(i);
                    let _ = leaf_remote::vault().forget(&secret_id(&gone));
                }
            });
            Ok(())
        }));
    }
    buttons.push(Button::cancel());
    let checks = vec![prompt::Check {
        label: t!("Without encryption (plain NNTP)").into(),
        note: t!("Not recommended").into(),
        on: plain,
        enabled: true,
    }];
    prompt::show_input_checks(
        t!("Usenet server"),
        Some(t!("Server and port; user name; connections (default 8, up to 50); primary, backup or fill; retention in days (0: unknown). The password is asked next and kept in the system's keyring.\nWithout encryption, the user name, the password and everything downloaded can be read on the way: tick it only for a server that has no TLS.").to_string()),
        (initial, "news.example.com:563; user; 8; primary; 0".to_string()),
        checks,
        buttons,
        window,
        cx,
    );
}

fn ask_server_password(srv: UsenetServer, window: &mut Window, cx: &mut App) {
    let id = secret_id(&srv);
    let has = leaf_remote::vault().get(&id).is_some();
    let line = if has { t!("Leave it empty to keep the saved password.") } else { t!("It is kept in the system's keyring, never in leaf's settings.") };
    prompt::show_secret(
        format!("{} {}", t!("Password for"), srv.host),
        Some(format!("{}\n{}", srv.user, line)),
        t!("Password").to_string(),
        vec![
            Button::new(t!("Save"), Kind::Primary, move |pw, _, _| {
                let pw = pw.unwrap_or_default();
                if pw.is_empty() {
                    return if has { Ok(()) } else { Err(t!("Enter the password").to_string()) };
                }
                leaf_remote::vault().keep(&id, &leaf_remote::Secret::new(pw)).map_err(|e| format!("{}\n{}", t!("The keyring did not take the password"), crate::i18n::td(&e)))
            }),
            Button::cancel(),
        ],
        window,
        cx,
    );
}

/// Turning "Usenet only while the VPN is on" off asks once.
fn toggle_vpn_only(on: bool, window: &mut Window, cx: &mut App) {
    if !on {
        settings::update(cx, |s| s.usenet_vpn_only = true);
        return;
    }
    let answer = prompt::ask(t!("Usenet without the VPN"), t!("Usenet traffic will then use your normal connection (or the VPN only when \"Downloads through VPN\" says so). Turn it off?").to_string(), window, cx);
    cx.spawn(async move |cx| {
        if answer.await.unwrap_or(false) {
            let _ = cx.update(|cx| settings::update(cx, |s| s.usenet_vpn_only = false));
        }
    })
    .detach();
}

pub fn servers_card(p: &Palette, s: &Settings) -> AnyElement {
    let mut shell = shell(p).child(card_body(
        p,
        L::Server,
        t!("Usenet servers").into(),
        Some(t!("For NZB files: open one (double click, a drop or the browser) and leaf downloads what it lists, checks and repairs it with par2, unpacks it and puts it in the download folder. Every article goes to the primary servers first, then to the backup and fill servers.").into()),
        Some(button("usenet-add", p, t!("Add"), |window, cx| edit_server(None, window, cx))),
    ));
    let tests = TESTS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (i, srv) in s.usenet_servers.iter().enumerate() {
        let mut parts = vec![format!("{} {}", srv.connections, t!("connections")), priority_text(srv.priority)];
        parts.push(if srv.tls { "TLS".to_string() } else { t!("not encrypted").to_string() });
        if srv.retention_days > 0 {
            parts.push(t!("{n} days retention").replace("{n}", &srv.retention_days.to_string()));
        }
        if !srv.user.is_empty() {
            parts.push(srv.user.clone());
        }
        let mut desc = parts.join(" · ");
        if let Some(t) = tests.get(&test_key(srv)) {
            desc.push_str(&format!("\n{t}"));
        }
        let test = srv.clone();
        shell = shell.child(card_body(
            p,
            if srv.tls { L::Lock } else { L::TriangleAlert },
            format!("{}:{}", srv.host, srv.port).into(),
            Some(desc.into()),
            Some(
                h_flex()
                    .gap_2()
                    .child(button(("usenet-test", i), p, t!("Test"), move |_, cx| test_server(test.clone(), cx)))
                    .child(button(("usenet-edit", i), p, t!("Edit"), move |window, cx| edit_server(Some(i), window, cx)))
                    .into_any_element(),
            ),
        ));
    }
    let vpn_desc = if !s.vpn_signed_in && s.usenet_vpn_only {
        t!("You are not signed in to leaf's VPN, so Usenet downloads wait. Sign in on the VPN page.").to_string()
    } else {
        t!("Every Usenet connection (downloads, the Test button, retries, backup servers) goes through leaf's VPN and nowhere else. While the VPN is off or connecting, Usenet jobs wait and go on by themselves when it is back.").to_string()
    };
    let on = s.usenet_vpn_only;
    let vpn_control = if !s.vpn_signed_in && on {
        h_flex()
            .gap_2()
            .child(button("usenet-vpn-page", p, t!("VPN settings"), |_, cx| crate::settings_view::open_page(crate::settings_view::Page::Vpn, cx)))
            .child(switch("usenet-vpn-only", p, on, move |window, cx| toggle_vpn_only(on, window, cx)))
            .into_any_element()
    } else {
        switch("usenet-vpn-only", p, on, move |window, cx| toggle_vpn_only(on, window, cx))
    };
    let watch = s.usenet_watch_folder.trim().to_string();
    let all = leaf_nzb::queue::all_paused();
    shell
        .child(card_body(p, L::ShieldCheck, t!("Usenet only while the VPN is on").into(), Some(vpn_desc.into()), Some(vpn_control)))
        .child(card_body(p, L::FileArchive, t!("Unpack archives").into(), Some(t!("RAR, 7z and zip, with the password from the NZB or asked once.").into()), Some(sw("usenet-unpack", p, s.usenet_unpack, |s, v| s.usenet_unpack = v))))
        .child(card_body(p, L::Trash, t!("Clean up after a good job").into(), Some(t!("Removes the par2 files, the archive volumes after unpacking, and samples.").into()), Some(sw("usenet-cleanup", p, s.usenet_cleanup, |s, v| s.usenet_cleanup = v))))
        .child(card_body(
            p,
            L::FolderSearch,
            t!("Watch folder for NZB files").into(),
            Some(if watch.is_empty() { t!("Off").to_string() } else { format!("{watch}\n{}", t!("An NZB file put there is downloaded and renamed to .nzb.taken.")) }.into()),
            Some(
                h_flex()
                    .gap_2()
                    .children((!watch.is_empty()).then(|| button("usenet-watch-off", p, t!("Turn off"), |_, cx| settings::update(cx, |s| s.usenet_watch_folder.clear()))))
                    .child(button("usenet-watch", p, t!("Choose…"), |_, cx| {
                        let (tx, rx) = futures::channel::oneshot::channel::<Option<PathBuf>>();
                        let title = t!("Choose the folder leaf watches for NZB files");
                        leaf_sys::sta::run(move |owner| {
                            let _ = tx.send(leaf_sys::dialogs::pick(owner, title, leaf_sys::dialogs::Pick::Folder));
                        });
                        cx.spawn(async move |cx| {
                            if let Ok(Some(path)) = rx.await {
                                let _ = cx.update(|cx| settings::update(cx, |s| s.usenet_watch_folder = path.to_string_lossy().into_owned()));
                            }
                        })
                        .detach();
                    }))
                    .into_any_element(),
            ),
        ))
        .child(card_body(
            p,
            L::Clock,
            t!("Usenet queue").into(),
            Some(format!("{} {} · {} {}", live_count(), t!("in the queue"), history_len(), t!("in the history")).into()),
            Some(
                h_flex()
                    .gap_2()
                    .child(button("usenet-pause-all", p, if all { t!("Resume all") } else { t!("Pause all") }, move |_, cx| {
                        pause_all(!all);
                        cx.refresh_windows();
                    }))
                    .child(button("usenet-history-clear", p, t!("Clear history"), |_, cx| {
                        clear_history();
                        cx.refresh_windows();
                    }))
                    .into_any_element(),
            ),
        ))
        .into_any_element()
}
