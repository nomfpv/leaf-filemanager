//! One NZB from start to end, each step visible: download, par2 quick check, verify, recovery volumes (only as many as
//! needed), repair, unpack, join, cleanup, move into the category folder. A job that fails keeps what it has in its folder
//! (the incomplete folder) and says why.

use std::path::{Path, PathBuf};

use crate::dial::Dialer;
use crate::engine::{self, Control, FileJob, State};
use crate::nzb::Nzb;
use crate::par;
use crate::post::{self, Taken, UnpackError};
use crate::server::Server;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Download,
    QuickCheck,
    Verify,
    /// Fetching par2 recovery volumes for a repair.
    Recovery,
    Repair,
    Unpack,
    Join,
    Cleanup,
    Move,
}

pub struct Spec {
    pub nzb: Nzb,
    /// The job's folder while it runs (inside the incomplete folder).
    pub job_dir: PathBuf,
    /// The folder the result goes into (the category's).
    pub dest_dir: PathBuf,
    /// The result's folder name.
    pub name: String,
    pub unpack: bool,
    /// Remove par2 files, archive volumes and samples once the job is good.
    pub cleanup: bool,
}

pub trait Host: engine::Host {
    fn step(&self, step: Step);
    /// The archive's password, asked once in leaf's dialog (`wrong`: the NZB's password did not fit). `None`: no answer.
    fn ask_password(&self, archive: &str, wrong: bool) -> Option<String>;
    /// The result's folder name is taken in the destination.
    fn taken(&self, path: &Path) -> Taken;
    /// Progress of a step that is not the download (0..=total).
    fn step_progress(&self, _done: u64, _total: u64) {}
}

#[derive(Debug)]
pub enum Error {
    Download(engine::Error),
    /// Articles are missing and there is no par2 set to repair with (or the repair could not do it).
    Damaged { failed: usize, detail: String },
    Password { wrong: bool },
    Unpack(String),
    Io(std::io::Error),
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Download(e) => write!(f, "{e}"),
            Error::Damaged { failed, detail } => write!(f, "{failed} articles missing: {detail}"),
            Error::Password { wrong } => f.write_str(if *wrong { "wrong archive password" } else { "archive password needed" }),
            Error::Unpack(d) => write!(f, "unpack: {d}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Cancelled => f.write_str("cancelled"),
        }
    }
}

#[derive(Debug, Default)]
pub struct Done {
    /// Where the result is; `None` when the destination's name was taken and the user chose to skip (the result stays in the
    /// incomplete folder).
    pub folder: Option<PathBuf>,
    pub articles_total: usize,
    pub articles_failed: usize,
    pub repaired_blocks: u32,
}

fn file_jobs(nzb: &Nzb, pick: impl Fn(usize) -> bool) -> Vec<FileJob> {
    nzb.files.iter().enumerate().filter(|(i, _)| pick(*i)).map(|(i, f)| FileJob { index: i, name: f.name.clone(), date: f.date, segments: f.segments.clone() }).collect()
}

/// The folder for a job of `nzb` named `name` in `incomplete`: `name`, else `name (2)`, `name (3)`, ...: the first that is
/// not there or holds this NZB's own state (a restart carries on there). Two NZBs with one title never share a folder, so
/// one never adopts or cleans up the other's files.
pub fn job_dir(incomplete: &Path, name: &str, nzb: &Nzb) -> PathBuf {
    let fp = nzb.fingerprint();
    for n in 1..10_000 {
        let p = if n == 1 { incomplete.join(name) } else { incomplete.join(format!("{name} ({n})")) };
        if !p.exists() || State::load(&p).nzb == fp {
            return p;
        }
    }
    incomplete.join(format!("{name} ({fp:016x})"))
}

pub fn run(spec: &Spec, servers: &[Server], dialer: &dyn Dialer, ctl: &Control, host: &dyn Host) -> Result<Done, Error> {
    let nzb = &spec.nzb;
    let dir = &spec.job_dir;
    std::fs::create_dir_all(dir).map_err(Error::Io)?;
    // the par2 recovery volumes wait: they are fetched only when a repair needs them (an NZB without an index file fetches
    // its smallest volume as the index)
    let volumes: Vec<(usize, u32)> = nzb.files.iter().enumerate().filter_map(|(i, f)| f.par2_blocks().map(|b| (i, b))).collect();
    let has_index = nzb.files.iter().any(|f| f.is_par2() && f.par2_blocks().is_none());
    let index_volume = (!has_index).then(|| volumes.iter().min_by_key(|v| v.1).map(|v| v.0)).flatten();
    let later: std::collections::HashSet<usize> = volumes.iter().map(|v| v.0).filter(|i| Some(*i) != index_volume).collect();
    // (the state of another NZB in this folder is never adopted: its files are not this job's)
    let fp = nzb.fingerprint();
    let mut state = State::load(dir);
    if state.nzb != fp && (state.nzb != 0 || !state.files.is_empty()) {
        return Err(Error::Io(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "the job folder belongs to another download")));
    }
    if state.nzb == 0 {
        state.nzb = fp;
        state.save(dir);
    }
    host.step(Step::Download);
    let now = file_jobs(nzb, |i| !later.contains(&i));
    let fetched = engine::download(dir, &now, servers, dialer, ctl, host, &mut state).map_err(|e| match e {
        engine::Error::Cancelled => Error::Cancelled,
        e => Error::Download(e),
    })?;
    let articles_total: usize = nzb.files.iter().map(|f| f.segments.len()).sum();
    let mut failed: usize = fetched.iter().map(|f| f.failed).sum();
    // (good without a full verify only when every segment of every file was written and the NZB numbered them all)
    let clean = failed == 0 && fetched.iter().all(|f| f.bad_crc == 0 && f.path.is_some() && f.complete && !nzb.files.get(f.index).is_some_and(|n| n.irregular));
    let mut repaired_blocks = 0;

    // par2
    // (a set whose names leave the job folder is refused whole: the job goes on as one without par2)
    let set = par::index_file(dir).and_then(|i| par::parse(&i)).filter(|s| match par::check_names(s, dir) {
        Ok(()) => true,
        Err(why) => {
            log::warn!("usenet: par2 set refused: {why}");
            false
        }
    });
    if let Some(set) = &set {
        par::fix_names(set, dir);
        host.step(Step::QuickCheck);
        if !par::quick_ok(set, dir, clean) {
            host.step(Step::Verify);
            let mut verified = par::verify(set, dir);
            if !verified.all_correct() {
                let need = verified.blocks_needed();
                let have = par::blocks_here(set, dir) as u32;
                if have < need {
                    let fetchable: Vec<(usize, u32)> = volumes.iter().copied().filter(|v| later.contains(&v.0)).collect();
                    let pick = par::choose_volumes(fetchable, need - have);
                    if !pick.is_empty() {
                        host.step(Step::Recovery);
                        let more = file_jobs(nzb, |i| pick.contains(&i));
                        engine::download(dir, &more, servers, dialer, ctl, host, &mut state).map_err(|e| match e {
                            engine::Error::Cancelled => Error::Cancelled,
                            e => Error::Download(e),
                        })?;
                        verified = par::verify(set, dir);
                    }
                }
                if !verified.all_correct() {
                    host.step(Step::Repair);
                    match par::repair(set, dir, &verified) {
                        par::Repaired::Ok(n) => {
                            repaired_blocks = n;
                            failed = 0;
                        }
                        par::Repaired::Failed(detail) => return Err(Error::Damaged { failed, detail }),
                    }
                }
            }
        }
    }
    if ctl.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }

    // unpack
    let mut unpacked = false;
    if spec.unpack {
        let mut password = nzb.password.clone();
        let mut asked = false;
        for archive in post::archives(dir) {
            host.step(Step::Unpack);
            loop {
                let cancelled = || ctl.cancel.load(std::sync::atomic::Ordering::Relaxed);
                match post::unpack(&archive, dir, password.as_deref(), &mut |d, t| host.step_progress(d, t), &cancelled) {
                    Ok(_) => {
                        unpacked = true;
                        break;
                    }
                    Err(UnpackError::Password { wrong }) if !asked => {
                        asked = true;
                        let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        match host.ask_password(&name, wrong && password.is_some()) {
                            Some(p) => password = Some(p),
                            None => return Err(Error::Password { wrong }),
                        }
                    }
                    Err(UnpackError::Password { wrong }) => return Err(Error::Password { wrong }),
                    Err(UnpackError::Cancelled) => return Err(Error::Cancelled),
                    Err(UnpackError::Failed(d)) => {
                        return Err(if failed > 0 { Error::Damaged { failed, detail: d } } else { Error::Unpack(d) });
                    }
                }
            }
        }
    }
    if failed > 0 && set.is_none() && !unpacked {
        return Err(Error::Damaged { failed, detail: "no par2 files to repair with".into() });
    }

    host.step(Step::Join);
    post::join_split(dir).map_err(Error::Io)?;
    post::join_split(&dir.join(post::UNPACK_DIR)).map_err(Error::Io)?;
    if spec.cleanup {
        host.step(Step::Cleanup);
        post::cleanup(dir, unpacked);
    }
    host.step(Step::Move);
    let folder = post::place(dir, &spec.dest_dir, &spec.name, &|p| host.taken(p)).map_err(Error::Io)?;
    if folder.is_some() {
        // leaf's own leftovers; the folder goes only when nothing else is in it
        for n in [engine::STATE_FILE, &format!("{}.tmp", engine::STATE_FILE)] {
            let _ = std::fs::remove_file(dir.join(n));
        }
        let _ = std::fs::remove_dir(dir.join(post::UNPACK_DIR));
        let _ = std::fs::remove_dir(dir);
    }
    Ok(Done { folder, articles_total, articles_failed: failed, repaired_blocks })
}
