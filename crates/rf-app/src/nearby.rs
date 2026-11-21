//! Nearby, the Windows side: sending files to leaf on the network's other devices, and taking theirs. The network part is
//! `crates/rf-nearby` (shared with the macOS app); this is the service in the main process, the Status Center cards for the transfers,
//! the small window that asks about an offer or a pairing code, the "Send with Nearby" menu and the Settings page.
//!
//! Off until the user turns it on (Settings > Nearby): it listens on a network port, and Windows' firewall may ask about that. It runs
//! while leaf runs (a window open, or in the notification area).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::assets::IconName as L;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rf_nearby::{Config, EstimateSource, LS_ID_BASE, LS_PORT, LinkKind, LocalSend, LsConfig, LsDevice, LsListener, LsRequest, LsResult, NearbyListener, NetStatus, Nearby, Offer, Peer, SpeedResult, TransferResult, Visibility};

use crate::fluent_menu::{Item, MenuIcon};
use crate::settings::{self, NearbyVisibility, Settings};
use crate::t;
use crate::theme::{self, Palette};

/// "Everyone" lasts this long, then the crate falls back to trusted devices.
const EVERYONE_MINUTES: u32 = 10;
/// An unanswered question goes away a little after the other side stops waiting (the crate waits three minutes).
const QUESTION_LIFE: Duration = Duration::from_secs(185);
const WINDOW_W: f32 = 380.;
const WINDOW_H: f32 = 176.;

/// What the crate says, carried to the UI thread.
enum Ev {
    Peers(Vec<Peer>),
    Offer(Offer),
    Receiving(Offer),
    Progress(u64, u64, u64),
    Finished(u64, TransferResult),
    Code(String, String),
    Paired(String),
    Visibility(Visibility),
    Status(NetStatus),
    /// A click on one of our toasts: its argument (`nearby:yes:<key>`, `nearby:no:<key>`, `nearby:show:<key>`, `nearby:open:<path>`).
    Toast(String),
    Speed(String, SpeedResult),
    /// LocalSend: the devices found changed; a device asks to send files.
    LsDevices(Vec<LsDevice>),
    LsRequest(LsRequest),
    /// A LocalSend request closed without our answer (the two minutes ran out, or the sender left): its question goes.
    LsRequestEnded(u64),
    /// A device we send to wants a PIN: the sender of the reply waits for what the user types (`None`: gives up).
    LsPin(String, bool, std::sync::mpsc::Sender<Option<String>>),
}

/// What the LocalSend engine says, carried to the UI thread in the same events as the other engine's (its transfer ids are far from
/// the other's, so one list of transfers serves both).
struct LsL(mpsc::UnboundedSender<Ev>);

impl LsListener for LsL {
    fn devices_changed(&self, devices: Vec<LsDevice>) {
        let _ = self.0.unbounded_send(Ev::LsDevices(devices));
    }
    fn request(&self, request: LsRequest) {
        let _ = self.0.unbounded_send(Ev::LsRequest(request));
    }
    fn request_ended(&self, id: u64) {
        let _ = self.0.unbounded_send(Ev::LsRequestEnded(id));
    }
    fn pin(&self, alias: &str, wrong: bool) -> Option<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.0.unbounded_send(Ev::LsPin(alias.to_string(), wrong, tx)).ok()?;
        // (a dialog closed without an answer drops its sender: that ends the wait too)
        rx.recv_timeout(Duration::from_secs(150)).ok().flatten()
    }
    fn progress(&self, transfer: u64, done: u64, total: u64) {
        let _ = self.0.unbounded_send(Ev::Progress(transfer, done, total));
    }
    fn finished(&self, transfer: u64, r: LsResult) {
        let _ = self.0.unbounded_send(Ev::Finished(transfer, TransferResult { ok: r.ok, cancelled: r.cancelled, error: r.error, paths: r.paths }));
    }
}

struct Listener(mpsc::UnboundedSender<Ev>);

impl NearbyListener for Listener {
    fn peers_changed(&self, peers: Vec<Peer>) {
        let _ = self.0.unbounded_send(Ev::Peers(peers));
    }
    fn offer(&self, offer: Offer) {
        let _ = self.0.unbounded_send(Ev::Offer(offer));
    }
    fn receiving(&self, offer: Offer) {
        let _ = self.0.unbounded_send(Ev::Receiving(offer));
    }
    fn progress(&self, transfer: u64, done: u64, total: u64) {
        let _ = self.0.unbounded_send(Ev::Progress(transfer, done, total));
    }
    fn finished(&self, transfer: u64, result: TransferResult) {
        let _ = self.0.unbounded_send(Ev::Finished(transfer, result));
    }
    fn pairing_code(&self, peer: String, code: String) {
        let _ = self.0.unbounded_send(Ev::Code(peer, code));
    }
    fn paired(&self, peer: String, _ok: bool) {
        let _ = self.0.unbounded_send(Ev::Paired(peer));
    }
    fn speed_measured(&self, peer: String, result: SpeedResult) {
        let _ = self.0.unbounded_send(Ev::Speed(peer, result));
    }
    fn visibility_changed(&self, visibility: Visibility) {
        let _ = self.0.unbounded_send(Ev::Visibility(visibility));
    }
    fn status(&self, status: NetStatus) {
        let _ = self.0.unbounded_send(Ev::Status(status));
    }
}

/// A transfer's news for its Status Center card's worker thread.
enum Step {
    Progress(u64, u64),
    End(TransferResult),
}

/// Something the user is asked about.
#[derive(Clone)]
enum Question {
    Offer(Offer),
    /// A LocalSend device asks to send files.
    Ls(LsRequest),
    Pair { peer: String, name: String, code: String },
}

impl Question {
    fn key(&self) -> String {
        match self {
            Question::Offer(o) => format!("offer-{}", o.id),
            Question::Ls(r) => format!("ls-{}", r.id),
            Question::Pair { peer, .. } => format!("pair-{peer}"),
        }
    }
}

/// What it was started with: a change of any of these starts it anew (the crate takes them once).
#[derive(Clone, PartialEq)]
struct Started {
    name: String,
    inbox: String,
    auto_accept_trusted: bool,
}

#[derive(Default)]
pub struct NearbyService {
    nearby: Option<Arc<Nearby>>,
    /// The LocalSend engine: made at start, silent (nothing bound, nothing sent) until a search or receiving.
    ls: Option<Arc<LocalSend>>,
    ls_devices: Vec<LsDevice>,
    /// Why LocalSend could not receive (the port taken, ...), for the Settings page.
    ls_problem: Option<String>,
    /// The device a "Send to device" command goes to again (its fingerprint).
    ls_last: Option<String>,
    started: Option<Started>,
    peers: Vec<Peer>,
    /// Why discovery or the whole service could not start, for the Settings page.
    problem: Option<String>,
    transfers: HashMap<u64, std::sync::mpsc::Sender<Step>>,
    /// The transfers coming in, with the name of the device they come from (for the toast when they arrive).
    incoming: HashMap<u64, String>,
    questions: Vec<Question>,
    /// The questions asked in a toast (by key); the others are asked in the window.
    in_toast: std::collections::HashSet<String>,
    window: Option<WindowHandle<QuestionView>>,
    events: Option<mpsc::UnboundedSender<Ev>>,
    /// Settings > Nearby is on screen: see [`page_seen`].
    viewing: Option<Viewing>,
    /// The speed tests running now (by device), and the last result with each device in this run.
    measuring: std::collections::HashSet<String>,
    speeds: HashMap<String, SpeedResult>,
}

/// Settings > Nearby on screen, in `window`, last drawn at `seen`; `restore` is the visibility to go back to when it goes.
struct Viewing {
    window: AnyWindowHandle,
    seen: std::time::Instant,
    restore: Option<NearbyVisibility>,
}

/// The user picked a visibility in the list: a choice that stays when the page goes.
static CHOSEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The page counts as gone when it was not drawn for this long (its window is redrawn every two seconds while it is open).
const PAGE_GONE: Duration = Duration::from_secs(5);

impl Global for NearbyService {}

/// At start, in the main process only: Nearby follows the settings from here on ([`apply`], called by every settings change).
pub fn init(cx: &mut App) {
    let (tx, mut rx) = mpsc::unbounded::<Ev>();
    cx.set_global(NearbyService { events: Some(tx), ..Default::default() });
    cx.spawn(async move |cx| {
        while let Some(ev) = rx.next().await {
            cx.update(|cx| on_event(ev, cx));
        }
    })
    .detach();
    // "everyone" never outlives a run: it is a ten-minute state
    if cx.global::<Settings>().nearby_visibility == NearbyVisibility::Everyone {
        settings::update(cx, |s| s.nearby_visibility = NearbyVisibility::Trusted);
    }
    ls_start(cx);
    apply(cx);
}

fn ls_start(cx: &mut App) {
    let Some(tx) = cx.global::<NearbyService>().events.clone() else { return };
    let Some(dir) = settings::app_dir().map(|d| d.join("nearby")) else { return };
    let model = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    let cfg = LsConfig { alias: name_of(cx.global::<Settings>()), model: model.into(), state_dir: dir.to_string_lossy().into_owned(), port: LS_PORT, multicast: true, local_only: false, pin: None };
    match LocalSend::new(cfg, Arc::new(LsL(tx))) {
        Ok(ls) => cx.global_mut::<NearbyService>().ls = Some(ls),
        Err(e) => log::warn!("localsend: cannot start: {e}"),
    }
}

/// Receiving from LocalSend devices follows its setting (and the name they see follows the device name).
fn ls_apply(cx: &mut App) {
    let Some(ls) = cx.global::<NearbyService>().ls.clone() else { return };
    let s = cx.global::<Settings>();
    ls.set_alias(&name_of(s));
    ls.set_pin(Some(&s.nearby_localsend_pin));
    let want = s.nearby_localsend_receive;
    if want == ls.is_receiving() {
        return;
    }
    let problem = match ls.set_receiving(want) {
        Ok(_) => None,
        Err(e) => {
            log::warn!("localsend: cannot receive: {e}");
            Some(e.to_string())
        }
    };
    cx.global_mut::<NearbyService>().ls_problem = problem;
    cx.refresh_windows();
}

/// Brings the service in line with the settings: starts or stops it, starts it anew when its name or folder changed, sets who sees
/// this PC. Cheap when nothing changed.
pub fn apply(cx: &mut App) {
    if cx.try_global::<NearbyService>().is_none() {
        return;
    }
    let s = cx.global::<Settings>();
    let want = s.nearby_enabled.then(|| Started { name: name_of(s), inbox: inbox_of(s).to_string_lossy().into_owned(), auto_accept_trusted: s.nearby_auto_accept_trusted });
    let vis = match s.nearby_visibility {
        NearbyVisibility::Off => Visibility::Off,
        NearbyVisibility::Trusted => Visibility::Trusted,
        NearbyVisibility::Everyone => Visibility::Everyone,
    };
    if cx.global::<NearbyService>().started != want {
        stop(cx);
        if let Some(w) = want {
            start(w, cx);
        }
        cx.refresh_windows();
    }
    if let Some(n) = cx.global::<NearbyService>().nearby.clone()
        && n.visibility() != vis
    {
        n.set_visibility(vis);
    }
    ls_apply(cx);
}

fn start(w: Started, cx: &mut App) {
    let Some(tx) = cx.global::<NearbyService>().events.clone() else { return };
    let Some(dir) = settings::app_dir().map(|d| d.join("nearby")) else { return };
    let config = Config {
        name: w.name.clone(),
        os: "windows".into(),
        state_dir: dir.to_string_lossy().into_owned(),
        inbox: w.inbox.clone(),
        auto_accept_trusted: w.auto_accept_trusted,
        port: 0,
        discovery: true,
        everyone_minutes: EVERYONE_MINUTES,
        local_only: false,
    };
    let f = cx.global_mut::<NearbyService>();
    f.started = Some(w);
    match Nearby::start(config, Arc::new(Listener(tx))) {
        Ok(n) => {
            f.nearby = Some(n);
            f.problem = None;
            watch_link(cx);
        }
        Err(e) => {
            log::warn!("nearby: cannot start: {e}");
            f.problem = Some(e.to_string());
        }
    }
}

fn stop(cx: &mut App) {
    let f = cx.global_mut::<NearbyService>();
    f.started = None;
    f.peers.clear();
    f.problem = None;
    f.questions.clear();
    for key in f.in_toast.drain() {
        leaf_sys::toast_post::remove(crate::installer::APP_ID, &key);
    }
    f.incoming.clear();
    f.viewing = None;
    // (the cards' workers see their channel go and end as cancelled)
    f.transfers.clear();
    let window = f.window.take();
    if let Some(n) = f.nearby.take() {
        // stopping waits for the runtime: not on the UI thread
        std::thread::spawn(move || n.stop());
    }
    if let Some(w) = window {
        let _ = w.update(cx, |_, window, _| window.remove_window());
    }
}

/// The name other devices see this PC by (Settings > Nearby > Device name, else the computer's name); shared folders use it too.
pub(crate) fn device_name(s: &Settings) -> String {
    name_of(s)
}

fn name_of(s: &Settings) -> String {
    let name = s.nearby_name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into())
}

fn inbox_of(s: &Settings) -> PathBuf {
    let custom = s.nearby_inbox.trim();
    if !custom.is_empty() {
        return PathBuf::from(custom);
    }
    dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads"))).unwrap_or_default()
}

/// Whether Nearby runs (the menu offers it only then).
pub(crate) fn running(cx: &App) -> bool {
    // (the LocalSend side needs no setting to send: the menu is there whenever the service is)
    cx.try_global::<NearbyService>().is_some_and(|f| f.nearby.is_some() || f.ls.is_some())
}

fn on_event(ev: Ev, cx: &mut App) {
    match ev {
        Ev::Peers(peers) => {
            cx.global_mut::<NearbyService>().peers = peers;
            cx.refresh_windows();
        }
        Ev::Offer(offer) => ask(Question::Offer(offer), cx),
        Ev::LsDevices(devices) => {
            cx.global_mut::<NearbyService>().ls_devices = devices;
            cx.refresh_windows();
        }
        Ev::LsRequest(request) => ask(Question::Ls(request), cx),
        Ev::LsRequestEnded(id) => forget_question(&format!("ls-{id}"), cx),
        Ev::LsPin(alias, wrong, reply) => ask_pin(alias, wrong, reply, cx),
        Ev::Receiving(offer) => {
            let dest = inbox_of(cx.global::<Settings>());
            cx.global_mut::<NearbyService>().incoming.insert(offer.transfer, offer.from_name.clone());
            incoming_started(&offer, cx);
            track(cx, offer.transfer, format!("{}: {}", t!("Receiving with Nearby"), offer.from_name), Some(dest));
        }
        Ev::Progress(transfer, done, total) => {
            if let Some(tx) = cx.global::<NearbyService>().transfers.get(&transfer) {
                let _ = tx.send(Step::Progress(done, total));
            }
        }
        Ev::Finished(transfer, result) => {
            let from = cx.global_mut::<NearbyService>().incoming.remove(&transfer);
            if let Some(from) = from
                && result.ok
            {
                arrived(transfer, from, result.paths.clone(), cx);
            }
            if let Some(tx) = cx.global_mut::<NearbyService>().transfers.remove(&transfer) {
                let _ = tx.send(Step::End(result));
            }
        }
        Ev::Toast(arg) => on_toast(&arg, cx),
        Ev::Speed(peer, result) => {
            let f = cx.global_mut::<NearbyService>();
            f.measuring.remove(&peer);
            f.speeds.insert(peer, result);
            cx.refresh_windows();
        }
        Ev::Code(peer, code) => {
            let f = cx.global::<NearbyService>();
            let name = f.peers.iter().find(|p| p.id == peer).map(|p| p.name.clone()).unwrap_or_else(|| peer.clone());
            ask(Question::Pair { peer, name, code }, cx);
        }
        Ev::Paired(peer) => {
            forget_question(&Question::Pair { peer, name: String::new(), code: String::new() }.key(), cx);
            cx.refresh_windows();
        }
        Ev::Visibility(v) => {
            // (the ten-minute fall-back is not undone when the page goes)
            if let Some(view) = cx.global_mut::<NearbyService>().viewing.as_mut() {
                view.restore = None;
            }
            let vis = match v {
                Visibility::Off => NearbyVisibility::Off,
                Visibility::Trusted => NearbyVisibility::Trusted,
                Visibility::Everyone => NearbyVisibility::Everyone,
            };
            settings::update(cx, |s| s.nearby_visibility = vis);
        }
        Ev::Status(status) => {
            cx.global_mut::<NearbyService>().problem = match status {
                NetStatus::Ready => None,
                NetStatus::DiscoveryUnavailable { reason } => Some(reason),
            };
            cx.refresh_windows();
        }
    }
}

/// A Status Center card for a transfer: its progress, its end, and its cancel button stopping the transfer.
fn track(cx: &mut App, transfer: u64, title: String, dest: Option<PathBuf>) {
    let f = cx.global::<NearbyService>();
    let cancel: Box<dyn Fn(u64) + Send> = if transfer >= LS_ID_BASE {
        let Some(ls) = f.ls.clone() else { return };
        Box::new(move |t| ls.cancel(t))
    } else {
        let Some(nearby) = f.nearby.clone() else { return };
        Box::new(move |t| nearby.cancel(t))
    };
    let (tx, rx) = std::sync::mpsc::channel::<Step>();
    cx.global_mut::<NearbyService>().transfers.insert(transfer, tx);
    let id = crate::status_center::start_with_id(
        cx,
        title,
        move |tracker| {
            let mut cancelled = false;
            loop {
                match rx.recv_timeout(Duration::from_millis(150)) {
                    Ok(Step::Progress(done, total)) => {
                        tracker.bytes.set(done, total);
                        let part = if total == 0 { 0 } else { (done.min(total) * 1000 / total) as u32 };
                        (tracker.on_progress)(1000, part, None);
                    }
                    Ok(Step::End(r)) if r.ok => return Ok(true),
                    Ok(Step::End(r)) if r.cancelled || cancelled => return Ok(false),
                    Ok(Step::End(r)) => return Err(anyhow::anyhow!("{}", failure_text(r.error.as_deref()))),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if !cancelled && tracker.cancel.load(Ordering::Relaxed) {
                            cancelled = true;
                            cancel(transfer);
                        }
                    }
                    // (Nearby stopped meanwhile)
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(false),
                }
            }
        },
        None,
    );
    if dest.is_some() {
        crate::status_center::set_extras(cx, id, None, dest, None);
    }
}

/// The crate's reasons are English sentences for logs: the ones a user meets get words of the interface.
fn failure_text(error: Option<&str>) -> String {
    let e = error.unwrap_or_default();
    let has = |s: &str| e.contains(s);
    if has("only takes files from its trusted devices") {
        t!("That device only takes files from its trusted devices. Pair with it first.").to_string()
    } else if has("declined") {
        t!("The other device declined.").to_string()
    } else if has("asks for a PIN") {
        t!("The other device asks for a PIN.").to_string()
    } else if has("PIN was not right") {
        t!("The PIN was not right.").to_string()
    } else if has("busy with another transfer") {
        t!("The other device is busy with another transfer.").to_string()
    } else if has("did not answer") || has("no answer") || has("timed out") {
        t!("The device did not answer.").to_string()
    } else if has("busy with another speed test") {
        t!("The speed to that device was just tested; try again in a few seconds.").to_string()
    } else if has("busy with other offers") {
        t!("You already have an offer waiting on that device.").to_string()
    } else if has("arrived damaged") {
        t!("A file arrived damaged. Send again: the rest picks up where it stopped.").to_string()
    } else if e.is_empty() {
        t!("The transfer stopped partway.").to_string()
    } else {
        e.to_string()
    }
}

// ------------------------------------------------------------------------------------------------------------------ questions

/// Asks: in a toast with the answers as its buttons (the installed app), else in the window.
fn ask(q: Question, cx: &mut App) {
    let key = q.key();
    let toasted = toast_question(&q, cx);
    let f = cx.global_mut::<NearbyService>();
    f.questions.retain(|x| x.key() != key);
    f.questions.push(q);
    if toasted {
        f.in_toast.insert(key.clone());
    }
    sync_window(cx);
    // the other side stops waiting after three minutes: the question goes with it
    cx.spawn(async move |cx| {
        cx.background_executor().timer(QUESTION_LIFE).await;
        cx.update(|cx| forget_question(&key, cx));
    })
    .detach();
}

fn answer(q: &Question, yes: bool, cx: &mut App) {
    forget_question(&q.key(), cx);
    if let Question::Ls(r) = q {
        if yes {
            ls_accept(r, inbox_of(cx.global::<Settings>()), cx);
        } else if let Some(ls) = cx.global::<NearbyService>().ls.clone() {
            ls.decline(r.id);
        }
        return;
    }
    let Some(n) = cx.global::<NearbyService>().nearby.clone() else { return };
    match q {
        Question::Offer(o) if yes => {
            let dest = inbox_of(cx.global::<Settings>());
            n.accept(o.id, dest.to_string_lossy().into_owned());
            cx.global_mut::<NearbyService>().incoming.insert(o.transfer, o.from_name.clone());
            track(cx, o.transfer, format!("{}: {}", t!("Receiving with Nearby"), o.from_name), Some(dest));
        }
        Question::Offer(o) => n.reject(o.id),
        Question::Pair { peer, .. } => n.confirm_pairing(peer.clone(), yes),
        Question::Ls(_) => {}
    }
}

/// A device we send to asks for its PIN: a small dialog for it (three tries are the crate's; each asks again with a note).
fn ask_pin(alias: String, wrong: bool, reply: std::sync::mpsc::Sender<Option<String>>, cx: &mut App) {
    let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) else {
        let _ = reply.send(None);
        return;
    };
    let reply = std::rc::Rc::new(std::cell::RefCell::new(Some(reply)));
    let on_ok = reply.clone();
    let _ = w.update(cx, move |_, window, cx| {
        let ok = crate::prompt::Button::new(t!("Send"), crate::prompt::Kind::Primary, move |text, _, _| {
            let pin = text.unwrap_or_default().trim().to_string();
            if pin.is_empty() {
                return Err(t!("Enter the PIN").into());
            }
            if let Some(tx) = on_ok.borrow_mut().take() {
                let _ = tx.send(Some(pin));
            }
            Ok(())
        });
        let mut message = format!("{} {}", alias, t!("asks for a PIN before it takes files. It shows or has it in its LocalSend settings."));
        if wrong {
            message = format!("{}\n{}", t!("That PIN was not right."), message);
        }
        crate::prompt::show(t!("PIN needed"), Some(message), Some((String::new(), "PIN".to_string())), vec![ok, crate::prompt::Button::cancel()], window, cx);
    });
    // (Cancel and Escape: the dialog goes, the sender with it, and the waiting side reads that as "no PIN")
    drop(reply);
}

/// Takes a LocalSend request into `dest`, when the drive has room for it (else it is declined and a card says why).
fn ls_accept(r: &LsRequest, dest: PathBuf, cx: &mut App) {
    let Some(ls) = cx.global::<NearbyService>().ls.clone() else { return };
    let root = dest.ancestors().last().map(PathBuf::from).unwrap_or_default();
    let free = leaf_sys::volume::volume_info(&root).ok().filter(|v| v.total > 0).map(|v| v.free);
    let title = format!("{}: {}", t!("Receiving with Nearby"), r.from);
    if let Some(free) = free
        && free < r.total_bytes
    {
        ls.decline(r.id);
        let msg = format!("{} ({}, {} {})", t!("Not enough free space for these files"), rf_core::format::size(r.total_bytes), rf_core::format::size(free), t!("free"));
        crate::status_center::start_with_id(cx, title, move |_| Err(anyhow::anyhow!("{msg}")), None);
        return;
    }
    ls.accept(r.id, dest.clone());
    cx.global_mut::<NearbyService>().incoming.insert(r.transfer, r.from.clone());
    track(cx, r.transfer, title, Some(dest));
}

/// "Choose folder…" on a LocalSend request: the files go where the user points (the question stays when the choice is cancelled).
fn choose_folder(r: LsRequest, cx: &mut App) {
    let (tx, rx) = futures::channel::oneshot::channel::<Option<PathBuf>>();
    let title = t!("Choose the folder for received files");
    leaf_sys::sta::run(move |owner| {
        let _ = tx.send(leaf_sys::dialogs::pick(owner, title, leaf_sys::dialogs::Pick::Folder));
    });
    cx.spawn(async move |cx| {
        if let Ok(Some(path)) = rx.await {
            let _ = cx.update(|cx| {
                forget_question(&Question::Ls(r.clone()).key(), cx);
                ls_accept(&r, path, cx);
            });
        }
    })
    .detach();
}

/// A question is no longer asked (answered, given up, or out of time): out of the window, and its toast taken away.
fn forget_question(key: &str, cx: &mut App) {
    if !cx.has_global::<NearbyService>() {
        return;
    }
    let f = cx.global_mut::<NearbyService>();
    f.questions.retain(|x| x.key() != key);
    if f.in_toast.remove(key) {
        leaf_sys::toast_post::remove(crate::installer::APP_ID, key);
    }
    sync_window(cx);
}

/// What a question says: its title, its lines, a pairing's code, and its two answers.
fn texts(q: &Question) -> (String, Vec<String>, Option<String>, &'static str, &'static str) {
    match q {
        Question::Offer(o) => {
            // "photo.jpg", or "photo.jpg and 3 items" (the top-level names: a folder counts once)
            let what = match o.names.len() {
                0 => crate::i18n::items(o.file_count as usize),
                1 => o.names[0].clone(),
                n => format!("{} {} {}", o.names[0], t!("and"), crate::i18n::items(n - 1)),
            };
            let mut lines = vec![format!("{} · {}", what, rf_core::format::size(o.total_bytes))];
            // (a device that was never paired can call itself anything)
            if !o.trusted {
                lines.push(t!("Unpaired device: its name can't be verified").to_string());
            }
            (format!("{} {}", o.from_name, t!("wants to send you files")), lines, None, t!("Deny"), t!("Accept"))
        }
        Question::Ls(r) => {
            let what = match r.files.len() {
                0 => crate::i18n::items(0),
                1 => r.files[0].name.clone(),
                n => format!("{} {} {}", r.files[0].name, t!("and"), crate::i18n::items(n - 1)),
            };
            let lines = vec![format!("{} · {}", what, rf_core::format::size(r.total_bytes)), t!("Unpaired device: its name can't be verified").to_string()];
            (format!("{} {}", r.from, t!("wants to send you files")), lines, None, t!("Deny"), t!("Accept"))
        }
        Question::Pair { name, code, .. } => (
            format!("{}: {name}", t!("Pair")),
            vec![t!("Make sure both devices show the same code. Paired devices can send each other files without asking.").to_string()],
            Some(format!("{} {}", &code[..3.min(code.len())], &code[3.min(code.len())..])),
            t!("Cancel"),
            t!("Pair"),
        ),
    }
}

/// Windows shows our toasts only for the installed app: its Start menu shortcut carries the AppUserModelID. A build from the source
/// tree or the portable zip has none, and a toast would vanish without a word: those ask in the window.
fn toasts_usable() -> bool {
    dirs::data_dir().is_some_and(|d| d.join(r"Microsoft\Windows\Start Menu\Programs\leaf.lnk").exists())
}

fn toast_question(q: &Question, cx: &App) -> bool {
    if !toasts_usable() {
        return false;
    }
    let Some(tx) = cx.global::<NearbyService>().events.clone() else { return false };
    let key = q.key();
    let (title, lines, code, no, yes) = texts(q);
    let mut all: Vec<String> = code.map(|c| format!("{} {c}", t!("Code:"))).into_iter().collect();
    all.extend(lines);
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    let buttons = [
        leaf_sys::toast_post::Button { label: no.to_string(), argument: format!("nearby:no:{key}") },
        leaf_sys::toast_post::Button { label: yes.to_string(), argument: format!("nearby:yes:{key}") },
    ];
    let toast = leaf_sys::toast_post::Toast { tag: &key, title: &title, lines: &refs, launch: &format!("nearby:show:{key}"), buttons: &buttons, stays: true };
    match leaf_sys::toast_post::show(crate::installer::APP_ID, &toast, move |arg| {
        let _ = tx.unbounded_send(Ev::Toast(arg));
    }) {
        Ok(()) => true,
        Err(e) => {
            log::info!("nearby: no toast ({e}); asking in the window");
            false
        }
    }
}

fn on_toast(arg: &str, cx: &mut App) {
    let mut parts = arg.splitn(3, ':');
    let (Some("nearby"), Some(kind), Some(rest)) = (parts.next(), parts.next(), parts.next()) else { return };
    match kind {
        "yes" | "no" => {
            let q = cx.global::<NearbyService>().questions.iter().find(|q| q.key() == rest).cloned();
            if let Some(q) = q {
                answer(&q, kind == "yes", cx);
            }
        }
        // a click on the question's toast itself: it goes away, so the question moves to the window
        "show" => {
            cx.global_mut::<NearbyService>().in_toast.remove(rest);
            sync_window(cx);
        }
        "open" => {
            crate::workspace::route_ipc_shown(format!("{}{rest}", crate::workspace::SELECT_PREFIX), cx);
        }
        _ => {}
    }
}

/// A trusted device's files started coming in without a question: said in a toast, with Windows' notification sound, so the user knows
/// (the user asked for it). Without toasts, the sound alone.
fn incoming_started(offer: &Offer, cx: &mut App) {
    let what = match offer.names.len() {
        0 => crate::i18n::items(offer.file_count as usize),
        1 => offer.names[0].clone(),
        n => format!("{} {} {}", offer.names[0], t!("and"), crate::i18n::items(n - 1)),
    };
    let line = format!("{what} · {}", rf_core::format::size(offer.total_bytes));
    let shown = toasts_usable()
        && cx.global::<NearbyService>().events.clone().is_some_and(|tx| {
            let title = format!("{} {}", offer.from_name, t!("is sending you files"));
            let lines = [line.as_str()];
            let toast = leaf_sys::toast_post::Toast { tag: &format!("in-{}", offer.transfer), title: &title, lines: &lines, launch: "nearby:none:", buttons: &[], stays: false };
            leaf_sys::toast_post::show(crate::installer::APP_ID, &toast, move |arg| {
                let _ = tx.unbounded_send(Ev::Toast(arg));
            })
            .is_ok()
        });
    if !shown {
        leaf_sys::sound::notify();
    }
}

/// Files came in: each is marked as come from another computer (SmartScreen asks before a program among them runs), and the installed
/// app says so in a toast that opens them.
fn arrived(transfer: u64, from: String, paths: Vec<String>, cx: &mut App) {
    let referrer = format!("nearby://{from}");
    let marked = paths.clone();
    std::thread::spawn(move || {
        for p in &marked {
            mark_tree(std::path::Path::new(p), &referrer);
        }
    });
    let Some(first) = paths.first().cloned() else { return };
    if !toasts_usable() {
        return;
    }
    let Some(tx) = cx.global::<NearbyService>().events.clone() else { return };
    let name = std::path::Path::new(&first).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let what = if paths.len() == 1 { name } else { format!("{name} {} {}", t!("and"), crate::i18n::items(paths.len() - 1)) };
    let title = format!("{from} {}", t!("sent you files"));
    let lines = [what.as_str(), t!("Click to see them")];
    let toast = leaf_sys::toast_post::Toast { tag: &format!("got-{transfer}"), title: &title, lines: &lines, launch: &format!("nearby:open:{first}"), buttons: &[], stays: false };
    if let Err(e) = leaf_sys::toast_post::show(crate::installer::APP_ID, &toast, move |arg| {
        let _ = tx.unbounded_send(Ev::Toast(arg));
    }) {
        log::info!("nearby: no toast for an arrival: {e}");
    }
}

/// Marks a received file, or every file in a received folder (links are not followed).
fn mark_tree(path: &std::path::Path, from: &str) {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return };
    if meta.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            for e in rd.flatten() {
                mark_tree(&e.path(), from);
            }
        }
    } else if meta.is_file()
        && let Err(e) = leaf_sys::fileinfo::mark_downloaded(path, from)
    {
        log::info!("nearby: cannot mark {}: {e}", path.display());
    }
}

/// The question window: open while a question is asked in it (not in a toast), closed when none is, redrawn when they change.
fn sync_window(cx: &mut App) {
    let Some(f) = cx.try_global::<NearbyService>() else { return };
    let empty = f.questions.iter().all(|q| f.in_toast.contains(&q.key()));
    let window = f.window;
    match (empty, window) {
        (true, Some(w)) => {
            cx.global_mut::<NearbyService>().window = None;
            let _ = w.update(cx, |_, window, _| window.remove_window());
        }
        (false, Some(w)) => {
            if w.update(cx, |_, _, cx| cx.notify()).is_err() {
                cx.global_mut::<NearbyService>().window = None;
                open_window(cx);
            }
        }
        (false, None) => open_window(cx),
        (true, None) => {}
    }
}

/// At the bottom right of the main screen, over the other windows, without taking the keyboard from the app in front.
fn open_window(cx: &mut App) {
    let Some(display) = cx.primary_display() else { return };
    let area = display.visible_bounds();
    let origin = point(area.origin.x + area.size.width - px(WINDOW_W + 16.), area.origin.y + area.size.height - px(WINDOW_H + 16.));
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size(px(WINDOW_W), px(WINDOW_H))))),
        display_id: Some(display.id()),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        // the app's own popup look: on HyperGlass when that is the backdrop (as the quick search), else Windows' acrylic
        window_background: crate::theme::popup_material(glassy(cx)),
        ..Default::default()
    };
    let glass = glassy(cx);
    match cx.open_window(opts, move |window, cx| {
        let hwnd = crate::look::window::hwnd_of(window);
        leaf_sys::window::set_dark_mode(hwnd, Palette::for_window(window).dark);
        cx.new(|cx| QuestionView::new(glass, window, cx))
    }) {
        Ok(w) => cx.global_mut::<NearbyService>().window = Some(w),
        Err(e) => log::warn!("nearby: question window: {e:#}"),
    }
}

/// HyperGlass is the app's backdrop (and this Windows has the materials it is drawn with).
fn glassy(cx: &App) -> bool {
    crate::settings::materials_available() && cx.try_global::<Settings>().is_some_and(|s| s.backdrop_in_use() == crate::settings::BackdropPref::Glass)
}

struct QuestionView {
    glass: Option<crate::glass_watch::PopupGlass>,
    _glass_watch: Option<Task<()>>,
}

impl QuestionView {
    fn new(glassy: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let glass = glassy.then(|| crate::glass_watch::PopupGlass::start_panel(crate::look::window::hwnd_of(window), Palette::for_window(window).dark));
        let watch = glass.is_some().then(|| crate::glass_watch::watch(cx, |this: &mut Self| this.glass.as_mut()));
        QuestionView { glass, _glass_watch: watch }
    }
}

impl Render for QuestionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        // (the questions asked in a toast are not asked here too)
        let questions: Vec<Question> = cx
            .try_global::<NearbyService>()
            .map(|f| f.questions.iter().filter(|q| !f.in_toast.contains(&q.key())).cloned().collect())
            .unwrap_or_default();
        let Some(q) = questions.first().cloned() else { return div().into_any_element() };
        let waiting = questions.len() - 1;
        let (title, lines, code, no, yes) = texts(&q);
        let choose = match &q {
            Question::Ls(r) => Some(r.clone()),
            _ => None,
        };
        let (qn, qy) = (q.clone(), q);
        // over glass a wash, or the plain fill until the glass runs (never a see-through window with nothing behind it)
        let fill = self.glass.as_ref().map(|g| crate::glass_watch::panel_fill(p.dark, g.fallback())).or_else(|| crate::theme::popup_fill(&p));
        let content = v_flex()
            .w_full()
            .p_4()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(L::Send).size(px(16.)).text_color(p.accent))
                    .child(div().text_xs().text_color(p.text_secondary).child("Nearby"))
                    .children((waiting > 0).then(|| div().ml_auto().text_xs().text_color(p.text_secondary).child(format!("+{waiting}")))),
            )
            .child(div().mt_1().text_sm().font_weight(FontWeight::SEMIBOLD).whitespace_normal().child(title))
            .children(lines.into_iter().map(|line| div().text_xs().text_color(p.text_secondary).whitespace_normal().child(line)))
            .children(code.map(|c| div().mt_1().text_2xl().font_weight(FontWeight::BOLD).child(c)))
            .child(
                h_flex()
                    .mt_3()
                    .gap_2()
                    .justify_end()
                    .child(crate::settings_view::button("nearby-no", &p, no, move |_, cx| answer(&qn, false, cx)))
                    .children(choose.map(|r| crate::settings_view::button("nearby-choose", &p, t!("Choose folder…"), move |_, cx| choose_folder(r.clone(), cx))))
                    .child(primary("nearby-yes", &p, yes, move |_, cx| answer(&qy, true, cx))),
            );
        // the window is as tall as what it says, growing upwards (its bottom stays above the taskbar)
        let vh = window.viewport_size().height;
        div()
            .size_full()
            .when_some(fill, |d, f| d.bg(f))
            .text_color(p.text)
            .child(
                div().relative().w_full().child(content).child(
                    canvas(
                        move |b, window, _| {
                            let h = b.size.height.ceil();
                            if (f32::from(h) - f32::from(vh)).abs() > 1. {
                                window.on_next_frame(move |w, _| {
                                    let hwnd = crate::look::window::hwnd_of(w);
                                    let new_h = (f32::from(h) * w.scale_factor()).round() as i32;
                                    if let Some((r, _)) = leaf_sys::window::rect(hwnd) {
                                        leaf_sys::window::place_quietly(hwnd, [r[0], r[3] - new_h, r[2], r[3]]);
                                    }
                                });
                            }
                        },
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .inset_0(),
                ),
            )
            .into_any_element()
    }
}

fn primary(id: &'static str, p: &Palette, label: &str, run: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    div()
        .id(id)
        .h(px(32.))
        .px_4()
        .flex()
        .items_center()
        .rounded(theme::RADIUS_SM)
        .bg(p.accent)
        .text_color(p.accent_text)
        .text_sm()
        .cursor_pointer()
        .tab_index(0)
        .hover(|s| s.opacity(0.9))
        .active(|s| s.opacity(0.8))
        .child(label.to_string())
        .on_click(move |_, window, cx| run(window, cx))
        .into_any_element()
}

// ------------------------------------------------------------------------------------------------------------------ sending

/// The "Send with Nearby" submenu for `paths`: the devices seen now (the trusted ones first), and the way to the settings.
pub(crate) fn menu_items(paths: &[PathBuf], cx: &App) -> Vec<Item> {
    let peers = cx.try_global::<NearbyService>().map(|f| f.peers.clone()).unwrap_or_default();
    let (ls, ls_devices) = cx.try_global::<NearbyService>().map(|f| (f.ls.clone(), f.ls_devices.clone())).unwrap_or_default();
    // (opening the menu is the sending UI being used: devices are looked for now, at most every 30 seconds)
    if let Some(ls) = &ls {
        ls.scan_if_stale();
    }
    let mut v: Vec<Item> = peers
        .into_iter()
        .map(|peer| {
            let paths = paths.to_vec();
            let icon = MenuIcon::Lucide(if peer.trusted { L::ShieldCheck } else { L::Monitor });
            Item::action(peer.name.clone(), icon, move |_, cx| send(&peer, paths.clone(), cx))
        })
        .collect();
    v.extend(ls_devices.into_iter().map(|dev| {
        let paths = paths.to_vec();
        Item::action(dev.alias.clone(), MenuIcon::Lucide(L::Monitor), move |_, cx| ls_send(&dev, paths.clone(), cx))
    }));
    if v.is_empty() {
        v.push(Item::action(t!("No devices nearby"), MenuIcon::Lucide(L::Wifi), |_, _| {}).enabled(false));
    }
    v.push(Item::Separator);
    v.push(Item::action(t!("Nearby settings…"), MenuIcon::Lucide(L::Settings), |_, cx| open_settings(cx)));
    v
}

fn ls_send(dev: &LsDevice, paths: Vec<PathBuf>, cx: &mut App) {
    let Some(ls) = cx.global::<NearbyService>().ls.clone() else { return };
    match ls.send(&dev.fingerprint, paths) {
        Ok(transfer) => {
            cx.global_mut::<NearbyService>().ls_last = Some(dev.fingerprint.clone());
            track(cx, transfer, format!("{}: {}", t!("Sending with Nearby"), dev.alias), None);
        }
        Err(e) => log::warn!("localsend: cannot send to {}: {e}", dev.alias),
    }
}

/// The "Send to device" command: to the device used last, or to the only one found; else a card says where to choose.
pub(crate) fn send_selection(paths: Vec<PathBuf>, cx: &mut App) {
    if paths.is_empty() || !cx.has_global::<NearbyService>() {
        return;
    }
    let f = cx.global::<NearbyService>();
    let (ls, devs, last) = (f.ls.clone(), f.ls_devices.clone(), f.ls_last.clone());
    if let Some(ls) = &ls {
        ls.scan_if_stale();
    }
    let pick = devs.iter().find(|d| last.as_deref() == Some(d.fingerprint.as_str())).or(if devs.len() == 1 { devs.first() } else { None }).cloned();
    match pick {
        Some(dev) => ls_send(&dev, paths, cx),
        None => {
            let msg = if devs.is_empty() { t!("No devices found nearby. LocalSend must be open on the other device.") } else { t!("Right-click the items and choose Send with Nearby to pick a device.") };
            crate::status_center::start_with_id(cx, t!("Send to device").to_string(), move |_| Err(anyhow::anyhow!("{msg}")), None);
        }
    }
}

pub(crate) fn open_settings(cx: &mut App) {
    crate::settings_view::open_page(crate::settings_view::Page::Nearby, cx);
    crate::workspace::show_settings(cx);
}

fn send(peer: &Peer, paths: Vec<PathBuf>, cx: &mut App) {
    let Some(n) = cx.global::<NearbyService>().nearby.clone() else { return };
    match n.send(peer.id.clone(), paths.iter().map(|p| p.to_string_lossy().into_owned()).collect()) {
        Ok(transfer) => track(cx, transfer, format!("{}: {}", t!("Sending with Nearby"), peer.name), None),
        Err(e) => log::warn!("nearby: cannot send to {}: {e}", peer.name),
    }
}

// ------------------------------------------------------------------------------------------------------------------ settings

/// Settings > Nearby.
pub(crate) fn page(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    use crate::settings_view::{button, card, dropdown, section, switch};
    let on = s.nearby_enabled;
    let mut v = vec![card(
        p,
        L::Send,
        t!("Use Nearby"),
        Some(t!("leaf on the same network find each other; files go encrypted straight from device to device, with no server or account. When you turn it on, the firewall may ask you to let leaf use the network: allow it for private networks.")),
        switch("nearby-on", p, on, move |_, cx| settings::update(cx, |s| s.nearby_enabled = !on)),
    )];
    v.extend(ls_cards(p, s, cx));
    v.extend(crate::share_ui::settings_cards(p, s, cx));
    if !on {
        return v;
    }
    let (nearby, peers, problem) = cx.try_global::<NearbyService>().map(|f| (f.nearby.clone(), f.peers.clone(), f.problem.clone())).unwrap_or_default();
    if let Some(problem) = problem {
        v.push(card(p, L::TriangleAlert, t!("Can't look for nearby devices"), Some(problem.as_str()), div().into_any_element()));
    }
    v.push(card(
        p,
        L::Eye,
        t!("Who can see this PC"),
        Some(t!("Everyone lasts 10 minutes, then it goes back to trusted devices only. To take files from a device you haven't paired with, or to pair with one, choose Everyone.")),
        dropdown(
            "nearby-vis",
            p,
            s.nearby_visibility,
            vec![
                (NearbyVisibility::Off, t!("No one").to_string()),
                (NearbyVisibility::Trusted, t!("Trusted devices only").to_string()),
                (NearbyVisibility::Everyone, t!("Everyone (10 minutes)").to_string()),
            ],
            |s, v| {
                s.nearby_visibility = v;
                CHOSEN.store(true, Ordering::Relaxed);
            },
        ),
    ));
    let name = name_of(s);
    v.push(card(
        p,
        L::Pencil,
        t!("Device name"),
        Some(name.as_str()),
        button("nearby-name", p, t!("Change…"), move |window, cx| {
            let ok = crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, |text, _, cx| {
                let text = text.unwrap_or_default().trim().to_string();
                settings::update(cx, |s| s.nearby_name = text);
                Ok(())
            });
            let current = cx.global::<Settings>().nearby_name.clone();
            crate::prompt::show(
                t!("Device name"),
                Some(t!("Other devices see this PC by this name. Leave it empty to use the computer's name.").to_string()),
                Some((current, std::env::var("COMPUTERNAME").unwrap_or_default())),
                vec![ok, crate::prompt::Button::cancel()],
                window,
                cx,
            );
        }),
    ));
    let inbox = inbox_of(s).to_string_lossy().into_owned();
    let custom = !s.nearby_inbox.trim().is_empty();
    v.push(card(
        p,
        L::FolderDown,
        t!("Received files"),
        Some(inbox.as_str()),
        h_flex()
            .gap_2()
            .children(custom.then(|| button("nearby-inbox-reset", p, t!("Default"), |_, cx| settings::update(cx, |s| s.nearby_inbox.clear()))))
            .child(button("nearby-inbox", p, t!("Change…"), |_, cx| {
                let (tx, rx) = futures::channel::oneshot::channel::<Option<PathBuf>>();
                let title = t!("Choose the folder for received files");
                leaf_sys::sta::run(move |owner| {
                    let _ = tx.send(leaf_sys::dialogs::pick(owner, title, leaf_sys::dialogs::Pick::Folder));
                });
                cx.spawn(async move |cx| {
                    if let Ok(Some(path)) = rx.await {
                        let _ = cx.update(|cx| settings::update(cx, |s| s.nearby_inbox = path.to_string_lossy().into_owned()));
                    }
                })
                .detach();
            }))
            .into_any_element(),
    ));
    let auto = s.nearby_auto_accept_trusted;
    v.push(card(
        p,
        L::ShieldCheck,
        t!("Take files from trusted devices without asking"),
        Some(t!("Files from devices you paired with go straight to the Received files folder; other devices are asked about every time.")),
        switch("nearby-auto", p, auto, move |_, cx| settings::update(cx, |s| s.nearby_auto_accept_trusted = !auto)),
    ));

    v.push(section(p, t!("Nearby devices")));
    let everyone = s.nearby_visibility == NearbyVisibility::Everyone;
    v.push(card(
        p,
        L::RefreshCw,
        t!("Look for devices again"),
        Some(t!("Devices on the network are found by themselves; if one that just came on doesn't show up, look again.")),
        h_flex()
            .gap_2()
            .children((!everyone).then(|| button("nearby-everyone", p, t!("Show to everyone for 10 minutes"), |_, cx| settings::update(cx, |s| s.nearby_visibility = NearbyVisibility::Everyone))))
            .child(button("nearby-refresh", p, t!("Refresh"), |_, cx| {
                if let Some(n) = cx.global::<NearbyService>().nearby.clone() {
                    n.refresh();
                }
            }))
            .into_any_element(),
    ));
    if peers.is_empty() {
        v.push(card(
            p,
            L::Wifi,
            t!("No devices yet"),
            Some(t!("leaf must be open on the other device, with Nearby on. If the two haven't paired yet, at least one must be visible to Everyone.")),
            div().into_any_element(),
        ));
    }
    for (i, peer) in peers.iter().enumerate() {
        let (measuring, last) = cx.try_global::<NearbyService>().map(|f| (f.measuring.contains(&peer.id), f.speeds.get(&peer.id).cloned())).unwrap_or_default();
        let mut desc = os_name(&peer.os).to_string();
        if peer.trusted {
            desc = format!("{desc} · {}", t!("Trusted"));
        }
        if let Some(speed) = speed_text(peer, measuring, last.as_ref()) {
            desc = format!("{desc} · {speed}");
        }
        let id = peer.id.clone();
        let control = h_flex()
            .gap_2()
            .child(button(("nearby-speed", i), p, if measuring { t!("Measuring…") } else { t!("Test speed") }, move |_, cx| measure(id.clone(), cx)))
            .children((!peer.trusted).then(|| {
                let id = peer.id.clone();
                button(("nearby-pair", i), p, t!("Pair"), move |_, cx| pair(id.clone(), cx))
            }))
            .into_any_element();
        v.push(card(p, if peer.trusted { L::ShieldCheck } else { L::Monitor }, &peer.name, Some(desc.as_str()), control));
    }

    let trusted = nearby.map(|n| n.trusted()).unwrap_or_default();
    if !trusted.is_empty() {
        v.push(section(p, t!("Trusted devices")));
        for (i, peer) in trusted.iter().enumerate() {
            let id = peer.id.clone();
            v.push(card(
                p,
                L::ShieldCheck,
                &peer.name,
                None,
                button(("nearby-forget", i), p, t!("Forget"), move |_, cx| {
                    if let Some(n) = cx.global::<NearbyService>().nearby.clone()
                        && let Err(e) = n.forget(id.clone())
                    {
                        log::warn!("nearby: cannot forget a device: {e}");
                    }
                    cx.refresh_windows();
                }),
            ));
        }
    }
    v
}

/// Settings > Nearby, the LocalSend part: the switch for receiving, and the devices found (a search runs when the page opens).
fn ls_cards(p: &Palette, s: &Settings, cx: &mut App) -> Vec<AnyElement> {
    use crate::settings_view::{button, card, section, switch};
    let recv = s.nearby_localsend_receive;
    let (ls, devices, problem) = cx.try_global::<NearbyService>().map(|f| (f.ls.clone(), f.ls_devices.clone(), f.ls_problem.clone())).unwrap_or_default();
    if let Some(ls) = &ls {
        ls.scan_if_stale();
    }
    let mut v = vec![card(
        p,
        L::Download,
        t!("Receive from nearby devices"),
        Some(t!("Lets phones and computers running LocalSend send files to this PC. Off by default: while it is on, leaf listens on port 53317 and appears in their lists. Every request asks you first, and nothing is ever overwritten.")),
        switch("nearby-ls-recv", p, recv, move |_, cx| settings::update(cx, |s| s.nearby_localsend_receive = !recv)),
    )];
    if let Some(problem) = problem.filter(|_| recv) {
        v.push(card(p, L::TriangleAlert, t!("Can't receive from nearby devices"), Some(problem.as_str()), div().into_any_element()));
    }
    let pin_set = !s.nearby_localsend_pin.trim().is_empty();
    v.push(card(
        p,
        L::Lock,
        t!("PIN for receiving"),
        Some(if pin_set { t!("A device must give this PIN before it can send here.") } else { t!("None: every request is asked about, but no PIN is needed.") }),
        h_flex()
            .gap_2()
            .children(pin_set.then(|| button("nearby-ls-pin-off", p, t!("Remove"), |_, cx| settings::update(cx, |s| s.nearby_localsend_pin.clear()))))
            .child(button("nearby-ls-pin", p, t!("Set…"), |window, cx| {
                let ok = crate::prompt::Button::new(t!("Save"), crate::prompt::Kind::Primary, |text, _, cx| {
                    let text = text.unwrap_or_default().trim().to_string();
                    settings::update(cx, |s| s.nearby_localsend_pin = text);
                    Ok(())
                });
                let current = cx.global::<Settings>().nearby_localsend_pin.clone();
                crate::prompt::show(
                    t!("PIN for receiving"),
                    Some(t!("Devices sending to this PC over LocalSend must type this PIN first. Leave it empty for none.").to_string()),
                    Some((current, "PIN".to_string())),
                    vec![ok, crate::prompt::Button::cancel()],
                    window,
                    cx,
                );
            }))
            .into_any_element(),
    ));
    v.push(section(p, t!("LocalSend devices")));
    v.push(card(
        p,
        L::RefreshCw,
        t!("Look for LocalSend devices"),
        Some(t!("Phones and computers with LocalSend open on this network. Send to them from the right-click menu: Send with Nearby.")),
        button("nearby-ls-scan", p, t!("Refresh"), |_, cx| {
            if let Some(ls) = cx.global::<NearbyService>().ls.clone() {
                ls.scan();
            }
        }),
    ));
    v.push(card(
        p,
        L::Plus,
        t!("Add device by address"),
        Some(t!("For a device the search does not find, such as one on another network: its address, like 192.168.1.20 or 192.168.1.20:53317.")),
        button("nearby-ls-add", p, t!("Add…"), |window, cx| {
            let ok = crate::prompt::Button::new(t!("Add"), crate::prompt::Kind::Primary, |text, _, cx| {
                let text = text.unwrap_or_default();
                let Some(addr) = parse_address(&text) else {
                    return Err(t!("Enter an address like 192.168.1.20 or 192.168.1.20:53317").into());
                };
                if let Some(ls) = cx.global::<NearbyService>().ls.clone() {
                    ls.add_device(addr);
                }
                Ok(())
            });
            crate::prompt::show(t!("Add device by address"), None, Some((String::new(), "192.168.1.20".to_string())), vec![ok, crate::prompt::Button::cancel()], window, cx);
        }),
    ));
    for d in &devices {
        let desc = format!("{} · {}", d.model.clone().unwrap_or_else(|| d.device_type.clone()), d.ip);
        v.push(card(p, L::Monitor, &d.alias, Some(desc.as_str()), div().into_any_element()));
    }
    v
}

/// "192.168.1.20", "192.168.1.20:53317" or "[::1]:53317": where a LocalSend device is (the port defaults to LocalSend's).
fn parse_address(text: &str) -> Option<std::net::SocketAddr> {
    let text = text.trim();
    text.parse::<std::net::SocketAddr>().ok().or_else(|| text.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok().map(|ip| std::net::SocketAddr::new(ip, LS_PORT)))
}

/// Settings > Nearby was drawn in `window`. While it is on screen this PC is seen by everyone, the way a Mac is while Finder's AirDrop
/// window is open (the user asked for both platforms): the first drawing remembers the visibility and sets Everyone, and when the
/// page has not been drawn for [`PAGE_GONE`] (left, another tab, the window closed or minimized) the remembered value comes back, if
/// it is still Everyone. A choice in the list, or the ten-minute fall-back, is kept instead.
pub(crate) fn page_seen(window: AnyWindowHandle, cx: &mut App) {
    if !cx.has_global::<NearbyService>() || cx.global::<NearbyService>().nearby.is_none() {
        return;
    }
    let vis = cx.global::<Settings>().nearby_visibility;
    let f = cx.global_mut::<NearbyService>();
    if let Some(view) = f.viewing.as_mut() {
        view.seen = std::time::Instant::now();
        view.window = window;
        if CHOSEN.swap(false, Ordering::Relaxed) {
            view.restore = None;
        }
        return;
    }
    CHOSEN.store(false, Ordering::Relaxed);
    let restore = (vis != NearbyVisibility::Everyone).then_some(vis);
    f.viewing = Some(Viewing { window, seen: std::time::Instant::now(), restore });
    if restore.is_some() {
        // (not while the page is drawn)
        cx.defer(|cx| settings::update(cx, |s| s.nearby_visibility = NearbyVisibility::Everyone));
    }
    // the heartbeat: the page's window is redrawn now and then, and the page that is no longer drawn is gone
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            let over = cx.update(|cx| {
                let Some(view) = cx.has_global::<NearbyService>().then(|| cx.global::<NearbyService>().viewing.as_ref().map(|v| (v.window, v.seen))).flatten() else {
                    return true;
                };
                let minimized = view.0.update(cx, |_, w, _| leaf_sys::window::is_minimized(crate::look::window::hwnd_of(w))).unwrap_or(true);
                if minimized || view.1.elapsed() > PAGE_GONE {
                    page_gone(cx);
                    return true;
                }
                let _ = view.0.update(cx, |_, w, _| w.refresh());
                false
            });
            if over {
                break;
            }
        }
    })
    .detach();
}

fn page_gone(cx: &mut App) {
    let Some(view) = cx.global_mut::<NearbyService>().viewing.take() else { return };
    let chosen = CHOSEN.swap(false, Ordering::Relaxed);
    if let Some(back) = view.restore
        && !chosen
        && cx.global::<Settings>().nearby_visibility == NearbyVisibility::Everyone
    {
        settings::update(cx, |s| s.nearby_visibility = back);
    }
}

fn os_name(os: &str) -> &str {
    match os {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    }
}

/// The speed part of a device's line: the test running, its result, or the estimate (measured or from a transfer: "~48 MB/s"; from
/// the two links: "≈ 52 MB/s (Wi-Fi)"), else the round trip.
fn speed_text(peer: &Peer, measuring: bool, last: Option<&SpeedResult>) -> Option<String> {
    let per_s = |bps: u64| format!("{}/s", rf_core::format::size(bps));
    if measuring {
        return Some(t!("testing speed…").to_string());
    }
    match last {
        Some(r) if r.ok => return Some(format!("↑ {} · ↓ {} · {} ms", per_s(r.up_bps), per_s(r.down_bps), r.rtt_ms)),
        Some(r) => return Some(format!("{}: {}", t!("Couldn't measure"), failure_text(r.error.as_deref()))),
        None => {}
    }
    match peer.estimate_source {
        EstimateSource::Measured | EstimateSource::Transfer => Some(format!("~{}", per_s(peer.estimate_bps))),
        EstimateSource::Link => {
            let link = match peer.link_kind {
                LinkKind::Wifi => "Wi-Fi",
                LinkKind::Ethernet => "Ethernet",
                _ => t!("network"),
            };
            Some(format!("≈ {} ({link})", per_s(peer.estimate_bps)))
        }
        EstimateSource::Unknown => (peer.rtt_ms > 0).then(|| format!("{} ms", peer.rtt_ms)),
    }
}

fn measure(peer_id: String, cx: &mut App) {
    let Some(n) = cx.global::<NearbyService>().nearby.clone() else { return };
    if cx.global::<NearbyService>().measuring.contains(&peer_id) {
        return;
    }
    match n.measure_speed(peer_id.clone()) {
        Ok(_) => {
            let f = cx.global_mut::<NearbyService>();
            f.speeds.remove(&peer_id);
            f.measuring.insert(peer_id);
            cx.refresh_windows();
        }
        Err(e) => log::warn!("nearby: cannot measure: {e}"),
    }
}

/// Tells the crate this PC's network link and keeps telling it while Nearby runs (a Wi-Fi rate drifts; the crate announces a real
/// change only). Read every 30 seconds, off the UI thread: one routing-table look and one interface row.
fn watch_link(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            let link = cx.background_executor().spawn(async { leaf_sys::network::lan_link() }).await;
            let running = cx.update(|cx| {
                let Some(n) = cx.has_global::<NearbyService>().then(|| cx.global::<NearbyService>().nearby.clone()).flatten() else { return false };
                let (kind, mbps) = match link {
                    Some((leaf_sys::network::LinkType::Wifi, m)) => (LinkKind::Wifi, m),
                    Some((leaf_sys::network::LinkType::Ethernet, m)) => (LinkKind::Ethernet, m),
                    Some((leaf_sys::network::LinkType::Other, m)) => (LinkKind::Other, m),
                    None => (LinkKind::Unknown, 0),
                };
                n.set_link(kind, mbps);
                true
            });
            if !running {
                break;
            }
            cx.background_executor().timer(Duration::from_secs(30)).await;
        }
    })
    .detach();
}

fn pair(peer_id: String, cx: &mut App) {
    let Some(n) = cx.global::<NearbyService>().nearby.clone() else { return };
    if let Err(e) = n.pair(peer_id) {
        log::warn!("nearby: cannot pair: {e}");
    }
}

#[cfg(test)]
mod tests {
    // (not `super::*`: gpui's own `test` attribute would come in with it)
    use super::{Question, failure_text, texts};
    use rf_nearby::Offer;

    fn offer(trusted: bool, names: &[&str]) -> Offer {
        Offer {
            id: 7,
            transfer: 8,
            from_id: "abc".into(),
            from_name: "Mac".into(),
            from_os: "macos".into(),
            trusted,
            names: names.iter().map(|n| n.to_string()).collect(),
            file_count: 3,
            total_bytes: 2048,
        }
    }

    #[test]
    fn an_unpaired_device_is_said_to_be_unverified() {
        let (_, lines, code, _, _) = texts(&Question::Offer(offer(false, &["photo.jpg"])));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("photo.jpg"));
        assert!(code.is_none());
        let (_, lines, _, _, _) = texts(&Question::Offer(offer(true, &["photo.jpg", "docs"])));
        assert_eq!(lines.len(), 1, "a paired device needs no warning: {lines:?}");
        assert!(lines[0].starts_with("photo.jpg "), "{lines:?}");
    }

    #[test]
    fn a_pairing_shows_its_code_in_two_halves() {
        let q = Question::Pair { peer: "p".into(), name: "Mac".into(), code: "012345".into() };
        let (_, _, code, _, _) = texts(&q);
        assert_eq!(code.as_deref(), Some("012 345"));
        assert_eq!(q.key(), "pair-p");
    }

    #[test]
    fn crate_reasons_get_interface_words() {
        for reason in ["the device is busy with other offers", "the device only takes files from its trusted devices", "declined", "x did not answer"] {
            assert_ne!(failure_text(Some(reason)), reason, "{reason}");
        }
        assert_eq!(failure_text(Some("something new")), "something new");
    }
}
