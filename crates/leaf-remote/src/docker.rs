//! The Docker view (H8): `docker://local/` or `docker://user@host/` lists the containers (running or not), a container lists its
//! mounts (bind mounts and volumes, by where they are inside the container), and a mount shows the folder it stands for, on
//! this PC or on the host over SFTP. Read-only. The list comes from `docker ps` and `docker inspect` (JSON); locally they are
//! started as argument lists, on a host they run over the open SSH connection. A volume's folder is where Docker keeps it
//! (`/var/lib/docker/volumes/...`): readable on a Linux machine you may read, not on Docker Desktop for Windows (inside its VM).

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use leaf_vfs::{Caps, Entry, EntryKind, Fs, FsError, Kind, Meta, MetaChange, VPath, WriteOpts, WriteSink};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// The name this mount has in the listing (the path inside the container, made one name).
    pub label: String,
    /// `bind` or `volume` (or `tmpfs`...).
    pub kind: String,
    /// Where it is on the Docker host (empty for tmpfs).
    pub source: String,
    /// Where it is inside the container.
    pub dest: String,
    pub rw: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub name: String,
    pub image: String,
    pub state: String,
    pub mounts: Vec<Mount>,
}

/// A path inside a container as one folder name: `/var/lib/data` becomes `var-lib-data`.
fn label_of(dest: &str, taken: &[String]) -> String {
    let base = dest.trim_matches('/').replace(['/', '\\', ':'], "-");
    let base = if base.is_empty() { "root".to_string() } else { base };
    let mut label = base.clone();
    let mut n = 2;
    while taken.contains(&label) {
        label = format!("{base}-{n}");
        n += 1;
    }
    label
}

/// The containers in the JSON `docker inspect` printed.
pub fn parse_inspect(json: &str) -> Result<Vec<Container>, FsError> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).map_err(|e| FsError::Other(format!("docker's answer could not be read: {e}")))?;
    let s = |x: &serde_json::Value, k: &str| x.get(k).and_then(|y| y.as_str()).unwrap_or_default().to_string();
    let mut out: Vec<Container> = Vec::new();
    for c in v.as_array().map(Vec::as_slice).unwrap_or_default() {
        let name = s(c, "Name").trim_start_matches('/').to_string();
        if leaf_vfs::names::address_problem(&name).is_some() {
            continue;
        }
        let mut mounts: Vec<Mount> = Vec::new();
        for m in c.get("Mounts").and_then(|m| m.as_array()).map(Vec::as_slice).unwrap_or_default() {
            let dest = s(m, "Destination");
            let taken: Vec<String> = mounts.iter().map(|x| x.label.clone()).collect();
            mounts.push(Mount { label: label_of(&dest, &taken), kind: s(m, "Type"), source: s(m, "Source"), dest, rw: m.get("RW").and_then(|x| x.as_bool()).unwrap_or(true) });
        }
        out.push(Container {
            image: c.get("Config").map(|x| s(x, "Image")).unwrap_or_default(),
            state: c.get("State").map(|x| s(x, "Status")).unwrap_or_default(),
            name,
            mounts,
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

/// The folder on the Docker host as a path on this PC (Docker Desktop on Windows shows `C:\x` or `/run/desktop/mnt/host/c/x`);
/// none when it cannot be reached from here.
fn local_path(source: &str) -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        let b = source.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return Some(source.into());
        }
        let rest = source.strip_prefix("/run/desktop/mnt/host/")?;
        let (drive, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if drive.len() != 1 {
            return None;
        }
        return Some(format!("{}:\\{}", drive.to_ascii_uppercase(), tail.replace('/', "\\")).into());
    }
    source.starts_with('/').then(|| source.into())
}

const LIST_CMD: &str = "ids=$(docker ps -aq); if [ -z \"$ids\" ]; then echo '[]'; else docker inspect $ids; fi";

fn docker_local(args: &[&str]) -> Result<String, FsError> {
    let mut c = Command::new("docker");
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let out = c.output().map_err(|e| FsError::Other(format!("docker could not be started: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(FsError::Other(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("docker failed").trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn fetch(authority: &str) -> Result<Vec<Container>, FsError> {
    if authority == "local" {
        let ids = docker_local(&["ps", "-aq"])?;
        let ids: Vec<&str> = ids.split_whitespace().collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut args = vec!["inspect"];
        args.extend(ids);
        return parse_inspect(&docker_local(&args)?);
    }
    let at = VPath::new("sftp", authority, "/").map_err(bad)?;
    let out = crate::exec_on(&at, LIST_CMD, Duration::from_secs(60))?;
    if out.code.unwrap_or(0) != 0 {
        return Err(FsError::Other(if out.code == Some(127) { "The server has no docker".into() } else { "docker gave no answer on the server".to_string() }));
    }
    parse_inspect(&out.stdout)
}

type Cache = Mutex<HashMap<String, (Instant, Arc<Vec<Container>>)>>;

fn cached(authority: &str) -> Result<Arc<Vec<Container>>, FsError> {
    static C: OnceLock<Cache> = OnceLock::new();
    let c = C.get_or_init(Mutex::default);
    if let Some((t, v)) = c.lock().unwrap_or_else(|e| e.into_inner()).get(authority)
        && t.elapsed() < Duration::from_secs(5)
    {
        return Ok(v.clone());
    }
    let v = Arc::new(fetch(authority)?);
    c.lock().unwrap_or_else(|e| e.into_inner()).insert(authority.to_string(), (Instant::now(), v.clone()));
    Ok(v)
}

enum Place {
    Root,
    Container(Container),
    /// The folder a mount stands for (a container's mount folder when `inner` is the mount's root).
    Mount { inner: Option<VPath>, root: bool },
}

fn bad(e: leaf_vfs::VPathError) -> FsError {
    FsError::Other(e.to_string())
}

fn resolve(v: &VPath) -> Result<Place, FsError> {
    let mut seg = v.segments();
    let Some(cname) = seg.next() else { return Ok(Place::Root) };
    let all = cached(v.authority())?;
    let c = all.iter().find(|c| c.name == cname).ok_or(FsError::NotFound)?;
    let Some(mlabel) = seg.next() else { return Ok(Place::Container(c.clone())) };
    let m = c.mounts.iter().find(|m| m.label == mlabel).ok_or(FsError::NotFound)?;
    let rest: Vec<&str> = seg.collect();
    let inner = target(v.authority(), m, &rest)?;
    Ok(Place::Mount { inner: Some(inner), root: rest.is_empty() })
}

/// The address of the mount's folder (plus `rest`) in the file system that really holds it.
fn target(authority: &str, m: &Mount, rest: &[&str]) -> Result<VPath, FsError> {
    if m.source.is_empty() {
        return Err(FsError::Other("This mount lives in memory (tmpfs); there is nothing to show".into()));
    }
    if authority == "local" {
        let mut p = local_path(&m.source).ok_or_else(|| FsError::Other("This folder is inside Docker's own virtual machine and cannot be reached from here".to_string()))?;
        for r in rest {
            p.push(r);
        }
        return Ok(leaf_vfs::LocalFs::address(&p));
    }
    let mut path = m.source.trim_end_matches('/').to_string();
    for r in rest {
        path.push('/');
        path.push_str(r);
    }
    VPath::new("sftp", authority, &path).map_err(bad)
}

fn inner_fs(v: &VPath) -> Result<Arc<dyn Fs>, FsError> {
    leaf_vfs::fs_for(v.scheme())
}

pub struct DockerFs;

impl Fs for DockerFs {
    fn caps(&self) -> Caps {
        Caps { links: true, ..Caps::default() }
    }

    fn list(&self, dir: &VPath, batch: usize, cancelled: &dyn Fn() -> bool, on_batch: &mut dyn FnMut(Vec<Entry>)) -> Result<usize, FsError> {
        let mut rows: Vec<Entry> = Vec::new();
        match resolve(dir)? {
            Place::Root => {
                for c in cached(dir.authority())?.iter() {
                    let mut e = Entry::new(&c.name, EntryKind::Dir, 0, 0, 0, rf_core::attrs::DIRECTORY);
                    e.label = Some(format!("{}  \u{b7}  {}  \u{b7}  {}", c.name, c.image, c.state).into());
                    rows.push(e);
                }
            }
            Place::Container(c) => {
                for m in &c.mounts {
                    let mut e = Entry::new(&m.label, EntryKind::Dir, 0, 0, 0, rf_core::attrs::DIRECTORY);
                    e.label = Some(format!("{}  \u{b7}  {}{}", m.dest, m.kind, if m.rw { "" } else { ", read-only" }).into());
                    rows.push(e);
                }
            }
            Place::Mount { inner, .. } => {
                let t = inner.ok_or(FsError::NotFound)?;
                return inner_fs(&t)?.list(&t, batch, cancelled, on_batch);
            }
        }
        let n = rows.len();
        for part in rows.chunks(batch.max(1)) {
            on_batch(part.to_vec());
        }
        Ok(n)
    }

    fn stat(&self, p: &VPath) -> Result<Meta, FsError> {
        match resolve(p)? {
            Place::Mount { inner: Some(t), root: false } => inner_fs(&t)?.stat(&t),
            _ => Ok(Meta { kind: Kind::Dir, size: 0, modified_ms: 0, created_ms: 0, mode: None }),
        }
    }

    fn open_read(&self, p: &VPath, offset: u64) -> Result<Box<dyn Read + Send>, FsError> {
        match resolve(p)? {
            Place::Mount { inner: Some(t), root: false } => inner_fs(&t)?.open_read(&t, offset),
            _ => Err(FsError::Unsupported),
        }
    }

    fn open_write(&self, _: &VPath, _: WriteOpts) -> Result<Box<dyn WriteSink>, FsError> {
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
    fn set_meta(&self, _: &VPath, _: &MetaChange) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_containers_and_their_mounts() {
        let json = r#"[{"Name":"/web","Config":{"Image":"nginx:1"},"State":{"Status":"running"},
            "Mounts":[{"Type":"bind","Source":"/srv/site","Destination":"/usr/share/nginx/html","RW":false},
                      {"Type":"volume","Name":"v","Source":"/var/lib/docker/volumes/v/_data","Destination":"/data","RW":true},
                      {"Type":"volume","Source":"/x","Destination":"/data","RW":true}]}]"#;
        let c = parse_inspect(json).unwrap();
        assert_eq!((c[0].name.as_str(), c[0].image.as_str(), c[0].state.as_str()), ("web", "nginx:1", "running"));
        let labels: Vec<&str> = c[0].mounts.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, ["usr-share-nginx-html", "data", "data-2"]);
        assert!(!c[0].mounts[0].rw);
    }
}
