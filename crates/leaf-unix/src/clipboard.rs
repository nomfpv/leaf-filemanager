//! Linux backend of rf_win::clipboard. Status: REAL inside the process (a store plus a sequence number); other programs
//! do not see it yet (text/uri-list comes later).

use crate::Owner;
use leaf_common::fileops::Transfer;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(1);
static STORE: Mutex<Option<(Vec<PathBuf>, Transfer)>> = Mutex::new(None);

fn bump() {
    SEQ.fetch_add(1, Ordering::Relaxed);
}

pub fn clear(_owner: Owner) -> anyhow::Result<()> {
    *STORE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    bump();
    Ok(())
}

pub fn sequence() -> u32 {
    SEQ.load(Ordering::Relaxed)
}

pub fn set_files(_owner: Owner, paths: &[PathBuf], kind: Transfer) -> anyhow::Result<()> {
    *STORE.lock().unwrap_or_else(|e| e.into_inner()) = Some((paths.to_vec(), kind));
    bump();
    Ok(())
}

pub fn has_files() -> bool {
    STORE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|(p, _)| !p.is_empty())
}

pub fn can_paste() -> bool {
    has_files()
}

pub fn get_files(_owner: Owner) -> anyhow::Result<Option<(Vec<PathBuf>, Transfer)>> {
    Ok(STORE.lock().unwrap_or_else(|e| e.into_inner()).clone().filter(|(p, _)| !p.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_set_come_back_and_the_sequence_moves() {
        let before = sequence();
        set_files(Owner::default(), &[PathBuf::from("/a")], Transfer::Copy).unwrap();
        assert!(sequence() > before);
        assert!(has_files());
        let (p, k) = get_files(Owner::default()).unwrap().unwrap();
        assert_eq!((p, k), (vec![PathBuf::from("/a")], Transfer::Copy));
        clear(Owner::default()).unwrap();
        assert!(!has_files());
    }
}
