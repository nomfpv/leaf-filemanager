//! leaf-remote: places on other machines behind the `leaf-vfs` interface (PLAN 2.2, W1a, W1c). Today: SFTP and FTP/FTPS (read
//! and write), editing a file on a server (`edit`, H10), Docker and rclone views.
//!
//! - `SftpFs` serves `sftp://user@host:port/path` over `russh` + `russh-sftp` (pure Rust).
//! - `FtpFs` serves `ftp://` and `ftps://` over `suppaftp` (blocking, on the caller's worker thread; see `ftp.rs`).
//! - One connection per server, shared by every pane; it is opened again when it broke, and closed after a while unused.
//! - The async runtime lives in this crate (one worker thread) and starts with the first connection, never at app start.
//! - Host keys are checked against `~/.ssh/known_hosts`; an unknown key is asked about, a changed or revoked one refused.
//! - Login: the running agent, the key files, then a password; secrets stay in memory for this run of leaf, and in the
//!   system's keyring when the user ticked "Remember" (`secrets`).
//!
//! The app calls [`register`] once at start (cheap: nothing connects, no thread starts) with its dialogs as the [`Prompter`].

pub mod checksum;
mod conn;
pub mod docker;
pub mod edit;
#[cfg(unix)]
mod gvfs;
mod ftp;
#[cfg(test)]
mod ftp_tests;
pub mod hostkind;
mod http;
pub mod known_hosts;
mod prompt;
pub mod rclone;
pub mod secrets;
pub mod server_copy;
mod sftp;
pub mod smb;
pub mod trash;
mod webdav;
#[cfg(test)]
mod webdav_tests;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, OnceLock};

pub use conn::{Config, ExecOut};
pub use leaf_vfs::{FsError, VPath};
pub use ftp::{FtpFs, QUIET_LOG_TARGETS, TRUSTED_CERTS_FILE, is_quiet};
pub use prompt::{CertQuestion, HostKeyAnswer, HostKeyQuestion, Keyring, NoPrompter, Prompter, Secret, SecretQuestion};
pub use secrets::{SecretId, SecretStore, Vault};
pub use sftp::SftpFs;
pub use webdav::WebDavFs;

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

fn the_ftp() -> &'static Mutex<Option<Arc<FtpFs>>> {
    static F: OnceLock<Mutex<Option<Arc<FtpFs>>>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(None))
}

/// The one `FtpFs` (both `ftp://` and `ftps://`; the scheme of each address decides the TLS), made on first use.
fn ftp_backend() -> Arc<FtpFs> {
    let mut f = the_ftp().lock().unwrap_or_else(|e| e.into_inner());
    f.get_or_insert_with(|| {
        let (cfg, p) = setup().lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| (Config::for_user(None), Arc::new(NoPrompter)));
        Arc::new(FtpFs::new(cfg, p))
    })
    .clone()
}

fn the_dav() -> &'static Mutex<Option<Arc<WebDavFs>>> {
    static F: OnceLock<Mutex<Option<Arc<WebDavFs>>>> = OnceLock::new();
    F.get_or_init(|| Mutex::new(None))
}

/// The one `WebDavFs` (both `dav://` and `davs://`; the scheme of each address decides the TLS), made on first use.
fn dav_backend() -> Arc<WebDavFs> {
    let mut f = the_dav().lock().unwrap_or_else(|e| e.into_inner());
    f.get_or_insert_with(|| {
        let (cfg, p) = setup().lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| (Config::for_user(None), Arc::new(NoPrompter)));
        Arc::new(WebDavFs::new(cfg, p))
    })
    .clone()
}

/// Serves `sftp://`, `ftp://`, `ftps://`, `dav://` and `davs://` from now on. Nothing connects here; the backend is made on its
/// first use.
pub fn register(cfg: Config, prompter: Arc<dyn Prompter>) {
    *setup().lock().unwrap_or_else(|e| e.into_inner()) = Some((cfg, prompter));
    leaf_vfs::register("sftp", || sftp_backend() as Arc<dyn leaf_vfs::Fs>);
    leaf_vfs::register("ftp", || ftp_backend() as Arc<dyn leaf_vfs::Fs>);
    leaf_vfs::register("ftps", || ftp_backend() as Arc<dyn leaf_vfs::Fs>);
    // WebDAV (X9): `dav://user@host/path` over http, `davs://` over https
    leaf_vfs::register("dav", || dav_backend() as Arc<dyn leaf_vfs::Fs>);
    leaf_vfs::register("davs", || dav_backend() as Arc<dyn leaf_vfs::Fs>);
    // `docker://local/` and `docker://user@host/`: the containers and their mounts, read-only (nothing runs until opened)
    leaf_vfs::register("docker", || Arc::new(docker::DockerFs) as Arc<dyn leaf_vfs::Fs>);
    // `smb://host/share` and `nfs://host/export` on Linux: mounted through gio, read through the gvfs folder
    #[cfg(unix)]
    {
        leaf_vfs::register("smb", || Arc::new(gvfs::GvfsFs::default()) as Arc<dyn leaf_vfs::Fs>);
        leaf_vfs::register("nfs", || Arc::new(gvfs::GvfsFs::default()) as Arc<dyn leaf_vfs::Fs>);
    }
    // `rclone://remote/path`, only when the rclone program is installed (a PATH look-up; nothing runs here)
    if rclone::find_exe().is_some() {
        leaf_vfs::register("rclone", || match rclone::find_exe() {
            Some(exe) => Arc::new(rclone::RcloneFs::new(exe)) as Arc<dyn leaf_vfs::Fs>,
            None => Arc::new(NoRclone),
        });
    }
}

/// Where the app's connections keep the secrets the user asked to remember (none before [`register`]).
pub fn vault() -> Vault {
    setup().lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(c, _)| c.vault.clone()).unwrap_or_default()
}

/// What is saved in the keyring (ids only; the secrets are never read for this).
pub fn saved_secrets() -> Result<Vec<SecretId>, String> {
    vault().list()
}

/// "Forget saved password": deletes it from the keyring and from this run's memory (a password, or the key file a
/// passphrase unlocked), so it is asked next time.
pub fn forget_saved(id: &SecretId) -> Result<(), String> {
    match id {
        SecretId::Password { scheme, host, port, .. } => match scheme.as_str() {
            "sftp" => sftp_backend().forget_passwords(host, *port),
            "ftp" | "ftps" => ftp_backend().forget_passwords(host, *port),
            "dav" | "davs" => dav_backend().forget_passwords(host, *port),
            _ => {}
        },
        SecretId::Passphrase { fingerprint, .. } => sftp_backend().forget_key(fingerprint),
        // (Windows keeps an SMB session itself: the host's Disconnect ends it)
        SecretId::Share { .. } => {}
        SecretId::Web { .. } => {}
        SecretId::Vpn { .. } => {}
    }
    vault().forget(id)
}

/// The runtime leaf's remote connections run on (started here when nothing started it yet): leaf's VPN tunnel runs on it too.
pub fn runtime_handle() -> tokio::runtime::Handle {
    conn::rt().handle().clone()
}

/// The home folder the server logs the user into (for an address that names only the server).
pub fn home(v: &VPath) -> Result<VPath, FsError> {
    match v.scheme() {
        "sftp" => sftp_backend().home(v),
        "ftp" | "ftps" => ftp_backend().home(v),
        // (WebDAV has no login folder: the share's root is the home, and only the root itself is guarded in the trash)
        "dav" | "davs" => Ok(v.root()),
        _ => Ok(v.root()),
    }
}

/// Whether a connection to the server of this `sftp://`, `ftp://` or `ftps://` address is open now (never connects).
pub fn is_connected(v: &VPath) -> bool {
    match v.scheme() {
        "sftp" => sftp_backend().target_connected(v),
        "ftp" | "ftps" => ftp_backend().target_connected(v),
        "dav" | "davs" => dav_backend().target_connected(v),
        _ => false,
    }
}

/// Free bytes on the server, only while it is connected already (never connects; `None` when unknown).
pub fn free_if_connected(v: &VPath) -> Option<u64> {
    if v.scheme() != "sftp" {
        return None;
    }
    sftp_backend().free_if_connected(&v.with_path("/~")).map(|s| s.free)
}

/// The sum of a file on the server, computed there by `sha256sum` or `xxhsum` over the open (or a new) connection; nothing of
/// the file crosses the line. Blocks: call from a worker thread. `Unsupported` for an address that is not `sftp://`.
pub fn remote_sum(v: &VPath, algo: checksum::Algo, cancelled: &dyn Fn() -> bool) -> Result<String, FsError> {
    if v.scheme() != "sftp" {
        return Err(FsError::Unsupported);
    }
    let out = sftp_backend().exec(v, &checksum::command(algo, v.path()), std::time::Duration::from_secs(30 * 60), cancelled)?;
    match checksum::parse_output(algo, &out.stdout) {
        Some(h) if out.code.unwrap_or(0) == 0 => Ok(h),
        _ if out.code == Some(127) => Err(FsError::Other(format!("The server has no program for {}", algo.label()))),
        _ => Err(FsError::Other(format!("The server gave no {} for this file", algo.label()))),
    }
}

/// What kind of machine the `sftp://` server of `v` is (Linux, a BSD, BusyBox, macOS, or unknown), asked once per connection
/// with `uname -s` (5 seconds at most) and kept with it. Never fails: a server that runs no commands, a menu shell or an
/// address that is not `sftp://` is `Unknown`. Blocks: call from a worker thread.
pub fn host_family(v: &VPath) -> hostkind::HostFamily {
    if v.scheme() != "sftp" {
        return hostkind::HostFamily::Unknown;
    }
    sftp_backend().family(v, &|| false)
}

/// Copies `from` (a file or a folder) to `to` on the same `sftp://` server, done by the server itself (`server_copy`): nothing
/// crosses the line. `to` must be free, or (with `replace`, the conflict dialog's answer) a file. `progress` gets the bytes
/// done of this item; while `paused` the server's copy is stopped there. `Unsupported` when the server cannot (another server, no shell, no `cp`; a folder that is merged into):
/// nothing was done, copy as before. Blocks: call from a worker thread.
pub fn copy_on_server(
    from: &VPath,
    to: &VPath,
    replace: bool,
    cancelled: &dyn Fn() -> bool,
    paused: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), FsError> {
    // a WebDAV server copies by itself too (`COPY`; no progress: it is one request)
    if matches!(from.scheme(), "dav" | "davs") && from.scheme() == to.scheme() {
        let r = dav_backend().copy_on_server(from, to, replace);
        if r.is_ok() {
            progress(0);
        }
        return r;
    }
    if from.scheme() != "sftp" || to.scheme() != "sftp" || from.authority() != to.authority() {
        return Err(FsError::Unsupported);
    }
    let fs = sftp_backend();
    // (before anything else: `/~/a` into `/home/u/a/b` is a folder into itself, whichever way it would be copied)
    let home = fs.home(from)?.path().to_string();
    if server_copy::into_itself(from.path(), to.path(), &home) {
        return Err(FsError::Other("a folder cannot be copied into itself".into()));
    }
    let ask = || fs.exec(from, server_copy::PROBE, std::time::Duration::from_secs(8), cancelled).map(|o| (o.stdout, o.code));
    let Some(tool) = server_copy::tool_of(from.authority(), ask)? else { return Err(FsError::Unsupported) };
    let progress = std::cell::RefCell::new(progress);
    server_copy::copy_with(&*fs, tool, &home, from, to, replace, &mut |command, part| {
        let poll = (tool != server_copy::Tool::Rsync).then(|| server_copy::size_command(part.path()));
        let mut on_text = |t: &str| {
            if tool == server_copy::Tool::Rsync
                && let Some(n) = server_copy::parse_rsync_progress(t)
            {
                (progress.borrow_mut())(n);
            }
        };
        let mut on_poll = |o: &ExecOut| {
            if let Some(n) = server_copy::parse_size(&o.stdout) {
                (progress.borrow_mut())(n);
            }
        };
        // (a channel the server refuses: nothing started, the copy goes through this PC)
        let end = match fs.exec_copy(from, command, poll.as_deref(), cancelled, paused, &mut on_text, &mut on_poll) {
            Ok(end) => end,
            Err(FsError::Cancelled) => return Err(FsError::Cancelled),
            Err(e) => {
                log::info!("remote: the server did not run the copy ({e}); copying through this PC");
                return Err(FsError::Unsupported);
            }
        };
        server_copy::outcome(tool, &end, part)
    })
}

/// Stands in when rclone went away after the start.
struct NoRclone;

impl leaf_vfs::Fs for NoRclone {
    fn caps(&self) -> leaf_vfs::Caps {
        leaf_vfs::Caps::default()
    }
    fn list(&self, _: &VPath, _: usize, _: &dyn Fn() -> bool, _: &mut dyn FnMut(Vec<leaf_vfs::Entry>)) -> Result<usize, FsError> {
        Err(FsError::Other("rclone is not installed".into()))
    }
    fn stat(&self, _: &VPath) -> Result<leaf_vfs::Meta, FsError> {
        Err(FsError::Unsupported)
    }
    fn open_read(&self, _: &VPath, _: u64) -> Result<Box<dyn std::io::Read + Send>, FsError> {
        Err(FsError::Unsupported)
    }
    fn open_write(&self, _: &VPath, _: leaf_vfs::WriteOpts) -> Result<Box<dyn leaf_vfs::WriteSink>, FsError> {
        Err(FsError::Unsupported)
    }
    fn mkdir(&self, _: &VPath) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
    fn rename(&self, _: &VPath, _: &VPath, _: bool) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
    fn remove(&self, _: &VPath, _: bool) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
    fn set_meta(&self, _: &VPath, _: &leaf_vfs::MetaChange) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
}

/// Runs a command on the server of `v` (for the Docker view). The caller quotes what it puts into the command.
pub(crate) fn exec_on(v: &VPath, command: &str, limit: std::time::Duration) -> Result<ExecOut, FsError> {
    sftp_backend().exec(v, command, limit, &|| false)
}
