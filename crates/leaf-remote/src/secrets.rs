//! Saved passwords and key passphrases (W1b): one seam, [`SecretStore`], with the system's keyring behind it ([`Keyring`]:
//! the Windows Credential Manager, the Secret Service on Linux). Nothing else in leaf names the keyring library.
//!
//! - A secret is saved only when the user ticked "Remember" in the dialog, and only after the server (or the key file)
//!   took it. A saved one is used without asking; when the server refuses it, it is deleted from the keyring and asked again.
//! - What is saved, by [`SecretId`]: a password by scheme, user, server and port; a passphrase by the SHA-256 of its key
//!   file's path (the path itself is not written; the file's name is, for the list).
//! - The list of what is saved (for Settings > Hosts) is kept in the keyring too, as one more entry holding only ids:
//!   nothing about saved secrets is ever in a file of leaf's (so the settings export cannot carry them).
//! - No keyring (no Secret Service running): [`SecretStore::available`] says why, the dialogs show "Remember" disabled with
//!   that line, and nothing is saved anywhere else.
//! - The keyring library's own error text goes to the log's details only (it never carries a secret); the user reads
//!   leaf's sentence.

use std::sync::{Arc, Mutex, OnceLock};

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
}

impl SecretId {
    pub fn password(scheme: &str, user: &str, host: &str, port: u16) -> SecretId {
        SecretId::Password { scheme: scheme.to_ascii_lowercase(), user: user.to_string(), host: host.to_ascii_lowercase(), port }
    }

    pub fn passphrase(key_file: &std::path::Path) -> SecretId {
        let digest = sha2::Sha256::digest(key_file.to_string_lossy().as_bytes());
        let fingerprint: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let file_name = key_file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        SecretId::Passphrase { fingerprint, file_name }
    }

    /// The name of the keyring entry (`sftp://user@host:22`, `key:<sha-256>`).
    fn account(&self) -> String {
        match self {
            SecretId::Password { scheme, user, host, port } => format!("{scheme}://{user}@{host}:{port}"),
            SecretId::Passphrase { fingerprint, .. } => format!("key:{fingerprint}"),
        }
    }

    /// What the list of saved passwords shows: `user@host:port (SFTP)`, or the key file's name.
    pub fn label(&self) -> String {
        match self {
            SecretId::Password { scheme, user, host, port } => format!("{user}@{host}:{port} ({})", scheme.to_ascii_uppercase()),
            SecretId::Passphrase { file_name, .. } => format!("Key file {file_name}"),
        }
    }

    /// Whether this is a password for a host of `scheme` (`ftp` and `ftps` count as one) on `host`, at `port` when given
    /// (any port otherwise), of `user` when given.
    pub fn is_for(&self, scheme: &str, host: &str, port: Option<u16>, user: Option<&str>) -> bool {
        let family = |s: &str| if s.eq_ignore_ascii_case("ftps") { "ftp".to_string() } else { s.to_ascii_lowercase() };
        matches!(self, SecretId::Password { scheme: s, user: u, host: h, port: p }
            if family(s) == family(scheme) && h.eq_ignore_ascii_case(host) && port.is_none_or(|w| w == *p) && user.is_none_or(|w| w == u))
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

// ------------------------------------------------------------------------------------------------ the system's keyring

/// The service name every entry of leaf's has in the keyring.
const SERVICE: &str = "leaf";
/// The entry that lists what leaf saved (ids only, as JSON).
const INDEX: &str = "leaf-saved-list";

/// The system's keyring through `keyring-core` and the platform's store. Opened on first use (on Linux that is a D-Bus
/// connection), never at app start.
#[derive(Default)]
pub struct Keyring {
    store: OnceLock<Result<Arc<keyring_core::CredentialStore>, String>>,
    /// One change of the list at a time.
    index: Mutex<()>,
}

/// leaf's sentence for a keyring failure; the library's text goes into the details after it.
fn failed(what: &str, e: keyring_core::Error) -> String {
    use keyring_core::Error as E;
    let why = match &e {
        E::NoStorageAccess(_) => "the keyring is locked or refused access",
        E::PlatformFailure(_) => "the keyring did not answer",
        E::TooLong(..) | E::Invalid(..) => "the name is too long for the keyring",
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

impl Keyring {
    fn store(&self) -> Result<&Arc<keyring_core::CredentialStore>, String> {
        self.store.get_or_init(open_store).as_ref().map_err(Clone::clone)
    }

    fn entry(&self, account: &str) -> Result<keyring_core::Entry, String> {
        self.store()?.build(SERVICE, account, None).map_err(|e| failed("The keyring entry cannot be made", e))
    }

    fn read_index(&self) -> Result<Vec<SecretId>, String> {
        match self.entry(INDEX)?.get_password() {
            Ok(text) => Ok(serde_json::from_str(&text).unwrap_or_else(|_| {
                log::warn!("remote: the keyring's list of saved passwords is not readable; starting a new one");
                Vec::new()
            })),
            Err(keyring_core::Error::NoEntry) => Ok(Vec::new()),
            Err(e) => Err(failed("The list of saved passwords cannot be read", e)),
        }
    }

    fn write_index(&self, ids: &[SecretId]) -> Result<(), String> {
        let e = self.entry(INDEX)?;
        if ids.is_empty() {
            return match e.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
                Err(err) => Err(failed("The list of saved passwords cannot be written", err)),
            };
        }
        let text = serde_json::to_string(ids).map_err(|e| e.to_string())?;
        e.set_password(&text).map_err(|e| failed("The list of saved passwords cannot be written", e))
    }

    fn change_index(&self, change: impl FnOnce(&mut Vec<SecretId>)) -> Result<(), String> {
        let _g = self.index.lock().unwrap_or_else(|e| e.into_inner());
        let mut ids = self.read_index()?;
        let before = ids.clone();
        change(&mut ids);
        if ids != before { self.write_index(&ids) } else { Ok(()) }
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
        self.entry(&id.account())?.set_password(secret.as_str()).map_err(|e| failed("The password cannot be saved", e))?;
        self.change_index(|ids| {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        })
    }

    fn delete(&self, id: &SecretId) -> Result<(), String> {
        match self.entry(&id.account())?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => {}
            Err(e) => return Err(failed("The saved password cannot be deleted", e)),
        }
        self.change_index(|ids| ids.retain(|i| i != id))
    }

    fn list(&self) -> Result<Vec<SecretId>, String> {
        let _g = self.index.lock().unwrap_or_else(|e| e.into_inner());
        self.read_index()
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
