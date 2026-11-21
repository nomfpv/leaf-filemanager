//! Saved passwords and key passphrases (W1b): one seam, [`SecretStore`], with the system's keyring behind it ([`Keyring`]:
//! the Windows Credential Manager, the Secret Service on Linux). Nothing else in leaf names the keyring library.
//!
//! - A secret is saved only when the user ticked "Remember" in the dialog, and only after the server (or the key file)
//!   took it. A saved one is used without asking; when the server refuses it, it is deleted from the keyring and asked again.
//! - What is saved, by [`SecretId`]: a password by scheme, user, server and port; a passphrase by the SHA-256 of its key
//!   file's path (the path itself is not written; the file's name is, for the list).
//! - The list of what is saved (for Settings > Hosts) is read from the keyring itself (a search for leaf's entries, the
//!   names parsed back into ids), so it cannot drift from what is stored and has no size cap: nothing about saved secrets
//!   is ever in a file of leaf's (so the settings export cannot carry them).
//! - No keyring (no Secret Service running): [`SecretStore::available`] says why, the dialogs show "Remember" disabled with
//!   that line, and nothing is saved anywhere else.
//! - The keyring library's own error text goes to the log's details only (it never carries a secret); the user reads
//!   leaf's sentence.

use std::sync::{Arc, OnceLock};
#[cfg(test)]
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::prompt::{Keyring as KeyringState, Secret};

/// What a saved secret is for. Its `Debug` and [`SecretId::label`] carry names only, never a secret.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SecretId {
    /// The password of `user` on `host:port` over `scheme` (`sftp`, `ftp`, `ftps`: a password saved for `ftps` is never
    /// sent over plain `ftp`).
    Password { scheme: String, user: String, host: String, port: u16 },
    /// The passphrase of a key file: the SHA-256 of its path, and the file's name (for the list).
    Passphrase { fingerprint: String, file_name: String },
    /// The password of `user` (`name`, `DOMAIN\name` or `name@domain`) for the Windows share `share` on `host` (SMB; the
    /// share `IPC$` stands for the server itself, its list of shares).
    Share { user: String, host: String, share: String },
}

impl SecretId {
    pub fn password(scheme: &str, user: &str, host: &str, port: u16) -> SecretId {
        SecretId::Password { scheme: scheme.to_ascii_lowercase(), user: user.to_string(), host: host.to_ascii_lowercase(), port }
    }

    pub fn share(user: &str, host: &str, share: &str) -> SecretId {
        SecretId::Share { user: user.to_string(), host: host.to_ascii_lowercase(), share: share.to_ascii_lowercase() }
    }

    pub fn passphrase(key_file: &std::path::Path) -> SecretId {
        let digest = sha2::Sha256::digest(key_file.to_string_lossy().as_bytes());
        let fingerprint: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let file_name = key_file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        SecretId::Passphrase { fingerprint, file_name }
    }

    /// The name of the keyring entry (`sftp://user@host:22`, `smb://user@host/share`, `key:<sha-256>:<file name>`); [`SecretId::from_account`]
    /// reads it back.
    fn account(&self) -> String {
        match self {
            SecretId::Password { scheme, user, host, port } => format!("{scheme}://{user}@{host}:{port}"),
            SecretId::Passphrase { fingerprint, file_name } => format!("key:{fingerprint}:{file_name}"),
            SecretId::Share { user, host, share } => format!("smb://{user}@{host}/{share}"),
        }
    }

    /// The id of a keyring entry's name; `None` for a name leaf did not make.
    fn from_account(account: &str) -> Option<SecretId> {
        if let Some(rest) = account.strip_prefix("key:") {
            let (fingerprint, file_name) = rest.split_once(':')?;
            let hex = fingerprint.len() == 64 && fingerprint.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            return hex.then(|| SecretId::Passphrase { fingerprint: fingerprint.to_string(), file_name: file_name.to_string() });
        }
        if let Some(rest) = account.strip_prefix("smb://") {
            let (server, share) = rest.rsplit_once('/')?;
            let (user, host) = server.rsplit_once('@')?;
            let ok = !user.is_empty() && !host.is_empty() && !share.is_empty() && !host.contains(['/', '\\']);
            return Some(SecretId::share(user, host, share)).filter(|id| ok && id.account() == account);
        }
        let (scheme, rest) = account.split_once("://")?;
        if !matches!(scheme, "sftp" | "ftp" | "ftps") {
            return None;
        }
        let (user, server) = rest.rsplit_once('@')?;
        let (host, port) = server.rsplit_once(':')?;
        let port: u16 = port.parse().ok()?;
        Some(SecretId::password(scheme, user, host, port)).filter(|id| !host.is_empty() && id.account() == account)
    }

    /// What the list of saved passwords shows: `user@host:port (SFTP)`, or the key file's name.
    pub fn label(&self) -> String {
        match self {
            SecretId::Password { scheme, user, host, port } => format!("{user}@{host}:{port} ({})", scheme.to_ascii_uppercase()),
            SecretId::Passphrase { file_name, .. } => format!("Key file {file_name}"),
            SecretId::Share { user, host, share } if share == "ipc$" => format!("{user}@{host} (SMB)"),
            SecretId::Share { user, host, share } => format!("{user}@{host}/{share} (SMB)"),
        }
    }

    /// Whether this is a password for a host of `scheme` (`ftp` and `ftps` count as one) on `host`, at `port` when given
    /// (any port otherwise), of `user` when given.
    pub fn is_for(&self, scheme: &str, host: &str, port: Option<u16>, user: Option<&str>) -> bool {
        let family = |s: &str| if s.eq_ignore_ascii_case("ftps") { "ftp".to_string() } else { s.to_ascii_lowercase() };
        match self {
            SecretId::Password { scheme: s, user: u, host: h, port: p } => {
                family(s) == family(scheme) && h.eq_ignore_ascii_case(host) && port.is_none_or(|w| w == *p) && user.is_none_or(|w| w == u)
            }
            // (SMB has no port of its own in leaf: every share of the server counts)
            SecretId::Share { user: u, host: h, .. } => scheme.eq_ignore_ascii_case("smb") && h.eq_ignore_ascii_case(host) && user.is_none_or(|w| w == u),
            SecretId::Passphrase { .. } => false,
        }
    }
}

/// Where saved secrets live. Errors are leaf's sentences.
pub trait SecretStore: Send + Sync {
    /// `Err(why)` when secrets cannot be saved here (no keyring running).
    fn available(&self) -> Result<(), String>;
    /// `None`: nothing saved for it.
    fn get(&self, id: &SecretId) -> Result<Option<Secret>, String>;
    fn set(&self, id: &SecretId, secret: &Secret) -> Result<(), String>;
    /// Deleting what is not there is fine.
    fn delete(&self, id: &SecretId) -> Result<(), String>;
    /// What is saved (ids only).
    fn list(&self) -> Result<Vec<SecretId>, String>;
}

/// The store a connection uses: none (nothing is saved, "Remember" is not offered), or one store.
#[derive(Clone, Default)]
pub struct Vault(Option<Arc<dyn SecretStore>>);

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "Vault(store)" } else { "Vault(none)" })
    }
}

const NOT_SAVED: &str = "This copy of leaf does not save passwords.";

static CHANGES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Goes up whenever leaf saves or deletes a secret (a list on screen reads the keyring again).
pub fn changes() -> u64 {
    CHANGES.load(std::sync::atomic::Ordering::Acquire)
}

fn changed() {
    CHANGES.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

impl Vault {
    pub fn none() -> Vault {
        Vault(None)
    }
    pub fn new(store: Arc<dyn SecretStore>) -> Vault {
        Vault(Some(store))
    }
    /// The system's keyring (opened on first use, not here).
    pub fn system() -> Vault {
        Vault::new(Arc::new(Keyring::default()))
    }

    /// What the dialog offers: "Remember", or why not.
    pub fn state(&self) -> KeyringState {
        match self.0.as_ref().map(|s| s.available()) {
            Some(Ok(())) => KeyringState::Ready,
            Some(Err(why)) => KeyringState::Missing(why),
            None => KeyringState::Missing(NOT_SAVED.into()),
        }
    }

    /// The saved secret, if there is one (a keyring that fails counts as none; logged without the secret).
    pub fn get(&self, id: &SecretId) -> Option<Secret> {
        let s = self.0.as_ref()?;
        if s.available().is_err() {
            return None;
        }
        match s.get(id) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("remote: the saved password of {} could not be read: {e}", id.label());
                None
            }
        }
    }

    /// Keeps `secret` when the user asked for it ("Remember" ticked); a failure is logged, the login goes on.
    pub fn keep_if_asked(&self, id: &SecretId, secret: &Secret) {
        if !secret.wants_remember() {
            return;
        }
        let Some(s) = &self.0 else { return };
        let r = s.set(id, secret);
        changed();
        match r {
            Ok(()) => log::info!("remote: saved the password of {} in the keyring", id.label()),
            Err(e) => log::warn!("remote: the password of {} could not be saved: {e}", id.label()),
        }
    }

    /// `secret` was refused (by the server, or it did not open the key file): when it is the saved one, it is deleted from
    /// the keyring, so the next try asks. (A wrong one just typed leaves a different saved one alone.)
    pub fn refused(&self, id: &SecretId, secret: &Secret) {
        if self.get(id).is_some_and(|saved| saved.as_str() == secret.as_str()) {
            log::info!("remote: the saved password of {} was refused; it is deleted from the keyring", id.label());
            if let Err(e) = self.forget(id) {
                log::warn!("remote: {e}");
            }
        }
    }

    /// Forgets a saved secret (the user said "Forget").
    pub fn forget(&self, id: &SecretId) -> Result<(), String> {
        let Some(s) = &self.0 else { return Ok(()) };
        let r = s.delete(id);
        changed();
        r
    }

    pub fn list(&self) -> Result<Vec<SecretId>, String> {
        match &self.0 {
            Some(s) => {
                s.available()?;
                s.list()
            }
            None => Ok(Vec::new()),
        }
    }
}

/// The same calls for code on leaf-remote's tokio runtime: each runs on the runtime's blocking threads, because a keyring
/// can wait for the user (the Secret Service's unlock prompt on a locked collection) and the runtime's worker drives every
/// SFTP connection and timeout meanwhile.
impl Vault {
    async fn off_runtime<T: Send + 'static>(&self, f: impl FnOnce(Vault) -> T + Send + 'static) -> Option<T> {
        let v = self.clone();
        tokio::task::spawn_blocking(move || f(v)).await.ok()
    }
    pub(crate) async fn get_async(&self, id: &SecretId) -> Option<Secret> {
        let id = id.clone();
        self.off_runtime(move |v| v.get(&id)).await.flatten()
    }
    pub(crate) async fn state_async(&self) -> KeyringState {
        self.off_runtime(|v| v.state()).await.unwrap_or_else(|| KeyringState::Missing(NOT_SAVED.into()))
    }
    pub(crate) async fn keep_if_asked_async(&self, id: &SecretId, secret: &Secret) {
        if !secret.wants_remember() || self.0.is_none() {
            return;
        }
        let (id, secret) = (id.clone(), secret.clone());
        self.off_runtime(move |v| v.keep_if_asked(&id, &secret)).await;
    }
    pub(crate) async fn refused_async(&self, id: &SecretId, secret: &Secret) {
        if self.0.is_none() {
            return;
        }
        let (id, secret) = (id.clone(), secret.clone());
        self.off_runtime(move |v| v.refused(&id, &secret)).await;
    }
}

// ------------------------------------------------------------------------------------------------ the system's keyring

/// The service name every entry of leaf's has in the keyring.
const SERVICE: &str = "leaf";
/// The entry an earlier build kept the list in (ids only); deleted when the list is read.
const OLD_INDEX: &str = "leaf-saved-list";

/// The system's keyring through `keyring-core` and the platform's store. Opened on first use (on Linux that is a D-Bus
/// connection), never at app start.
#[derive(Default)]
pub struct Keyring {
    store: OnceLock<Result<Arc<keyring_core::CredentialStore>, String>>,
}

/// leaf's sentence for a keyring failure; the library's text goes into the details after it.
fn failed(what: &str, e: keyring_core::Error) -> String {
    use keyring_core::Error as E;
    let why = match &e {
        E::NoStorageAccess(_) => "the keyring is locked or refused access",
        E::PlatformFailure(_) => "the keyring did not answer",
        E::TooLong(..) | E::Invalid(..) => "the name is too long for the keyring",
        E::NotSupportedByStore(_) => "this keyring cannot list its entries",
        _ => "the keyring gave an error",
    };
    // (`BadEncoding` and `BadDataFormat` hold the stored bytes: never printed)
    let details = match &e {
        E::NoStorageAccess(p) | E::PlatformFailure(p) => format!(" ({p})"),
        _ => String::new(),
    };
    format!("{what}: {why}{details}")
}

fn open_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
    #[cfg(windows)]
    {
        windows_native_keyring_store::Store::new().map(|s| s as Arc<keyring_core::CredentialStore>).map_err(|e| failed("The Windows Credential Manager cannot be used", e))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        zbus_secret_service_keyring_store::Store::new()
            .map(|s| s as Arc<keyring_core::CredentialStore>)
            .map_err(|e| {
                log::info!("remote: no Secret Service: {}", failed("keyring", e));
                "No keyring is running on this system (Secret Service), so leaf cannot remember passwords.".to_string()
            })
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    {
        Err("leaf cannot remember passwords on this system yet.".to_string())
    }
}

/// What finds leaf's entries in the store: the Credential Manager matches a pattern on the target name
/// (`<account>.<service>`), the Secret Service the `service` attribute. Each found entry's service is checked again.
fn search_spec() -> std::collections::HashMap<&'static str, &'static str> {
    #[cfg(windows)]
    {
        std::collections::HashMap::from([("pattern", r"\.leaf$")])
    }
    #[cfg(not(windows))]
    {
        std::collections::HashMap::from([("service", SERVICE)])
    }
}

impl Keyring {
    fn store(&self) -> Result<&Arc<keyring_core::CredentialStore>, String> {
        self.store.get_or_init(open_store).as_ref().map_err(Clone::clone)
    }

    fn entry(&self, account: &str) -> Result<keyring_core::Entry, String> {
        self.store()?.build(SERVICE, account, None).map_err(|e| failed("The keyring entry cannot be made", e))
    }
}

impl SecretStore for Keyring {
    fn available(&self) -> Result<(), String> {
        self.store().map(|_| ())
    }

    fn get(&self, id: &SecretId) -> Result<Option<Secret>, String> {
        match self.entry(&id.account())?.get_password() {
            Ok(p) => Ok(Some(Secret::new(p))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(keyring_core::Error::BadEncoding(_)) => Ok(None),
            Err(e) => Err(failed("The saved password cannot be read", e)),
        }
    }

    fn set(&self, id: &SecretId, secret: &Secret) -> Result<(), String> {
        self.entry(&id.account())?.set_password(secret.as_str()).map_err(|e| failed("The password cannot be saved", e))
    }

    fn delete(&self, id: &SecretId) -> Result<(), String> {
        match self.entry(&id.account())?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(e) => Err(failed("The saved password cannot be deleted", e)),
        }
    }

    /// leaf's entries as the store has them now (an entry whose name leaf cannot read back is left out, never deleted).
    fn list(&self) -> Result<Vec<SecretId>, String> {
        let found = self.store()?.search(&search_spec()).map_err(|e| failed("The list of saved passwords cannot be read", e))?;
        let mut ids = Vec::new();
        for entry in found {
            let Some((service, account)) = entry.get_specifiers() else { continue };
            if service != SERVICE {
                continue;
            }
            if account == OLD_INDEX {
                let _ = entry.delete_credential();
                continue;
            }
            match SecretId::from_account(&account) {
                Some(id) if !ids.contains(&id) => ids.push(id),
                Some(_) => {}
                None => log::debug!("remote: a keyring entry of service {SERVICE} that leaf did not make is left alone"),
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
#[test]
fn saved_ids_read_back_from_their_keyring_names() {
    let ids = [
        SecretId::password("sftp", "alice", "example.org", 22),
        SecretId::password("ftps", "a@b.c", "192.0.2.1", 990),
        SecretId::password("sftp", "bob", "[::1]", 2222),
        SecretId::passphrase(std::path::Path::new("C:/Users/x/.ssh/id_ed25519")),
        SecretId::passphrase(std::path::Path::new("/home/x/keys/a:b.key")),
        SecretId::share("alice", "nas", "photos"),
        SecretId::share("WORK\\bob", "files.example.org", "ipc$"),
        SecretId::share("bob@work.example", "192.0.2.7", "my share"),
    ];
    for id in &ids {
        assert_eq!(SecretId::from_account(&id.account()).as_ref(), Some(id), "{}", id.account());
    }
    // names that are not leaf's ids are not read as one (so never listed, never deleted by "Forget all")
    for other in ["leaf-saved-list", "key:abc:x", "http://a@b:1", "sftp://a@b:99999", "sftp://a@B:22", "sftp://nobody", "sftp://a@:22", "smb://a@b", "smb://@b/c", "smb://a@b/", "smb://a@B/c", ""] {
        assert_eq!(SecretId::from_account(other), None, "{other}");
    }
}

// ------------------------------------------------------------------------------------------------ tests: in memory

/// A keyring in memory for the tests (the real one is never touched by a test).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct MemoryStore {
    pub saved: Mutex<std::collections::HashMap<SecretId, String>>,
}

#[cfg(test)]
impl SecretStore for MemoryStore {
    fn available(&self) -> Result<(), String> {
        Ok(())
    }
    fn get(&self, id: &SecretId) -> Result<Option<Secret>, String> {
        Ok(self.saved.lock().unwrap().get(id).cloned().map(Secret::new))
    }
    fn set(&self, id: &SecretId, secret: &Secret) -> Result<(), String> {
        self.saved.lock().unwrap().insert(id.clone(), secret.as_str().to_string());
        Ok(())
    }
    fn delete(&self, id: &SecretId) -> Result<(), String> {
        self.saved.lock().unwrap().remove(id);
        Ok(())
    }
    fn list(&self) -> Result<Vec<SecretId>, String> {
        Ok(self.saved.lock().unwrap().keys().cloned().collect())
    }
}
