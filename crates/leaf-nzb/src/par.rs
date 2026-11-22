//! The par2 steps: names fixed from the par2 file list (obfuscated posts), a quick check, a full verify only when the quick
//! check is not sure, and a repair only when something is damaged, with only as many recovery volumes fetched as the
//! damage needs. The engine is `rust_par2` (MIT OR Apache-2.0).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use rust_par2::{Par2FileSet, VerifyResult};

/// The index file of the set: the smallest `.par2` that is not a recovery volume (the smallest volume when there is none).
pub fn index_file(dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<(bool, u64, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("par2")))
        .map(|p| (crate::nzb::par2_blocks(&p.file_name().unwrap_or_default().to_string_lossy()).is_some(), std::fs::metadata(&p).map_or(u64::MAX, |m| m.len()), p))
        .collect();
    v.sort();
    v.into_iter().next().map(|(_, _, p)| p)
}

/// Whether every file the set names is one plain file right in `dir`: a name that is not exactly its own [`safe_name`]
/// (a path, `..`, a drive, `:`, a reserved device name) or a target that resolves outside `dir` (a link) refuses the whole
/// set. `rust_par2` joins the names onto the folder and creates what is "missing", so a hostile set could otherwise write
/// anywhere. The reason when refused.
///
/// [`safe_name`]: crate::nzb::safe_name
pub fn check_names(set: &Par2FileSet, dir: &Path) -> Result<(), String> {
    let root = std::fs::canonicalize(dir).map_err(|e| format!("the job folder cannot be read: {e}"))?;
    for f in set.files.values() {
        if f.filename.is_empty() || crate::nzb::safe_name(&f.filename) != f.filename {
            return Err("the par2 set names a file outside the job folder".into());
        }
        let target = dir.join(&f.filename);
        // (a name that is there already must be a file in the folder itself, not a link to elsewhere)
        let inside = match std::fs::symlink_metadata(&target) {
            Ok(m) if m.file_type().is_symlink() => false,
            Ok(_) => std::fs::canonicalize(&target).is_ok_and(|t| t.parent() == Some(root.as_path())),
            Err(_) => target.parent().and_then(|p| std::fs::canonicalize(p).ok()).as_deref() == Some(root.as_path()),
        };
        if !inside {
            return Err("the par2 set names a file outside the job folder".into());
        }
    }
    Ok(())
}

pub fn parse(index: &Path) -> Option<Par2FileSet> {
    match rust_par2::parse(index) {
        Ok(s) => Some(s),
        Err(e) => {
            log::info!("usenet: the par2 file could not be read: {e}");
            None
        }
    }
}

/// Files of the job whose content is a file of the par2 set under another name (obfuscated posts): renamed to the set's
/// name. A name already taken by a file that is not of the set is left alone. Returns how many were renamed.
pub fn fix_names(set: &Par2FileSet, dir: &Path) -> usize {
    let mut wanted: HashMap<([u8; 16], u64), String> = HashMap::new();
    for f in set.files.values() {
        wanted.insert((f.hash_16k, f.size), crate::nzb::safe_name(&f.filename));
    }
    let names: std::collections::HashSet<String> = wanted.values().map(|n| n.to_lowercase()).collect();
    let mut renamed = 0;
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    for e in rd.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        if !p.is_file() || name.starts_with(".leaf-nzb") || name.to_lowercase().ends_with(".par2") || names.contains(&name.to_lowercase()) {
            continue;
        }
        let Ok(len) = std::fs::metadata(&p).map(|m| m.len()) else { continue };
        let Ok(h) = rust_par2::compute_hash_16k(&p) else { continue };
        if let Some(target) = wanted.get(&(h, len)).filter(|t| !t.is_empty()) {
            let to = dir.join(target);
            if !to.exists() && std::fs::rename(&p, &to).is_ok() {
                log::info!("usenet: renamed an obfuscated file after the par2 list");
                renamed += 1;
            }
        }
    }
    renamed
}

/// The quick check: every file of the set is there with its size and the hash of its first 16 KiB, and the download had no
/// failed or damaged article. `true`: no full verify is needed.
pub fn quick_ok(set: &Par2FileSet, dir: &Path, clean_download: bool) -> bool {
    clean_download
        && set.files.values().all(|f| {
            let p = dir.join(crate::nzb::safe_name(&f.filename));
            std::fs::metadata(&p).is_ok_and(|m| m.len() == f.size) && rust_par2::compute_hash_16k(&p).is_ok_and(|h| h == f.hash_16k)
        })
}

pub fn verify(set: &Par2FileSet, dir: &Path) -> VerifyResult {
    rust_par2::verify(set, dir)
}

/// Recovery blocks the par2 files already in `dir` hold.
pub fn blocks_here(set: &Par2FileSet, dir: &Path) -> usize {
    rust_par2::recovery::load_recovery_blocks(dir, &set.recovery_set_id, set.slice_size).len()
}

/// Which volumes to fetch for `need` more blocks: `(index, blocks)` of the volumes not fetched yet; the fewest bytes that give
/// enough (the smallest volumes that reach `need`, else all).
pub fn choose_volumes(mut volumes: Vec<(usize, u32)>, need: u32) -> Vec<usize> {
    if need == 0 {
        return Vec::new();
    }
    // the one smallest volume that covers it alone, else the biggest first until it is covered
    volumes.sort_by_key(|v| v.1);
    if let Some(v) = volumes.iter().find(|v| v.1 >= need) {
        return vec![v.0];
    }
    let mut got = 0;
    let mut out = Vec::new();
    for v in volumes.iter().rev() {
        if got >= need {
            break;
        }
        got += v.1;
        out.push(v.0);
    }
    out
}

pub enum Repaired {
    Ok(u32),
    /// Not enough recovery blocks, or the repair did not give correct files.
    Failed(String),
}

pub fn repair(set: &Par2FileSet, dir: &Path, verified: &VerifyResult) -> Repaired {
    // (checked again right before anything is written)
    if let Err(why) = check_names(set, dir) {
        return Repaired::Failed(why);
    }
    match rust_par2::repair_from_verify(set, dir, verified) {
        Ok(r) if r.success => Repaired::Ok(r.blocks_repaired),
        Ok(r) => Repaired::Failed(r.message),
        Err(e) => Repaired::Failed(e.to_string()),
    }
}
