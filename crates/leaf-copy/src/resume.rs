//! What to do with one file of an interrupted copy when it is carried on: pure decisions from
//! what the journal recorded and what lies on the disk now (the engine gathers both; nothing here
//! touches the file system).

/// A file's size and modification time (seconds and nanoseconds since 1970).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Meta {
    pub size: u64,
    pub mtime: (i64, i64),
}

/// A file the journal saw being copied: its source as it was when the copy started, and how
/// many bytes of its part file were on the disk (synced) at the last progress record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Started {
    pub src: Meta,
    pub offset: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    /// The destination already is the finished copy: nothing to write.
    Skip,
    /// Carry on into the part file from this byte (the part is cut back to it first).
    ResumeAt(u64),
    /// Copy the file again from its first byte (into a fresh part file).
    Restart,
}

/// How far a finished copy's time may lie from its source's: FAT keeps 2 s, exFAT 10 ms, SMB and ntfs-3g 100 ns, so a copy there
/// never has its source's time to the nanosecond.
pub const TIME_SLACK_NS: i128 = 2_000_000_000;

/// Whether `dst` is the finished copy of `src`, for a destination name the journal knows as its own (it recorded the file finished
/// there, or began its part file): the source's size, and a time within [`TIME_SLACK_NS`] of the source's (the engine sets the copy's
/// time from the source after the last byte; the destination's file system may keep it coarser). A name the journal does not know
/// is never taken for the copy, whatever its size and time.
pub fn is_finished(src: Meta, dst: Meta) -> bool {
    let ns = |m: Meta| m.mtime.0 as i128 * 1_000_000_000 + m.mtime.1 as i128;
    src.size == dst.size && (ns(src) - ns(dst)).abs() <= TIME_SLACK_NS
}

/// One file of a resumed job.
/// * `started`: the journal's record of it, if the copy had begun;
/// * `src`: the source now; `part_len`: the length of its part file now, if there is one;
/// * `dst_finished`: the destination name already holds the finished copy ([`is_finished`], for a name the journal knows).
///
/// A part file is carried on only while the source is unchanged since the journal saw it and the part still holds every recorded
/// byte; anything else starts the file over. A finished destination is never a target for writing: resuming only ever writes into
/// the part file.
pub fn decide(started: Option<Started>, src: Meta, part_len: Option<u64>, dst_finished: bool) -> Decision {
    if dst_finished {
        return Decision::Skip;
    }
    let (Some(j), Some(have)) = (started, part_len) else { return Decision::Restart };
    if j.src != src || j.offset == 0 || j.offset > src.size || have < j.offset {
        return Decision::Restart;
    }
    Decision::ResumeAt(j.offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_decisions() {
        let src = Meta { size: 100, mtime: (5, 7) };
        let j = Some(Started { src, offset: 40 });
        assert_eq!(decide(j, src, Some(60), true), Decision::Skip, "a finished copy is kept");
        assert_eq!(decide(j, src, Some(60), false), Decision::ResumeAt(40));
        assert_eq!(decide(j, Meta { size: 100, mtime: (6, 0) }, Some(60), false), Decision::Restart, "the source changed");
        assert_eq!(decide(j, Meta { size: 120, mtime: (5, 7) }, Some(60), false), Decision::Restart, "the source grew");
        assert_eq!(decide(j, src, Some(39), false), Decision::Restart, "the part lost recorded bytes");
        assert_eq!(decide(j, src, None, false), Decision::Restart, "no part file");
        assert_eq!(decide(None, src, Some(60), false), Decision::Restart, "never recorded");
        // a copy on FAT (2 s), exFAT or SMB keeps a coarser time
        assert!(is_finished(src, Meta { size: 100, mtime: (6, 0) }));
        assert!(is_finished(src, Meta { size: 100, mtime: (4, 0) }));
        assert!(!is_finished(src, Meta { size: 100, mtime: (8, 0) }), "more than 2 s off");
        assert!(!is_finished(src, Meta { size: 99, mtime: (5, 7) }), "another size");
    }
}
