//! `.ts` is TypeScript far more often than an MPEG transport stream. A file is a video only when it carries the
//! transport stream sync bytes; inside a repository, or when it is small readable text, it is source code.

use std::path::Path;

use crate::Entry;

/// Bytes read from the start of a file to decide.
pub const SNIFF_LEN: usize = 600;

/// True when 0x47 stands at the start of three packets in a row (188-byte packets, or 192 with a timecode).
pub fn is_transport_stream(head: &[u8]) -> bool {
    let synced = |start: usize, step: usize| (0..3).all(|k| head.get(start + k * step) == Some(&0x47));
    synced(0, 188) || synced(4, 192)
}

fn is_readable_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        // a multi-byte character cut by the end of the sample is fine
        Err(e) => e.error_len().is_none(),
    }
}

/// The extensions that are TypeScript source when they are not video.
pub fn is_ts_ext(ext: &str) -> bool {
    matches!(ext, "ts" | "mts" | "cts")
}

/// Whether a `.ts`/`.mts`/`.cts` file is TypeScript source: always inside a repository, otherwise when the start of
/// the file is readable text and not a transport stream.
pub fn is_ts_source(in_repo: bool, head: &[u8]) -> bool {
    if is_transport_stream(head) {
        return false;
    }
    in_repo || is_readable_text(head)
}

/// Whether `dir` is inside a git repository (a `.git` folder or file in it or above it).
pub fn in_repository(dir: &Path) -> bool {
    dir.ancestors().any(|d| d.join(".git").exists())
}

/// Sets `ts_source` on the `.ts`/`.mts`/`.cts` files of a listing of `dir`.
pub fn mark_ts_sources(dir: &Path, entries: &mut [Entry]) {
    use std::io::Read;
    let mut repo: Option<bool> = None;
    for e in entries.iter_mut() {
        if e.is_dir() || !is_ts_ext(&e.ext) {
            continue;
        }
        let in_repo = *repo.get_or_insert_with(|| in_repository(dir));
        let mut head = Vec::with_capacity(SNIFF_LEN);
        if let Ok(f) = std::fs::File::open(dir.join(&*e.name)) {
            let _ = f.take(SNIFF_LEN as u64).read_to_end(&mut head);
        }
        e.ts_source = is_ts_source(in_repo, &head);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> Vec<u8> {
        let mut v = vec![0u8; 188 * 3 + 1];
        for k in 0..3 {
            v[k * 188] = 0x47;
        }
        v
    }

    #[test]
    fn ts_is_source_or_video() {
        let cases: [(&str, bool, Vec<u8>, bool); 6] = [
            ("small text outside a repository", false, b"export const a: number = 1;\n".to_vec(), true),
            ("empty file", false, Vec::new(), true),
            ("real transport stream", false, stream(), false),
            ("transport stream in a repository stays video", true, stream(), false),
            ("binary without sync bytes outside a repository", false, vec![1, 0, 2, 0xff], false),
            ("binary inside a repository", true, vec![1, 0, 2, 0xff], true),
        ];
        for (what, repo, head, want) in cases {
            assert_eq!(is_ts_source(repo, &head), want, "{what}");
        }
        assert!(is_ts_ext("mts") && is_ts_ext("cts") && !is_ts_ext("m2ts"));
    }
}
