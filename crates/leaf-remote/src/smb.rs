//! Logging in to a Windows share (SMB, W1d): which user name and password to try next, and keeping the one that worked.
//! The connecting itself is the system's (Windows: `WNetAddConnection2W` on a `\\host\share` path; Linux: `gio mount`); this
//! file only decides what is tried, with the same keyring and dialog as SFTP and FTP.
//!
//! - A password saved for the share (scheme `smb`, user, host and share in the keyring) is tried first, once, without asking.
//!   Refused: it is deleted from the keyring (as [`Vault::refused`] does for SFTP) and the dialog asks.
//! - The dialog asks for the user name and the password, with "Remember". A refused one asks again ("Wrong password"); it
//!   never tries again by itself, and Cancel ends it.
//! - What worked is saved only when "Remember" was ticked. The secrets are [`Secret`]s: wiped when dropped, never printed.

use std::sync::Arc;

use crate::prompt::{self, NoPrompter, Prompter, Secret, SecretQuestion};
use crate::secrets::{SecretId, Vault};
use crate::FsError;

/// How one try of a user name and password went.
pub enum Tried {
    Worked,
    /// The server said the name or the password is wrong: a saved one is deleted, the dialog asks again.
    Wrong,
    /// The server refused the login for another reason (no rights to the share, logon hours): the dialog asks again, but a
    /// saved password stays in the keyring (it may be right).
    Denied,
    /// Something else went wrong (the server is not there, ...): no more tries.
    Failed(FsError),
}

/// The logins of one share, tried in turn.
pub struct Login {
    host: String,
    share: String,
    /// The user name to fill in (the address's, or the one typed last).
    hint: Option<String>,
    vault: Vault,
    prompter: Arc<dyn Prompter>,
    saved_tried: bool,
    /// The last one handed out, and whether it came from the dialog (only those are saved).
    last: Option<(SecretId, Secret, bool)>,
}

impl Login {
    /// A login to `\\host\share` with the app's keyring and dialogs. `share` `IPC$` is the server itself (its list of shares).
    pub fn new(host: &str, share: &str, user_hint: Option<&str>) -> Login {
        let (vault, prompter) = match crate::setup().lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some((cfg, p)) => (cfg.vault.clone(), p.clone()),
            None => (Vault::none(), Arc::new(NoPrompter) as Arc<dyn Prompter>),
        };
        Login::with(host, share, user_hint, vault, prompter)
    }

    pub(crate) fn with(host: &str, share: &str, user_hint: Option<&str>, vault: Vault, prompter: Arc<dyn Prompter>) -> Login {
        Login {
            host: host.to_ascii_lowercase(),
            share: share.to_ascii_lowercase(),
            hint: user_hint.filter(|u| !u.is_empty()).map(str::to_string),
            vault,
            prompter,
            saved_tried: false,
            last: None,
        }
    }

    /// `\\host\share`, as the dialog names it.
    fn place(&self) -> String {
        if self.share == "ipc$" { format!(r"\\{}", self.host) } else { format!(r"\\{}\{}", self.host, self.share) }
    }

    /// The next user name and password to try. `wrong`: the one given last was refused; `bad`: as a wrong password (a saved
    /// one is deleted from the keyring then). The saved one comes first, once, without asking; then the dialog. `None`: the
    /// user cancelled. The secret of a saved one is marked "Remember" (it was ticked when it was saved).
    pub fn next(&mut self, wrong: bool, bad: bool) -> Option<(String, Secret)> {
        if let Some((id, s, _)) = self.last.take()
            && wrong
            && bad
        {
            self.vault.refused(&id, &s);
        }
        if !self.saved_tried {
            self.saved_tried = true;
            if let Some((id, s)) = self.saved() {
                let SecretId::Share { user, .. } = &id else { return None };
                let user = user.clone();
                let s = s.remembered(true);
                self.last = Some((id, s.clone(), false));
                return Some((user, s));
            }
        }
        let q = SecretQuestion::Login { place: self.place(), user: self.hint.clone().unwrap_or_default(), retry: wrong, keyring: self.vault.state() };
        let s = prompt::ask_secret_blocking(&*self.prompter, q)?;
        let user = s.user().unwrap_or_default().trim().to_string();
        if !user.is_empty() {
            self.hint = Some(user.clone());
        }
        self.last = Some((SecretId::share(&user, &self.host, &self.share), s.clone(), true));
        Some((user, s))
    }

    /// The last one worked: kept in the keyring when "Remember" was ticked in the dialog (a saved one is there already).
    pub fn worked(&mut self) {
        if let Some((id, s, typed)) = self.last.take()
            && typed
            && matches!(&id, SecretId::Share { user, .. } if !user.is_empty())
        {
            self.vault.keep_if_asked(&id, &s);
        }
    }

    /// Tries with `connect` until one works, the user cancels, or something other than a wrong password goes wrong.
    /// Blocks (the dialog waits for the user): a worker thread only.
    pub fn log_in(&mut self, connect: &mut dyn FnMut(&str, &Secret) -> Tried) -> Result<(), FsError> {
        let (mut wrong, mut bad) = (false, false);
        loop {
            let (user, secret) = self.next(wrong, bad).ok_or(FsError::Cancelled)?;
            match connect(&user, &secret) {
                Tried::Worked => {
                    self.worked();
                    return Ok(());
                }
                Tried::Wrong => (wrong, bad) = (true, true),
                Tried::Denied => (wrong, bad) = (true, false),
                Tried::Failed(e) => return Err(e),
            }
        }
    }

    /// The saved password of this share: the hinted user's, else any user's.
    fn saved(&self) -> Option<(SecretId, Secret)> {
        if let Some(u) = &self.hint {
            let id = SecretId::share(u, &self.host, &self.share);
            if let Some(s) = self.vault.get(&id) {
                return Some((id, s));
            }
        }
        let ids = self.vault.list().ok()?;
        ids.into_iter()
            .filter(|id| matches!(id, SecretId::Share { host, share, .. } if *host == self.host && *share == self.share))
            .find_map(|id| self.vault.get(&id).map(|s| (id, s)))
    }
}

// ------------------------------------------------------------------------------------------------ the system's side

/// `smb://[user@]host/share/dir` as the Windows path `\\host\share\dir` (`\\host` for an address without a share: its list of
/// shares). `None` for text that is no `smb://` address. Reads the text only. (The address's user name is not part of a UNC
/// path; the login dialog asks for it.)
pub fn unc_of(text: &str) -> Option<std::path::PathBuf> {
    let t = text.trim().trim_matches('"');
    if !t.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("smb://")) {
        return None;
    }
    let v = crate::VPath::parse(t).ok()?;
    if v.host().is_empty() {
        return None;
    }
    let rest = v.path().trim_matches('/').replace('/', "\\");
    Some(if rest.is_empty() { format!(r"\\{}", v.host()) } else { format!(r"\\{}\{rest}", v.host()) }.into())
}

/// The server and share of a UNC path (`\\host\share\...`; `\\host` alone is the share `IPC$`, the server's list of shares).
pub fn unc_share(path: &std::path::Path) -> Option<(String, String)> {
    let s = path.to_str()?;
    let rest = s.strip_prefix(r"\\")?;
    if rest.starts_with(['?', '.']) {
        return None;
    }
    let mut parts = rest.split('\\').filter(|p| !p.is_empty());
    let host = parts.next()?.to_string();
    let share = parts.next().unwrap_or("IPC$").to_string();
    Some((host, share))
}

/// Whether a failed listing of `path` (a UNC path on Windows) is the share asking for a login: a logon failure or a wrong
/// password always; access denied only on the share itself (or the server's list of shares), since a folder of a share the
/// user is logged in to may be closed to them all the same.
pub fn wants_login(path: &std::path::Path, err: &std::io::Error) -> bool {
    let Some((_, share)) = unc_share(path) else { return false };
    if !cfg!(windows) || !leaf_sys::network::is_login_refused(err) {
        return false;
    }
    if err.raw_os_error() != Some(5) {
        return true;
    }
    // (access denied: only where the share itself is closed, its top folder)
    let depth = path.to_str().map_or(0, |s| s.trim_start_matches('\\').split('\\').filter(|p| !p.is_empty()).count());
    depth <= 2 || share.eq_ignore_ascii_case("ipc$")
}

/// Logs in to the share of a UNC path (Windows): the saved password first, else leaf's dialog, then `WNetAddConnection2W` with
/// it for `\\host\share` (no drive letter). Blocks (the dialog waits): a worker thread only. `Cancelled` when the user
/// cancelled; another error in leaf's words, the system's text after the colon.
pub fn log_in_unc(path: &std::path::Path) -> Result<(), FsError> {
    let (host, share) = unc_share(path).ok_or(FsError::Unsupported)?;
    let remote = format!(r"\\{host}\{share}");
    Login::new(&host, &share, None).log_in(&mut |user, secret| match leaf_sys::network::connect_share(&remote, user, secret.as_str(), secret.wants_remember()) {
        Ok(()) => Tried::Worked,
        Err(e) if leaf_sys::network::is_bad_password(&e) => Tried::Wrong,
        Err(e) if leaf_sys::network::is_login_refused(&e) => Tried::Denied,
        // ERROR_SESSION_CREDENTIAL_CONFLICT: Windows is logged in to this server as another user already
        Err(e) if e.raw_os_error() == Some(1219) => {
            Tried::Failed(FsError::Auth(
                "Windows is logged in to this server as another user already (a mapped drive or Explorer). Open it by its IP address or its full name instead: Windows gives that a session of its own. Or end the other session first (Disconnect network drive in Explorer)".into(),
            ))
        }
        Err(e) => Tried::Failed(FsError::Offline(e.to_string())),
    })
}

/// Linux: mounts `smb://host/share` through `gio mount` with the saved password, else leaf's dialog (the user name may be
/// `DOMAIN\name`), answering gio's questions on its input. The gvfs folder of `uri`.
#[cfg(unix)]
pub(crate) fn mount_with_login(v: &crate::VPath, uri: &str) -> Result<std::path::PathBuf, FsError> {
    let share = v.path().trim_matches('/').split('/').next().filter(|s| !s.is_empty()).unwrap_or("IPC$").to_string();
    let mut found = None;
    Login::new(v.host(), &share, v.user()).log_in(&mut |user, secret| {
        let (domain, name) = user.split_once('\\').unwrap_or(("", user));
        match leaf_sys::network::mount_path_login(uri, name, domain, secret.as_str()) {
            Ok(p) => {
                found = Some(p);
                Tried::Worked
            }
            Err(e) if leaf_sys::network::is_bad_password(&e) => Tried::Wrong,
            Err(e) if leaf_sys::network::is_login_refused(&e) => Tried::Denied,
            Err(e) => Tried::Failed(FsError::Offline(e.to_string())),
        }
    })?;
    found.ok_or(FsError::Cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;
    use std::sync::Mutex;

    /// The user at the dialog: answers in turn (user, password, Remember), and notes what was asked.
    #[derive(Default)]
    struct User {
        answers: Mutex<Vec<Option<(&'static str, &'static str, bool)>>>,
        asked: Mutex<Vec<String>>,
    }

    impl Prompter for User {
        fn host_key(&self, _: crate::HostKeyQuestion, reply: Box<dyn FnOnce(crate::HostKeyAnswer) + Send>) {
            reply(crate::HostKeyAnswer::Cancel)
        }
        fn secret(&self, q: SecretQuestion, reply: Box<dyn FnOnce(Option<Secret>) + Send>) {
            let SecretQuestion::Login { place, retry, .. } = q else { panic!("not a login: {q:?}") };
            self.asked.lock().unwrap().push(format!("{place} retry={retry}"));
            let a = self.answers.lock().unwrap().remove(0);
            reply(a.map(|(u, p, r)| Secret::new(p.into()).with_user(u.into()).remembered(r)))
        }
    }

    /// The password saved for a share is found again under its id (scheme smb, user, host, share) and used without asking;
    /// refused, it is deleted from the keyring and the dialog asks; a typed one with Remember is saved after it worked.
    #[test]
    fn smb_saved_password_round_trip_and_refused_one_deleted() {
        let store = Arc::new(MemoryStore::default());
        let vault = Vault::new(store.clone());
        let user = Arc::new(User::default());

        // 1. typed with Remember, refused once (asked again), then right: saved under smb + user + host + share
        *user.answers.lock().unwrap() = vec![Some(("alice", "wrong", true)), Some(("alice", "s3cret", true))];
        let mut l = Login::with("NAS", "Photos", None, vault.clone(), user.clone());
        let mut tries = Vec::new();
        l.log_in(&mut |u, s| {
            tries.push(format!("{u}:{}", s.as_str()));
            if s.as_str() == "s3cret" { Tried::Worked } else { Tried::Wrong }
        })
        .unwrap();
        assert_eq!(tries, ["alice:wrong", "alice:s3cret"]);
        assert_eq!(*user.asked.lock().unwrap(), [r"\\nas\photos retry=false", r"\\nas\photos retry=true"]);
        let id = SecretId::share("alice", "nas", "photos");
        assert_eq!(store.saved.lock().unwrap().get(&id).map(String::as_str), Some("s3cret"), "the one that worked is saved");
        assert_eq!(store.saved.lock().unwrap().len(), 1, "the refused one is not");
        assert_eq!(vault.list().unwrap(), [id.clone()]);

        // 2. next time: used silently (no dialog)
        user.asked.lock().unwrap().clear();
        let mut l = Login::with("nas", "photos", None, vault.clone(), user.clone());
        l.log_in(&mut |u, s| if u == "alice" && s.as_str() == "s3cret" { Tried::Worked } else { Tried::Wrong }).unwrap();
        assert!(user.asked.lock().unwrap().is_empty());

        // 2b. the saved try fails for another reason (the share renamed, the server gone mid-mount): it stays in the keyring
        let mut l = Login::with("nas", "photos", None, vault.clone(), user.clone());
        let r = l.log_in(&mut |_, _| Tried::Failed(FsError::Offline("timed out".into())));
        assert!(matches!(r, Err(FsError::Offline(_))), "{r:?}");
        assert!(user.asked.lock().unwrap().is_empty(), "no dialog after a failure that is no refusal");
        assert_eq!(store.saved.lock().unwrap().get(&id).map(String::as_str), Some("s3cret"), "a failure that is no refusal never deletes it");
        // ... and a refusal that is no wrong password (no rights to the share, logon hours): asked again, kept
        *user.answers.lock().unwrap() = vec![None];
        let mut l = Login::with("nas", "photos", None, vault.clone(), user.clone());
        assert!(matches!(l.log_in(&mut |_, _| Tried::Denied), Err(FsError::Cancelled)));
        assert_eq!(*user.asked.lock().unwrap(), [r"\\nas\photos retry=true"]);
        assert_eq!(store.saved.lock().unwrap().get(&id).map(String::as_str), Some("s3cret"), "a denied login keeps the saved password");
        user.asked.lock().unwrap().clear();

        // 3. the server refuses the saved one now: deleted from the keyring, then the dialog asks; Cancel ends it (no loop)
        *user.answers.lock().unwrap() = vec![None];
        let mut l = Login::with("nas", "photos", None, vault.clone(), user.clone());
        let mut n = 0;
        let r = l.log_in(&mut |_, _| {
            n += 1;
            Tried::Wrong
        });
        assert!(matches!(r, Err(FsError::Cancelled)), "{r:?}");
        assert_eq!(n, 1, "tried once, then asked");
        assert_eq!(*user.asked.lock().unwrap(), [r"\\nas\photos retry=true"]);
        assert!(store.saved.lock().unwrap().is_empty(), "the refused saved password is deleted");

        // 4. a typed one without Remember is never saved
        *user.answers.lock().unwrap() = vec![Some((r"WORK\bob", "pw", false))];
        let mut l = Login::with("nas", "ipc$", None, vault.clone(), user.clone());
        l.log_in(&mut |_, _| Tried::Worked).unwrap();
        assert!(store.saved.lock().unwrap().is_empty());
    }
}
