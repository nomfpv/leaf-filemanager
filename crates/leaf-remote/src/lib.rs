//! leaf-remote: places on other machines behind the `leaf-vfs` interface (PLAN 2.2, W1a). Today: SFTP, read-only.
//!
//! - `SftpFs` serves `sftp://user@host:port/path` over `russh` + `russh-sftp` (pure Rust).
//! - One connection per server, shared by every pane; it is opened again when it broke, and closed after a while unused.
//! - The async runtime lives in this crate (one worker thread) and starts with the first connection, never at app start.
//! - Host keys are checked against `~/.ssh/known_hosts`; an unknown key is asked about, a changed or revoked one refused.
//! - Login: the running agent, the key files, then a password; secrets stay in memory for this run of leaf only.
//!
//! The app calls [`register`] once at start (cheap: nothing connects, no thread starts) with its dialogs as the [`Prompter`].

mod conn;
pub mod known_hosts;
mod prompt;
mod sftp;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, OnceLock};

pub use conn::Config;
pub use leaf_vfs::{FsError, VPath};
pub use prompt::{HostKeyAnswer, HostKeyQuestion, NoPrompter, Prompter, Secret, SecretQuestion};
pub use sftp::SftpFs;

fn setup() -> &'static Mutex<Option<(Config, Arc<dyn Prompter>)>> {
    static S: OnceLock<Mutex<Option<(Config, Arc<dyn Prompter>)>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

fn the_sftp() -> &'static Mutex<Option<Arc<SftpFs>>> {
    static F: OnceLock<Mutex<Option<Arc<SftpFs>>>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(None))
}

/// The one `SftpFs` the registry hands out (made on first use).
fn sftp_backend() -> Arc<SftpFs> {
    let mut f = the_sftp().lock().unwrap_or_else(|e| e.into_inner());
    f.get_or_insert_with(|| {
        let (cfg, p) = setup().lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| (Config::for_user(None), Arc::new(NoPrompter)));
        Arc::new(SftpFs::new(cfg, p))
    })
    .clone()
}

/// Serves `sftp://` from now on. Nothing connects here; the backend is made on its first use.
pub fn register(cfg: Config, prompter: Arc<dyn Prompter>) {
    *setup().lock().unwrap_or_else(|e| e.into_inner()) = Some((cfg, prompter));
    leaf_vfs::register("sftp", || sftp_backend() as Arc<dyn leaf_vfs::Fs>);
}

/// The home folder the server logs the user into (for an address that names only the server).
pub fn home(v: &VPath) -> Result<VPath, FsError> {
    match v.scheme() {
        "sftp" => sftp_backend().home(v),
        _ => Ok(v.root()),
    }
}
