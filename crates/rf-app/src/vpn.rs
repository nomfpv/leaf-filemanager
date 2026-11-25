//! leaf's VPN, leaf's face for it: the Mullvad sign-in dialog, the account and this PC's key in the system keyring (never in
//! settings, logs or error text), connect / disconnect from the settings, the title bar and the commands, the state the
//! windows show. The engine is `leaf_vpn` (the tunnel inside leaf: only leaf's own connections, nothing else on the PC).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::*;
use leaf_remote::{Secret, SecretId, Vault};
use leaf_vpn::account::{self, AccountNumber};
use leaf_vpn::mullvad::{self, Device, DnsBlocking, Mullvad, Place, Relay};

use crate::prompt::{self, Button, Kind};
use crate::settings::Settings;
use crate::t;

/// The keyring entries (service `leaf`): the account number, and this PC's device (its key, id, name and address).
const ACCOUNT: &str = "mullvad-account";
const DEVICE: &str = "mullvad-device";

/// What the settings page and the title bar show about the account (no secret in it: the last four digits only).
#[derive(Clone, Debug, Default)]
pub struct AccountView {
    pub signed_in: bool,
    /// "…1234"
    pub hint: String,
    /// Mullvad's name for this PC's device.
    pub device: String,
    pub days_left: Option<i64>,
    /// The device was removed from the account (on the website).
    pub device_gone: bool,
    /// The settings say there is a sign-in, but the keyring could not be read (locked): leaf's words and the keyring's.
    pub keyring_locked: Option<String>,
}

#[derive(Default)]
struct Ui {
    account: AccountView,
    /// A sign-in, sign-out or check is running.
    busy: bool,
    /// The last thing to tell (an error of a sign-in, the API's refusal), shown on the page.
    message: Option<String>,
    relays: Vec<Relay>,
    relays_asked: bool,
    account_asked: bool,
}

static UI: Mutex<Option<Ui>> = Mutex::new(None);
/// When this run signed in (a connect in the minutes after gives the servers time to learn the new key).
static SIGNED_IN_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
static UI_CHANGES: AtomicU64 = AtomicU64::new(0);

fn with_ui<T>(f: impl FnOnce(&mut Ui) -> T) -> T {
    let mut g = UI.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Ui::default))
}

fn ui_changed() {
    UI_CHANGES.fetch_add(1, Ordering::AcqRel);
}

pub fn account_view() -> AccountView {
    with_ui(|u| u.account.clone())
}

pub fn busy() -> bool {
    with_ui(|u| u.busy)
}

pub fn message() -> Option<String> {
    with_ui(|u| u.message.clone())
}

pub fn relays() -> Vec<Relay> {
    with_ui(|u| u.relays.clone())
}

pub fn clear_message() {
    set_message(None);
}

fn set_message(m: Option<String>) {
    with_ui(|u| u.message = m);
    ui_changed();
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

// ------------------------------------------------------------------------------------------------ the keyring

fn vault() -> Vault {
    leaf_remote::vault()
}

/// The account and the device from the keyring (blocking: never on the UI thread).
fn kept(v: &Vault) -> Option<(AccountNumber, Device)> {
    let acc = v.get(&SecretId::vpn(ACCOUNT)).and_then(|s| AccountNumber::parse(s.as_str()))?;
    let dev = v.get(&SecretId::vpn(DEVICE)).and_then(|s| Device::from_kept(s.as_str()))?;
    Some((acc, dev))
}

/// [`kept`], telling "nothing kept" (`Ok(None)`) from "the keyring cannot be read" (`Err`: locked, or no keyring).
fn kept_checked(v: &Vault) -> Result<Option<(AccountNumber, Device)>, String> {
    let acc = v.try_get(&SecretId::vpn(ACCOUNT))?.and_then(|s| AccountNumber::parse(s.as_str()));
    let dev = v.try_get(&SecretId::vpn(DEVICE))?.and_then(|s| Device::from_kept(s.as_str()));
    Ok(acc.zip(dev))
}

/// Keeps a sign-in in the keyring, and only there.
pub(crate) fn keep_sign_in(v: &Vault, acc: &AccountNumber, dev: &Device) -> Result<(), String> {
    v.keep(&SecretId::vpn(ACCOUNT), &Secret::new(acc.expose().to_string()))?;
    v.keep(&SecretId::vpn(DEVICE), &Secret::new(dev.to_kept().to_string()))
}

/// Deletes both entries; the first failure is returned (the other is still tried).
fn forget_sign_in(v: &Vault) -> Result<(), String> {
    let mut first = None;
    for item in [DEVICE, ACCOUNT] {
        if let Err(e) = v.forget(&SecretId::vpn(item)) {
            log::warn!("vpn: {e}");
            first.get_or_insert(e);
        }
    }
    first.map_or(Ok(()), Err)
}

// ------------------------------------------------------------------------------------------------ start

/// At start: the runtime for the tunnel, the kill switch from the settings, the account's state (in the background), the
/// windows redrawn when the VPN's state changes, and the tunnel brought up when "Connect when leaf starts" is on.
/// Whether the VPN's start-up work (account, proxy, the poll that redraws the windows) is running.
static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn init(cx: &mut App) {
    // (the VPN module off: nothing starts; turning it on later calls this from `settings_changed`)
    if !crate::modules::enabled(cx, crate::modules::Module::Vpn) || STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    static LIB: std::sync::Once = std::sync::Once::new();
    LIB.call_once(|| leaf_vpn::init(leaf_remote::runtime_handle));
    let s = cx.global::<Settings>();
    leaf_vpn::set_kill_switch(s.vpn_kill_switch);
    // (the keyring is opened at start only when there is a sign-in in it)
    if s.vpn_signed_in {
        let connect_now = s.vpn_connect_on_start && std::env::var_os("LEAF_MULTI_INSTANCE").is_none();
        // (from here, what is set to use the VPN waits for it: restored downloads must not start direct while the account
        // is read)
        if connect_now {
            leaf_vpn::hold();
        }
        refresh_account(cx, connect_now);
    }
    // (the proxy for other programs refuses everything until the tunnel is up)
    if cx.global::<Settings>().vpn_socks && std::env::var_os("LEAF_MULTI_INSTANCE").is_none() {
        apply_proxy(cx);
    }
    cx.spawn(async move |cx| {
        let (mut seen_vpn, mut seen_ui) = (leaf_vpn::changes(), UI_CHANGES.load(Ordering::Acquire));
        loop {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            // (switched off while running: the poll ends)
            if !cx.update(|cx| crate::modules::enabled(cx, crate::modules::Module::Vpn)) {
                STARTED.store(false, Ordering::Release);
                break;
            }
            let (v, u) = (leaf_vpn::changes(), UI_CHANGES.load(Ordering::Acquire));
            if (v, u) != (seen_vpn, seen_ui) {
                (seen_vpn, seen_ui) = (v, u);
                cx.update(|cx| cx.refresh_windows());
            }
        }
    })
    .detach();
}

/// The settings page is open: the account is read once (when the settings say there is one).
pub fn want_account(cx: &mut App) {
    let first = with_ui(|u| !std::mem::replace(&mut u.account_asked, true));
    if first && cx.global::<Settings>().vpn_signed_in && !account_view().signed_in && !busy() {
        refresh_account(cx, false);
    }
}

/// "Try again" after a locked keyring.
pub fn retry_account(cx: &mut App) {
    if !busy() {
        refresh_account(cx, false);
    }
}

/// Reads the account from the keyring and asks Mullvad for the days left (and whether this device is still on the account).
fn refresh_account(cx: &mut App, then_connect: bool) {
    with_ui(|u| {
        u.busy = true;
        u.account_asked = true;
    });
    ui_changed();
    let work = cx.background_executor().spawn(async move {
        let v = vault();
        let (acc, dev) = match kept_checked(&v) {
            Ok(Some(k)) => k,
            // (an empty keyring that works: the sign-in is gone)
            Ok(None) => return (AccountView::default(), Some(String::new())),
            // one that does not open says nothing about it: the page says so instead of offering a second sign-in
            Err(why) => {
                let locked = format!("{}\n{why}", t!("The system keyring cannot be read (it may be locked). Unlock it, then try again: signing in again would register a second device."));
                return (AccountView { keyring_locked: Some(locked), ..AccountView::default() }, None);
            }
        };
        let mut view = AccountView { signed_in: true, hint: acc.hint(), device: dev.name.clone(), ..AccountView::default() };
        let rt = leaf_remote::runtime_handle();
        let status = rt.block_on(account::status(&acc, &dev.id));
        match status {
            Ok((expiry, present)) => {
                view.days_left = account::days_left(&expiry, now_unix());
                view.device_gone = !present;
            }
            Err(e) => log::info!("vpn: the account could not be checked: {e}"),
        }
        (view, None)
    });
    cx.spawn(async move |cx| {
        let (view, gone) = work.await;
        let signed_in = view.signed_in && !view.device_gone;
        with_ui(|u| {
            u.account = view;
            u.busy = false;
        });
        ui_changed();
        if gone.is_some() {
            let _ = cx.update(|cx| crate::settings::update(cx, |s| s.vpn_signed_in = false));
        }
        if then_connect && signed_in {
            let _ = cx.update(connect);
        } else if then_connect {
            // (nothing to connect with: the hold of `leaf_vpn::hold` ends)
            leaf_vpn::disconnect();
        }
    })
    .detach();
}

/// The relay list for the page's country list (asked once per run; it is public).
pub fn want_relays(cx: &mut App) {
    if with_ui(|u| std::mem::replace(&mut u.relays_asked, true)) {
        return;
    }
    let work = cx.background_executor().spawn(async move { leaf_remote::runtime_handle().block_on(mullvad::relays()) });
    cx.spawn(async move |_| {
        match work.await {
            Ok(list) => with_ui(|u| u.relays = list),
            Err(e) => {
                log::info!("vpn: no relay list: {e}");
                with_ui(|u| u.relays_asked = false);
            }
        }
        ui_changed();
    })
    .detach();
}

// ------------------------------------------------------------------------------------------------ sign in / out

/// "Sign in": the account number, once. It goes to Mullvad (a new key for this PC is registered) and into the keyring.
pub fn sign_in_dialog(window: &mut Window, cx: &mut App) {
    let go = Button::new(t!("Sign in"), Kind::Primary, |text, _, cx| {
        let text = zeroize::Zeroizing::new(text.unwrap_or_default());
        let Some(acc) = AccountNumber::parse(&text) else {
            return Err(t!("Enter the 16 digits of your Mullvad account number").to_string());
        };
        sign_in(acc, cx);
        Ok(())
    });
    prompt::show_secret(
        t!("Sign in to Mullvad"),
        Some(t!("Your account number is kept in the system keyring only. leaf registers this PC as one of the account's devices; only leaf's own connections use the VPN.").to_string()),
        "1234 5678 9012 3456".to_string(),
        vec![go, Button::cancel()],
        window,
        cx,
    );
}

fn sign_in(acc: AccountNumber, cx: &mut App) {
    with_ui(|u| {
        u.busy = true;
        u.message = None;
    });
    ui_changed();
    let work = cx.background_executor().spawn(async move {
        let v = vault();
        // (no keyring: nothing is registered, or the device would be left on the account with its key lost)
        if let leaf_remote::Keyring::Missing(why) = v.state() {
            return Err(format!("{}\n{why}", t!("leaf keeps the Mullvad account only in the system keyring, and it cannot be used")));
        }
        let rt = leaf_remote::runtime_handle();
        // a device kept from before is taken off its account first (five devices at most); a keyring that cannot be read
        // stops here, so no second device is registered beside one that is still kept
        match kept_checked(&v) {
            Ok(Some((old_acc, old_dev))) => {
                if let Err(e) = rt.block_on(account::sign_out(&old_acc, &old_dev.id)) {
                    log::warn!("vpn: the earlier device could not be removed from its account: {e}");
                }
                forget_sign_in(&v).map_err(|e| format!("{}\n{e}", t!("The earlier sign-in could not be deleted from the system keyring")))?;
            }
            Ok(None) => {}
            Err(why) => return Err(format!("{}\n{why}", t!("The system keyring cannot be read (it may be locked). Unlock it, then sign in."))),
        }
        let signed = rt.block_on(account::sign_in(&acc)).map_err(|e| e.to_string())?;
        if let Err(e) = keep_sign_in(&v, &acc, &signed.device) {
            let _ = rt.block_on(account::sign_out(&acc, &signed.device.id));
            let _ = forget_sign_in(&v);
            return Err(format!("{}\n{e}", t!("The sign-in could not be kept in the system keyring")));
        }
        log::info!("vpn: signed in to Mullvad as device \"{}\"", signed.device.name);
        *SIGNED_IN_AT.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        Ok(AccountView {
            signed_in: true,
            hint: acc.hint(),
            device: signed.device.name.clone(),
            days_left: signed.expiry.as_deref().and_then(|e| account::days_left(e, now_unix())),
            device_gone: false,
            keyring_locked: None,
        })
    });
    cx.spawn(async move |cx| {
        let r = work.await;
        let ok = r.is_ok();
        with_ui(|u| {
            u.busy = false;
            match r {
                Ok(view) => u.account = view,
                Err(e) => u.message = Some(e),
            }
        });
        ui_changed();
        if ok {
            let _ = cx.update(|cx| crate::settings::update(cx, |s| s.vpn_signed_in = true));
        }
    })
    .detach();
}

/// "Sign out": the tunnel goes down, this PC's device is removed from the account, the keyring entries are deleted.
pub fn sign_out(cx: &mut App) {
    leaf_vpn::disconnect();
    with_ui(|u| u.busy = true);
    ui_changed();
    let work = cx.background_executor().spawn(async move {
        let v = vault();
        let note = match kept(&v) {
            Some((acc, dev)) => match leaf_remote::runtime_handle().block_on(account::sign_out(&acc, &dev.id)) {
                Ok(()) => None,
                Err(e) => Some(format!("{}\n{e}", t!("Signed out here, but Mullvad was not told: remove this device on mullvad.net (Account > Devices)."))),
            },
            None => None,
        };
        // (the entries must really be gone: otherwise the account number would stay in the keyring unseen)
        let gone = forget_sign_in(&v);
        (note, gone)
    });
    cx.spawn(async move |cx| {
        let (note, gone) = work.await;
        let left = gone.is_err();
        with_ui(|u| {
            u.busy = false;
            u.message = match gone {
                Ok(()) => note,
                Err(e) => Some(format!("{}\n{e}", t!("The account number could not be deleted from the system keyring; it is still there. Unlock the keyring, then Sign out again."))),
            };
            if !left {
                u.account = AccountView::default();
            }
        });
        ui_changed();
        if !left {
            let _ = cx.update(|cx| crate::settings::update(cx, |s| s.vpn_signed_in = false));
        }
    })
    .detach();
}

// ------------------------------------------------------------------------------------------------ connect

fn dns_of(s: &Settings) -> DnsBlocking {
    DnsBlocking { ads: s.vpn_block_ads, trackers: s.vpn_block_trackers, malware: s.vpn_block_malware, adult: s.vpn_block_adult, gambling: s.vpn_block_gambling, social: s.vpn_block_social }
}

fn place_of(s: &Settings) -> Place {
    let some = |v: &str| Some(v.trim().to_ascii_lowercase()).filter(|v| !v.is_empty());
    Place { country: some(&s.vpn_country), city: some(&s.vpn_country).and(some(&s.vpn_city)), fastest: s.vpn_fastest }
}

/// Brings the tunnel up with the settings' place and DNS choice (the key is read from the keyring off the UI thread).
pub fn connect(cx: &mut App) {
    let s = cx.global::<Settings>();
    let (place, dns) = (place_of(s), dns_of(s));
    let view = account_view();
    if !view.signed_in && s.vpn_signed_in && !busy() {
        // (not read from the keyring yet in this run: read it, then connect)
        refresh_account(cx, true);
        return;
    }
    if !view.signed_in {
        set_message(Some(t!("Sign in to Mullvad first (Settings > VPN).").to_string()));
        return;
    }
    if view.days_left.is_some_and(|d| d < 0) {
        set_message(Some(mullvad::ApiError::Expired.to_string()));
        return;
    }
    set_message(None);
    let work = cx.background_executor().spawn(async move { kept(&vault()) });
    cx.spawn(async move |_| match work.await {
        Some((_, device)) => {
            let fresh = SIGNED_IN_AT.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|t| t.elapsed() < Duration::from_secs(300));
            leaf_vpn::connect(Arc::new(Mullvad { device, place, dns, fresh }))
        }
        None => set_message(Some(t!("The Mullvad sign-in is no longer in the system keyring. Sign in again.").to_string())),
    })
    .detach();
}

/// A job that may go only through the VPN (a torrent) starts while the tunnel is off: it is brought up, when there is a
/// sign-in to do it with. Nothing while it is already connecting (also the hold of "Connect when leaf starts") or up.
pub fn connect_for_job(cx: &mut App) {
    if !crate::modules::enabled(cx, crate::modules::Module::Vpn) || !cx.global::<Settings>().vpn_signed_in || !STARTED.load(Ordering::Acquire) {
        return;
    }
    if matches!(leaf_vpn::state(), leaf_vpn::State::Off | leaf_vpn::State::Failed(_)) && leaf_vpn::tunnel().is_none() {
        log::info!("vpn: connecting for a torrent that waits for it");
        connect(cx);
    }
}

pub fn disconnect(_cx: &mut App) {
    leaf_vpn::disconnect();
}

/// A setting of the page changed: the kill switch takes it at once; a new DNS choice goes to the running tunnel; a new place
/// reconnects a running tunnel.
pub fn settings_changed(before: &Settings, cx: &mut App) {
    // the module's switch: off closes a running tunnel and lets go of the kill switch; on starts what start-up would have
    let was = crate::modules::enabled_in(before, crate::modules::Module::Vpn);
    if !crate::modules::enabled(cx, crate::modules::Module::Vpn) {
        if was {
            leaf_vpn::set_kill_switch(false);
            disconnect(cx);
        }
        return;
    }
    if !was || !STARTED.load(Ordering::Acquire) {
        init(cx);
    }
    let s = cx.global::<Settings>();
    leaf_vpn::set_kill_switch(s.vpn_kill_switch);
    if let Some(t) = leaf_vpn::tunnel()
        && dns_of(before) != dns_of(s)
    {
        t.set_dns(dns_of(s).resolver());
    }
    let proxy = (before.vpn_socks, before.vpn_socks_port, before.vpn_socks_password) != (s.vpn_socks, s.vpn_socks_port, s.vpn_socks_password);
    let moved = place_of(before) != place_of(s);
    if proxy {
        apply_proxy(cx);
    }
    if moved && !matches!(leaf_vpn::state(), leaf_vpn::State::Off | leaf_vpn::State::Failed(_)) {
        connect(cx);
    }
}

// ------------------------------------------------------------------------------------------------ other programs (SOCKS)

/// The keyring entry of the proxy's password (the user name is always `leaf_vpn::socks::USER`).
const SOCKS_PASSWORD: &str = "socks-password";

/// What the page and the browser extension are told about the proxy. No `Debug`: the password is in it.
#[derive(Clone, Default)]
pub struct ProxyView {
    /// The port it listens on, while it runs.
    pub port: Option<u16>,
    /// The password it asks for (shown on the page, handed to the leaf extension).
    pub password: Option<zeroize::Zeroizing<String>>,
    /// Why it does not run (leaf's words, the details after a line break).
    pub problem: Option<String>,
}

static PROXY: Mutex<Option<ProxyView>> = Mutex::new(None);
/// Each start or stop: a start that finishes after a newer one is thrown away.
static PROXY_GEN: AtomicU64 = AtomicU64::new(0);
/// Held while the proxy is started or stopped (the keyring is read before it).
static PROXY_LOCK: Mutex<()> = Mutex::new(());

pub fn proxy_view() -> ProxyView {
    PROXY.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

fn set_proxy_view(v: ProxyView) {
    *PROXY.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
    ui_changed();
}

/// The proxy's sign-in from the keyring; a new one is made and kept there the first time (or when `renew`).
fn socks_login(v: &Vault, renew: bool) -> Result<leaf_vpn::socks::Credentials, String> {
    if let leaf_remote::Keyring::Missing(why) = v.state() {
        return Err(format!("{}\n{why}", t!("The proxy's password is kept in the system keyring, and it cannot be used. Turn the password off, or set up the keyring.")));
    }
    let id = SecretId::vpn(SOCKS_PASSWORD);
    if !renew {
        match v.try_get(&id) {
            Ok(Some(s)) if !s.as_str().is_empty() => {
                return Ok(leaf_vpn::socks::Credentials { user: leaf_vpn::socks::USER.to_string(), password: zeroize::Zeroizing::new(s.as_str().to_string()) });
            }
            Ok(_) => {}
            Err(why) => return Err(format!("{}\n{why}", t!("The system keyring cannot be read (it may be locked), so the proxy's password is not known. Unlock it, then turn the proxy off and on."))),
        }
    }
    let c = leaf_vpn::socks::Credentials::generate();
    v.keep(&id, &Secret::new(c.password.to_string())).map_err(|e| format!("{}\n{e}", t!("The proxy's password could not be kept in the system keyring")))?;
    Ok(c)
}

/// Starts, restarts or stops the proxy as the settings say ("Let other programs use the VPN", its port and password).
pub fn apply_proxy(cx: &mut App) {
    proxy_with(cx, false);
}

/// "New password": the proxy gets a new one (programs that had the old one must be given the new one).
pub fn new_proxy_password(cx: &mut App) {
    proxy_with(cx, true);
}

fn proxy_with(cx: &mut App, renew: bool) {
    let s = cx.global::<Settings>();
    let (on, port, password) = (s.vpn_socks, s.vpn_socks_port, s.vpn_socks_password);
    let generation = PROXY_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    if !on {
        let _g = PROXY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        leaf_vpn::socks::stop();
        set_proxy_view(ProxyView::default());
        return;
    }
    let work = cx.background_executor().spawn(async move {
        let login = if password { Some(socks_login(&vault(), renew)?) } else { None };
        let shown = login.as_ref().map(|c| c.password.clone());
        let _g = PROXY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if PROXY_GEN.load(Ordering::Acquire) != generation {
            return Ok(None);
        }
        match leaf_vpn::socks::start(port, login) {
            Ok(p) => Ok(Some(ProxyView { port: Some(p), password: shown, problem: None })),
            Err(e) => {
                // (a proxy started before stays down: the page says why)
                leaf_vpn::socks::stop();
                Err(format!("{}\n{e}", t!("The proxy for other programs could not be started")))
            }
        }
    });
    cx.spawn(async move |_| {
        let view = match work.await {
            Ok(Some(v)) => v,
            Ok(None) => return,
            Err(why) => ProxyView { problem: Some(why), ..ProxyView::default() },
        };
        if PROXY_GEN.load(Ordering::Acquire) == generation {
            set_proxy_view(view);
        }
    })
    .detach();
}

/// What the leaf extension is told (its "Browse through Mullvad"): the VPN's state and exit country, and the proxy's address
/// and its own sign-in when it runs. Nothing else (no account, no key).
pub fn browser_state() -> serde_json::Value {
    let (state, country) = match leaf_vpn::state() {
        leaf_vpn::State::Off => ("off", None),
        leaf_vpn::State::Connecting { .. } => ("connecting", None),
        leaf_vpn::State::On { country, .. } if leaf_vpn::tunnel().is_some() => ("on", Some(country)),
        leaf_vpn::State::On { .. } => ("connecting", None),
        leaf_vpn::State::Failed(_) => ("failed", None),
    };
    let p = proxy_view();
    let proxy = p.port.map(|port| {
        serde_json::json!({
            "host": "127.0.0.1",
            "port": port,
            "user": p.password.as_ref().map(|_| leaf_vpn::socks::USER),
            "password": p.password.as_ref().map(|s| s.as_str()),
        })
    });
    serde_json::json!({ "v": 1, "state": state, "country": country, "proxy": proxy })
}

// ------------------------------------------------------------------------------------------------ what uses it

/// The kill switch's words, for a transfer that waits.
pub fn held_text() -> String {
    match leaf_vpn::state() {
        leaf_vpn::State::Connecting { .. } => t!("Waiting for leaf's VPN to connect.").to_string(),
        _ => t!("Waiting for leaf's VPN: it is not connected and the kill switch is on, so nothing goes out directly.").to_string(),
    }
}

/// Whether a connection to `host` that is set to use the VPN would be held by the kill switch now.
pub fn held(host: &str) -> bool {
    leaf_vpn::route(host) == leaf_vpn::Route::Blocked
}

/// A download's connections through the VPN (leaf-download's connector seam): LAN addresses direct, the rest through the
/// tunnel; while it is down they go direct, or with the kill switch on they are refused.
pub struct DownloadDialer;

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

impl leaf_download::dial::Conn for VpnConn {
    fn set_read_timeout(&mut self, t: Option<Duration>) {
        self.0.set_read_timeout(t);
    }
    fn set_write_timeout(&mut self, t: Option<Duration>) {
        self.0.set_write_timeout(t);
    }
}

impl leaf_download::dial::Dialer for DownloadDialer {
    fn generation(&self) -> u64 {
        leaf_vpn::generation()
    }

    fn takes(&self, host: &str) -> bool {
        leaf_vpn::route(host) != leaf_vpn::Route::Direct
    }

    fn dial(&self, host: &str, port: u16, timeout: Duration) -> std::io::Result<Option<Box<dyn leaf_download::dial::Conn>>> {
        match leaf_vpn::route(host) {
            leaf_vpn::Route::Direct => Ok(None),
            leaf_vpn::Route::Blocked => Err(leaf_download::dial::refused(held_text())),
            leaf_vpn::Route::Tunnel => match leaf_vpn::dial_blocking(host, port, timeout) {
                Ok(s) => Ok(Some(Box::new(VpnConn(s)))),
                Err(e) if leaf_vpn::is_blocked(&e) => Err(leaf_download::dial::refused(held_text())),
                Err(e) => Err(e),
            },
        }
    }
}

/// The state in a word and a line for the title bar's tooltip and the page.
pub fn describe(state: &leaf_vpn::State) -> (SharedString, Option<String>) {
    match state {
        leaf_vpn::State::Off => (t!("VPN off").into(), None),
        leaf_vpn::State::Connecting { place } if place.is_empty() => (t!("VPN connecting").into(), None),
        leaf_vpn::State::Connecting { place } => (t!("VPN connecting").into(), Some(place.clone())),
        leaf_vpn::State::On { country, city, unverified: Some(why), .. } => {
            (t!("VPN connected, not verified").into(), Some(format!("{country}, {city}\n{} {why}", t!("The exit could not be confirmed:"))))
        }
        leaf_vpn::State::On { country, city, exit_ip, .. } => {
            let ip = exit_ip.map(|i| i.to_string()).unwrap_or_default();
            (t!("VPN on").into(), Some(format!("{country}, {city}{}", if ip.is_empty() { String::new() } else { format!(" · {ip}") })))
        }
        leaf_vpn::State::Failed(why) => (t!("VPN not connected").into(), Some(why.clone())),
    }
}

#[cfg(test)]
mod tests {
    // (not `super::*`: that brings gpui's own `test` attribute in with the toolkit's prelude)
    use super::{AccountNumber, Device, Secret, SecretId, Settings, Vault, account_view, forget_sign_in, keep_sign_in, kept, mullvad};
    use std::sync::{Arc, Mutex};

    /// A keyring in memory: the sign-in goes there, and only there.
    #[derive(Default)]
    struct Memory(Mutex<std::collections::HashMap<String, String>>);

    impl leaf_remote::SecretStore for Memory {
        fn available(&self) -> Result<(), String> {
            Ok(())
        }
        fn get(&self, id: &SecretId) -> Result<Option<Secret>, String> {
            Ok(self.0.lock().unwrap().get(&format!("{id:?}")).cloned().map(Secret::new))
        }
        fn set(&self, id: &SecretId, s: &Secret) -> Result<(), String> {
            self.0.lock().unwrap().insert(format!("{id:?}"), s.as_str().to_string());
            Ok(())
        }
        fn delete(&self, id: &SecretId) -> Result<(), String> {
            self.0.lock().unwrap().remove(&format!("{id:?}"));
            Ok(())
        }
        fn list(&self) -> Result<Vec<SecretId>, String> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn vpn_account_and_key_stay_out_of_settings_and_debug() {
        let acc = AccountNumber::parse("1234 5678 9012 3456").unwrap();
        let key = leaf_vpn::PrivateKey::generate();
        let key_text = key.to_base64().to_string();
        let dev = Device { id: "0f1e2d3c-aaaa-bbbb-cccc-000000000001".into(), name: "happy seal".into(), ipv4: "10.64.1.2".parse().unwrap(), key };

        // kept: in the keyring, readable back
        let store = Arc::new(Memory::default());
        let v = Vault::new(store.clone());
        keep_sign_in(&v, &acc, &dev).unwrap();
        let (acc2, dev2) = kept(&v).unwrap();
        assert_eq!(acc2.expose(), "1234567890123456");
        assert_eq!(dev2.key.to_base64().as_str(), key_text);

        // never in the settings file (with every VPN setting changed) nor in anything's Debug or an error's text
        let mut s = Settings::default();
        s.vpn_country = "se".into();
        s.vpn_kill_switch = true;
        s.vpn_downloads = true;
        let file = toml::to_string(&s).unwrap() + &serde_json::to_string(&s).unwrap();
        let shown = [
            format!("{acc:?}"),
            format!("{dev:?}"),
            format!("{:?}", dev.key),
            format!("{s:?}"),
            format!("{:?}", account_view()),
            format!("{:?}", leaf_vpn::account::SignedIn { device: dev.clone(), expiry: None }),
            mullvad::ApiError::InvalidAccount.to_string(),
            mullvad::ApiError::MaxDevices.to_string(),
        ];
        for text in std::iter::once(&file).chain(shown.iter()) {
            assert!(!text.contains("1234567890123456") && !text.contains("1234 5678"), "account number in: {text}");
            assert!(!text.contains(&key_text), "private key in: {text}");
        }
        assert_eq!(acc.hint(), "…3456");

        // signing out removes both entries
        forget_sign_in(&v).unwrap();
        assert!(kept(&v).is_none());
        assert!(store.0.lock().unwrap().is_empty());
    }
}
