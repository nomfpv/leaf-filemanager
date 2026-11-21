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

/// One file of a resumed job.
/// * `started`: the journal's record of it, if the copy had begun;
/// * `src`: the source now; `part_len`: the length of its part file now, if there is one;
/// * `dst`: the finished name now, if something is there.
///
/// The destination counts as finished only when it has the source's size and time (the engine
/// sets the time from the source after the last byte). A part file is carried on only while the
/// source is unchanged since the journal saw it and the part still holds every recorded byte;
/// anything else starts the file over. A finished destination is never a target for writing:
/// resuming only ever writes into the part file.
pub fn decide(started: Option<Started>, src: Meta, part_len: Option<u64>, dst: Option<Meta>) -> Decision {
    if dst == Some(src) {
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
        assert_eq!(decide(j, src, Some(60), Some(src)), Decision::Skip, "a finished copy is kept");
        assert_eq!(decide(j, src, Some(60), None), Decision::ResumeAt(40));
        assert_eq!(decide(j, src, Some(40), Some(Meta { size: 3, mtime: (1, 1) })), Decision::ResumeAt(40), "an old file there (a replace) is not the copy");
        assert_eq!(decide(j, Meta { size: 100, mtime: (6, 0) }, Some(60), None), Decision::Restart, "the source changed");
        assert_eq!(decide(j, Meta { size: 120, mtime: (5, 7) }, Some(60), None), Decision::Restart, "the source grew");
        assert_eq!(decide(j, src, Some(39), None), Decision::Restart, "the part lost recorded bytes");
        assert_eq!(decide(j, src, None, None), Decision::Restart, "no part file");
        assert_eq!(decide(None, src, Some(60), None), Decision::Restart, "never recorded");
    }
}
