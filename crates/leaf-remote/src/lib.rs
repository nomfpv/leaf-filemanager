//! leaf-remote: places on other machines behind the `leaf-vfs` interface (PLAN 2.2, W1a). Today: SFTP, read-only.
//!
//! - `SftpFs` serves `sftp://user@host:port/path` over `russh` + `russh-sftp` (pure Rust).
//! - One connection per server, shared by every pane; it is opened again when it broke, and closed after a while unused.
//! - The async runtime lives in this crate (one worker thread) and starts with the first connection, never at app start.
//! - Host keys are checked against `~/.ssh/known_hosts`; an unknown key is asked about, a changed or revoked one refused.
//! - Login: the running agent, the key files, then a password; secrets stay in memory for this run of leaf only.
//!
//! The app calls [`register`] once at start (cheap: nothing connects, no thread starts) with its dialogs as the [`Prompter`].

pub mod checksum;
mod conn;
pub mod docker;
pub mod known_hosts;
mod prompt;
pub mod rclone;
mod sftp;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, OnceLock};

pub use conn::{Config, ExecOut};
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
    // `docker://local/` and `docker://user@host/`: the containers and their mounts, read-only (nothing runs until opened)
    leaf_vfs::register("docker", || Arc::new(docker::DockerFs) as Arc<dyn leaf_vfs::Fs>);
    // `rclone://remote/path`, only when the rclone program is installed (a PATH look-up; nothing runs here)
    if rclone::find_exe().is_some() {
        leaf_vfs::register("rclone", || match rclone::find_exe() {
            Some(exe) => Arc::new(rclone::RcloneFs::new(exe)) as Arc<dyn leaf_vfs::Fs>,
            None => Arc::new(NoRclone),
        });
    }
}

/// The home folder the server logs the user into (for an address that names only the server).
pub fn home(v: &VPath) -> Result<VPath, FsError> {
    match v.scheme() {
        "sftp" => sftp_backend().home(v),
        _ => Ok(v.root()),
    }
}

/// Whether a connection to the server of this `sftp://` address is open now (never connects).
pub fn is_connected(v: &VPath) -> bool {
    v.scheme() == "sftp" && sftp_backend().target_connected(v)
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
        _ if out.code == Some(127) => Err(FsError::Other(format!("The server has no {}", algo.program()))),
        _ => Err(FsError::Other(format!("{} gave no sum for this file", algo.program()))),
    }
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
