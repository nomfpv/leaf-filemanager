//! Torrent downloads as Status Center jobs next to the web and Usenet downloads: a magnet link ("Add a magnet link…",
//! "Download here…", `leaf download magnet:…`) or a .torrent file (opened, "Download with leaf (torrent)", the browser)
//! becomes a job card with its name, size, speed, time left, its peers and its route. The engine is `leaf_torrent`; this
//! file is leaf's face for it: wording, the setting, the saved list.
//!
//! The owner's rule: with "Torrents only while the VPN is on" (on by default) everything a torrent sends goes through
//! leaf's VPN and nowhere else; while the tunnel is down the jobs wait as "Waiting for the VPN" and go on by themselves.
//! Nothing is uploaded after a torrent is complete.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::h_flex;
use gpui_kit::*;
use leaf_sys::fileops::Tracker;
use leaf_torrent::{Source, Step};
use serde::{Deserialize, Serialize};

use crate::prompt::{self, Button, Kind};
use crate::settings::{self, Settings};
use crate::settings_view::{button, card_body, shell, sw, switch};
use crate::t;
use crate::theme::Palette;

// ---------------------------------------------------------------------------------------------------------------------------------
// what is a torrent

/// Whether `p` is a .torrent file.
pub fn is_torrent(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("torrent")) && p.is_file()
}

/// The magnet link in a piece of text: the first word that is one.
pub fn magnet_in(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']')))
        .find(|w| w.len() > 20 && w[..8].eq_ignore_ascii_case("magnet:?") && w.contains("xt=urn:btih:"))
        .map(str::to_string)
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the route and the limit, for every job

static LIMIT: LazyLock<Arc<AtomicU64>> = LazyLock::new(Default::default);

fn apply(s: &Settings) {
    leaf_torrent::set_route(leaf_torrent::Route { vpn_only: s.torrent_vpn_only, through_vpn: s.vpn_downloads });
    leaf_torrent::set_no_upload(s.torrent_no_upload);
    LIMIT.store(u64::from(s.download_speed_limit) << 20, Ordering::Relaxed);
}

/// The route or the speed limit changed in the settings: running torrents take it at once.
pub fn settings_changed(cx: &App) {
    apply(cx.global::<Settings>());
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the saved list

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SavedJob {
    key: u64,
    /// A magnet link, or
    magnet: Option<String>,
    /// leaf's own copy of the .torrent file.
    file: Option<PathBuf>,
    title: String,
    /// The folder the result goes into.
    dest: PathBuf,
    /// The files chosen (by their place in the torrent), once the question was answered: not asked again after a restart.
    only: Option<Vec<usize>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Store {
    active: Vec<SavedJob>,
}

static STORE: LazyLock<Mutex<Store>> = LazyLock::new(|| Mutex::new(store_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()));
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);
static LIVE: AtomicU64 = AtomicU64::new(0);

fn store_path() -> Option<PathBuf> {
    settings::app_dir().map(|d| d.join("torrents.json"))
}

fn copies_dir() -> Option<PathBuf> {
    settings::app_dir().map(|d| d.join("torrents"))
}

fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

fn persist() {
    let snapshot = store().clone();
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

pub fn live_count() -> usize {
    LIVE.load(Ordering::Relaxed) as usize
}

fn forget_copy(job: &SavedJob) {
    if let Some(f) = job.file.as_ref().filter(|f| f.starts_with(copies_dir().unwrap_or_default())) {
        let _ = std::fs::remove_file(f);
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// starting

fn short(hash: &str) -> String {
    format!("{} {}", t!("Torrent"), &hash[..hash.len().min(8)])
}

/// A magnet link as a torrent job whose result goes into `dest`. `false`: it is not a magnet link leaf can read.
pub fn start_magnet(magnet: String, dest: PathBuf, cx: &mut App) -> bool {
    let Ok((name, hash)) = leaf_torrent::describe(&Source::Magnet(magnet.clone())) else { return false };
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    let job = SavedJob { key, magnet: Some(magnet), file: None, title: name.unwrap_or_else(|| short(&hash)), dest, only: None };
    store().active.push(job.clone());
    persist();
    run_job(job, cx);
    true
}

/// A .torrent file as a torrent job whose result goes into `dest`.
pub fn start_file(path: PathBuf, dest: PathBuf, cx: &mut App) {
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    // (leaf keeps its own copy: the job survives a restart even when the file was a temporary download)
    let copy = copies_dir().map(|d| d.join(format!("{key}.torrent"))).filter(|c| {
        let _ = std::fs::create_dir_all(c.parent().unwrap_or(c));
        std::fs::copy(&path, c).is_ok()
    });
    let title = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| t!("Torrent").to_string());
    let job = SavedJob { key, magnet: None, file: Some(copy.unwrap_or(path)), title, dest, only: None };
    store().active.push(job.clone());
    persist();
    run_job(job, cx);
}

/// Opens a .torrent file as a torrent job into the download folder.
pub fn open_torrent(path: PathBuf, cx: &mut App) {
    let dest = crate::downloads::default_folder(cx.global::<Settings>());
    start_file(path, dest, cx);
}

/// What "Retry" on the Downloads page starts again from: a magnet link or a .torrent file.
pub fn start_again(from: &str, dest: PathBuf, cx: &mut App) -> bool {
    if is_torrent(Path::new(from)) {
        start_file(PathBuf::from(from), dest, cx);
        true
    } else {
        magnet_in(from).is_some_and(|m| start_magnet(m, dest, cx))
    }
}

/// The torrents that were not finished when leaf was closed, started again (what is on disk is checked and kept).
fn restore(cx: &mut App) {
    let saved = store().active.clone();
    for s in &saved {
        NEXT_KEY.fetch_max(s.key + 1, Ordering::Relaxed);
    }
    for s in saved {
        run_job(s, cx);
    }
}

pub fn init(cx: &mut App) {
    use futures::StreamExt as _;
    apply(cx.global::<Settings>());
    restore(cx);
    // the jobs' questions (which files), shown one at a time in a main window
    if let Some(mut rx) = ASKS.1.lock().unwrap_or_else(|e| e.into_inner()).take() {
        cx.spawn(async move |cx| {
            while let Some(q) = rx.next().await {
                let mut pending = Some(q);
                loop {
                    let _ = cx.update(|cx| {
                        let mains = crate::workspace::main_windows(cx);
                        let Some(w) = cx.active_window().filter(|a| mains.contains(a)).or_else(|| mains.first().copied()) else {
                            // no window to ask in: dropped, which the job reads as "cancelled"
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
                    cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
                }
            }
        })
        .detach();
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// questions on the UI thread (which files)

type Ask = Box<dyn FnOnce(&mut Window, &mut App) + Send>;

static ASKS: LazyLock<(futures::channel::mpsc::UnboundedSender<Ask>, Mutex<Option<futures::channel::mpsc::UnboundedReceiver<Ask>>>)> = LazyLock::new(|| {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    (tx, Mutex::new(Some(rx)))
});

/// What the files dialog said.
enum Picked {
    Files(Vec<usize>),
    Cancel,
}

/// Asks which of a torrent's files to download (all ticked at first) and waits for the answer. `None`: cancelled.
fn ask_files(tr: &Tracker, name: &str, files: &[(String, u64)]) -> Option<Vec<usize>> {
    let all: Vec<usize> = (0..files.len()).collect();
    let (tx, rx) = std::sync::mpsc::channel::<Picked>();
    (tr.on_progress)(0, 0, Some(t!("Waiting for you to choose the files").to_string()));
    let (name, files) = (name.to_string(), files.to_vec());
    let ask: Ask = Box::new(move |window, cx| {
        let cells: Vec<std::rc::Rc<std::cell::Cell<bool>>> = files.iter().map(|_| std::rc::Rc::new(std::cell::Cell::new(true))).collect();
        let checks: Vec<prompt::Check> = files.iter().zip(&cells).map(|((path, len), on)| prompt::Check { label: path.clone().into(), note: size_text(*len).into(), on: on.clone(), enabled: true }).collect();
        let tx = std::rc::Rc::new(std::cell::Cell::new(Some(tx)));
        let (ok_tx, no_tx) = (tx.clone(), tx);
        let total: u64 = files.iter().map(|f| f.1).sum();
        prompt::show_with_checks(
            t!("Choose the files to download"),
            Some(format!("{name}\n{} {} · {}", files.len(), t!("files"), size_text(total))),
            None,
            Vec::new(),
            checks,
            vec![
                Button::new(t!("Download"), Kind::Primary, move |_, _, _| {
                    let picked: Vec<usize> = cells.iter().enumerate().filter(|(_, c)| c.get()).map(|(i, _)| i).collect();
                    if picked.is_empty() {
                        return Err(t!("Tick at least one file").to_string());
                    }
                    if let Some(t) = ok_tx.take() {
                        let _ = t.send(Picked::Files(picked));
                    }
                    Ok(())
                }),
                Button::new(t!("Cancel"), Kind::Normal, move |_, _, _| {
                    if let Some(t) = no_tx.take() {
                        let _ = t.send(Picked::Cancel);
                    }
                    Ok(())
                }),
            ],
            window,
            cx,
        );
    });
    if ASKS.0.unbounded_send(ask).is_err() {
        return Some(all);
    }
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(Picked::Files(v)) => return Some(v),
            Ok(Picked::Cancel) => return None,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !tr.cancel.load(Ordering::Relaxed) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return None,
            // the dialog went without an answer (Escape), or there was no window to ask in: nothing is downloaded unasked
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

fn run_job(job: SavedJob, cx: &mut App) {
    apply(cx.global::<Settings>());
    // (a torrent that may go only through the VPN does not sit waiting for someone to press Connect)
    if leaf_torrent::go_now() == leaf_torrent::Go::Wait {
        crate::vpn::connect_for_job(cx);
    }
    LIVE.fetch_add(1, Ordering::Relaxed);
    let key = job.key;
    let title = format!("{} {}", t!("Torrent:"), job.title);
    let done_path: Arc<Mutex<Option<PathBuf>>> = Arc::default();
    let failed = Arc::new(AtomicBool::new(false));
    let id_cell = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let (work_job, got, fail_flag) = (job.clone(), done_path.clone(), failed.clone());
    let on_done: crate::status_center::OnDone = {
        let id_cell = id_cell.clone();
        let job = job.clone();
        Box::new(move |ok, cx| {
            let id = id_cell.get();
            LIVE.fetch_sub(1, Ordering::Relaxed);
            let path = done_path.lock().unwrap_or_else(|e| e.into_inner()).clone();
            store().active.retain(|a| a.key != key);
            persist();
            if ok {
                forget_copy(&job);
                let name = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| job.title.clone());
                crate::status_center::set_result(cx, id, Some(format!("{} {name}", t!("Downloaded"))), None);
                if let Some(p) = &path {
                    crate::status_center::set_extras(cx, id, None, Some(p.clone()), None);
                    crate::workspace::select_in_open_folder(p, cx);
                }
            } else if failed.load(Ordering::Relaxed) {
                let retry: crate::status_center::ResultAction = (
                    t!("Retry").to_string(),
                    std::rc::Rc::new(move |cx: &mut App| {
                        crate::status_center::remove(cx, id);
                        store().active.push(job.clone());
                        persist();
                        run_job(job.clone(), cx);
                    }),
                );
                crate::status_center::set_result(cx, id, None, Some(retry));
            } else {
                // cancelled: what was downloaded stays in the folder; leaf's copy of the .torrent file goes
                forget_copy(&job);
            }
        })
    };
    let saved = got.clone();
    let id = crate::status_center::start_with_id(cx, title, move |tr| work(&tr, &work_job, &got, &fail_flag), Some(on_done));
    id_cell.set(id);
    crate::status_center::set_pausable(cx, id);
    // (the Downloads page lists it; the history keeps the hash or the file, never a tracker address: one can hold a passkey)
    let from = match (&job.file, &job.magnet) {
        (Some(f), _) => f.to_string_lossy().into_owned(),
        (None, Some(m)) => leaf_torrent::describe(&Source::Magnet(m.clone())).map(|(_, hash)| format!("magnet:?xt=urn:btih:{hash}")).unwrap_or_default(),
        (None, None) => String::new(),
    };
    crate::status_center::set_transfer(
        cx,
        id,
        crate::xfer_history::Note {
            kind: crate::xfer_history::Kind::Download,
            what: job.title.clone(),
            from,
            to: job.dest.to_string_lossy().into_owned(),
            via: Some(crate::xfer_history::Via::Torrent),
            saved: Some(Arc::new(move || saved.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect())),
        },
    );
}

// ---------------------------------------------------------------------------------------------------------------------------------
// the job's thread

fn size_text(b: u64) -> String {
    let (v, u) = if b >= 1 << 30 { (b as f64 / (1u64 << 30) as f64, "GB") } else { (b as f64 / (1u64 << 20) as f64, "MB") };
    format!("{v:.1} {u}")
}

struct Link<'a> {
    tr: &'a Tracker,
    label: Mutex<String>,
    key: u64,
}

impl leaf_torrent::Host for Link<'_> {
    fn progress(&self, p: &leaf_torrent::Progress) {
        self.tr.bytes.set(p.done_bytes, p.total_bytes);
        let permille = if p.total_bytes > 0 { (p.done_bytes as f64 / p.total_bytes as f64 * 1000.0) as u32 } else { 0 };
        let label = match p.step {
            Step::WaitingForVpn => t!("Waiting for the VPN").to_string(),
            Step::Paused => t!("Paused").to_string(),
            Step::Metadata => t!("Asking peers for the list of files").to_string(),
            Step::Checking => t!("Checking what is already there").to_string(),
            Step::Downloading => {
                let mut parts = Vec::new();
                if p.total_bytes > 0 {
                    parts.push(size_text(p.total_bytes));
                }
                parts.push(if p.peers == 1 { t!("1 peer").to_string() } else { format!("{} {}", p.peers, t!("peers")) });
                parts.push(if p.tunnel { t!("through the VPN").to_string() } else { t!("without the VPN").to_string() });
                parts.join(" · ")
            }
        };
        let mut last = self.label.lock().unwrap_or_else(|e| e.into_inner());
        let name = (*last != label).then(|| label.clone());
        *last = label;
        drop(last);
        (self.tr.on_progress)(1000, permille, name);
    }

    fn choose(&self, name: &str, files: &[(String, u64)]) -> Option<Vec<usize>> {
        let picked = ask_files(self.tr, name, files)?;
        // (kept with the job: a restart does not ask again)
        if let Some(a) = store().active.iter_mut().find(|a| a.key == self.key) {
            a.only = Some(picked.clone());
        }
        persist();
        Some(picked)
    }
}

fn work(tr: &Tracker, job: &SavedJob, got: &Arc<Mutex<Option<PathBuf>>>, failed: &AtomicBool) -> anyhow::Result<bool> {
    let fail = |msg: String| -> anyhow::Result<bool> {
        failed.store(true, Ordering::Relaxed);
        Err(anyhow::anyhow!(msg))
    };
    let unreadable = || t!("This is not a magnet link or .torrent file leaf can read").to_string();
    let source = match (&job.file, &job.magnet) {
        (Some(f), _) => match std::fs::read(f) {
            Ok(b) => Source::File(b),
            Err(e) => return fail(format!("{}\n{e}", t!("The .torrent file could not be read"))),
        },
        (None, Some(m)) => Source::Magnet(m.clone()),
        (None, None) => return fail(unreadable()),
    };
    if let Err(e) = std::fs::create_dir_all(&job.dest) {
        return fail(format!("{} {}\n{e}", t!("The folder could not be made:"), job.dest.display()));
    }
    let ctl = leaf_torrent::Control { cancel: tr.cancel.clone(), paused: tr.paused.clone(), limit: LIMIT.clone() };
    let link = Link { tr, label: Mutex::new(String::new()), key: job.key };
    match leaf_torrent::run(&leaf_torrent::Spec { source, dest: job.dest.clone(), only: job.only.clone() }, &ctl, &link) {
        Ok(done) => {
            *got.lock().unwrap_or_else(|e| e.into_inner()) = Some(done.path);
            Ok(true)
        }
        Err(leaf_torrent::Error::Cancelled) => Ok(false),
        Err(e) => {
            log::warn!("torrent: job {} failed: {e}", job.key);
            fail(match e {
                leaf_torrent::Error::NotTorrent(d) => format!("{}\n{d}", unreadable()),
                leaf_torrent::Error::Duplicate => t!("This torrent is already downloading").to_string(),
                leaf_torrent::Error::Engine(d) => format!("{} {}\n{d}", t!("The torrent could not be downloaded. What was downloaded is kept in"), job.dest.display()),
                leaf_torrent::Error::Cancelled => t!("Cancelled").to_string(),
            })
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------------------
// commands and dialogs

/// "Download with leaf (torrent)" on the selected files: the .torrent files among them become torrent jobs.
pub fn download_selected(paths: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
    let torrents: Vec<PathBuf> = paths.into_iter().filter(|p| is_torrent(p)).collect();
    if torrents.is_empty() {
        prompt::notice(t!("Download with leaf (torrent)"), t!("Select one or more .torrent files first.").to_string(), window, cx);
        return;
    }
    for p in torrents {
        open_torrent(p, cx);
    }
}

/// What the dialogs say about the route right now.
fn route_line(s: &Settings) -> String {
    if s.torrent_vpn_only {
        if leaf_vpn::is_on() {
            t!("It goes through leaf's VPN and nowhere else.").to_string()
        } else if s.vpn_signed_in {
            t!("leaf's VPN is connected for it: the torrent waits until the VPN is up, and sends nothing before.").to_string()
        } else {
            t!("leaf's VPN is not connected: the torrent waits until it is, and sends nothing before.").to_string()
        }
    } else {
        t!("\"Torrents only while the VPN is on\" is turned off: peers and trackers can see this PC's own address.").to_string()
    }
}

/// "Add a magnet link…": asks for the link (one on the clipboard is already in the box) and downloads it into `dest`.
pub fn ask_magnet(dest: PathBuf, window: &mut Window, cx: &mut App) {
    let initial = cx.read_from_clipboard().and_then(|c| c.text()).and_then(|t| magnet_in(&t)).unwrap_or_default();
    let shown = dest.clone();
    let go = Button::new(t!("Download"), Kind::Primary, move |text, _, cx| {
        let Some(magnet) = magnet_in(text.as_deref().unwrap_or_default()) else {
            return Err(t!("Enter a magnet link (it starts with magnet:?)").to_string());
        };
        if start_magnet(magnet, dest.clone(), cx) { Ok(()) } else { Err(t!("This is not a magnet link or .torrent file leaf can read").to_string()) }
    });
    prompt::show(
        t!("Add a magnet link"),
        Some(format!("{} {}\n{}", t!("It is downloaded into"), shown.display(), route_line(cx.global::<Settings>()))),
        Some((initial, "magnet:?xt=urn:btih:…".to_string())),
        vec![go, Button::cancel()],
        window,
        cx,
    );
}

/// Turning "Torrents only while the VPN is on" off asks once.
fn toggle_vpn_only(on: bool, window: &mut Window, cx: &mut App) {
    if !on {
        settings::update(cx, |s| s.torrent_vpn_only = true);
        return;
    }
    let answer = prompt::ask(
        t!("Torrents without the VPN"),
        t!("Every peer and tracker of a torrent will then see this PC's own internet address, and your provider can see that you use BitTorrent (the VPN is used only when \"Downloads through the VPN\" says so). Running torrents switch at once. Turn it off?").to_string(),
        window,
        cx,
    );
    cx.spawn(async move |cx| {
        if answer.await.unwrap_or(false) {
            let _ = cx.update(|cx| settings::update(cx, |s| s.torrent_vpn_only = false));
        }
    })
    .detach();
}

/// Settings > Downloads: the "Torrents" cards.
pub fn settings_card(p: &Palette, s: &Settings) -> AnyElement {
    let on = s.torrent_vpn_only;
    let vpn_desc = if !s.vpn_signed_in && on {
        t!("You are not signed in to leaf's VPN, so torrents wait. Sign in on the VPN page.").to_string()
    } else if on {
        t!("Everything a torrent sends (peers, trackers, name lookups) goes through leaf's VPN and nowhere else. While the VPN is off or connecting, torrents wait and go on by themselves when it is back. The DHT is only asked, through the VPN; leaf is never listed there. What cannot go through the VPN is off: incoming connections, port forwarding, local discovery and IPv6.").to_string()
    } else {
        t!("Off: torrents use this PC's own connection, and peers and trackers see its address.").to_string()
    };
    let vpn_control = if !s.vpn_signed_in && on {
        h_flex()
            .gap_2()
            .child(button("torrent-vpn-page", p, t!("VPN settings"), |_, cx| crate::settings_view::open_page(crate::settings_view::Page::Vpn, cx)))
            .child(switch("torrent-vpn-only", p, on, move |window, cx| toggle_vpn_only(on, window, cx)))
            .into_any_element()
    } else {
        switch("torrent-vpn-only", p, on, move |window, cx| toggle_vpn_only(on, window, cx))
    };
    shell(p)
        .child(card_body(
            p,
            L::Download,
            t!("Torrents").into(),
            Some(t!("Magnet links and .torrent files: open a .torrent file, or add a magnet link, choose its files, and leaf downloads them into the download folder. Nothing runs while no torrent does.").into()),
            Some(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(crate::guide_page::help_button(p, "torrent-help", "torrent"))
                    .child(button("torrent-add", p, t!("Add a magnet link…"), |window, cx| {
                        let dest = crate::downloads::default_folder(cx.global::<Settings>());
                        ask_magnet(dest, window, cx);
                    }))
                    .into_any_element(),
            ),
        ))
        .child(card_body(p, L::ShieldCheck, t!("Torrents only while the VPN is on").into(), Some(vpn_desc.into()), Some(vpn_control)))
        .child(card_body(
            p,
            L::Upload,
            t!("Never upload").into(),
            Some(t!("Nothing of a torrent is sent to other peers, also while it downloads: leaf tells no peer which pieces it has and refuses every request for one. Some peers give less to a client that gives nothing, so a download can be slower. Off: leaf uploads while a torrent downloads; once it is complete nothing is uploaded either way.").into()),
            Some(sw("torrent-no-upload", p, s.torrent_no_upload, |s, v| s.torrent_no_upload = v)),
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    // (not `super::*`: the toolkit's own `test` attribute would come with it)
    use super::magnet_in;

    #[test]
    fn a_magnet_link_is_found_in_text() {
        let m = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=x";
        assert_eq!(magnet_in(&format!("look: <{m}> ok")).as_deref(), Some(m));
        assert_eq!(magnet_in("https://example.com/a.torrent"), None);
        assert_eq!(magnet_in("magnet:?dn=only-a-name-and-nothing-else"), None);
    }
}
