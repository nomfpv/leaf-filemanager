//! Folder compare: two listings in, one verdict per name out. Pure; the window turns the verdicts into marks and tints.
//! Names match as this system matches file names (`pathkey`): without case on Windows, exactly on Linux. Modified times
//! within [`TOLERANCE_MS`] are the same time (FAT keeps two-second steps, zip too).

use crate::pathkey;
use std::collections::HashMap;

/// Two modified times this close (milliseconds) are equal.
pub const TOLERANCE_MS: i64 = 2000;

/// What the compare needs to know about one item of a listing.
#[derive(Clone, Debug)]
pub struct Item<'a> {
    pub name: &'a str,
    pub size: u64,
    pub modified_ms: i64,
    pub is_dir: bool,
}

/// The verdict for one name, as seen from the left pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mark {
    OnlyLeft,
    OnlyRight,
    /// Same size and (within the tolerance) the same time.
    Same,
    /// The left one was changed later.
    NewerLeft,
    /// The right one was changed later.
    NewerRight,
    /// Same time, other size.
    DifferentSize,
}

impl Mark {
    pub fn is_same(self) -> bool {
        self == Mark::Same
    }
}

/// The verdicts of both sides (the same one for a name on both; "left" and "right" always mean the two panes), keyed by the folded name (`pathkey::fold`).
#[derive(Default, Debug)]
pub struct Outcome {
    pub left: HashMap<String, Mark>,
    pub right: HashMap<String, Mark>,
}

/// Counts for the bar: (only left, only right, newer left, newer right, different size, same).
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts {
    pub only_left: usize,
    pub only_right: usize,
    pub newer_left: usize,
    pub newer_right: usize,
    pub different: usize,
    pub same: usize,
}

impl Outcome {
    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        for m in self.left.values() {
            match m {
                Mark::OnlyLeft => c.only_left += 1,
                Mark::NewerLeft => c.newer_left += 1,
                Mark::NewerRight => c.newer_right += 1,
                Mark::DifferentSize => c.different += 1,
                Mark::Same => c.same += 1,
                Mark::OnlyRight => {}
            }
        }
        c.only_right = self.right.values().filter(|m| **m == Mark::OnlyRight).count();
        c
    }
}

/// The verdict for a pair that exists on both sides. Folders have no size or time worth comparing: they are the same.
fn pair(l: &Item, r: &Item) -> Mark {
    if l.is_dir && r.is_dir {
        return Mark::Same;
    }
    let d = l.modified_ms - r.modified_ms;
    if d > TOLERANCE_MS {
        Mark::NewerLeft
    } else if d < -TOLERANCE_MS {
        Mark::NewerRight
    } else if l.size != r.size {
        Mark::DifferentSize
    } else {
        Mark::Same
    }
}

pub fn compare(left: &[Item], right: &[Item]) -> Outcome {
    let rmap: HashMap<String, &Item> = right.iter().map(|i| (pathkey::fold(i.name), i)).collect();
    let mut out = Outcome::default();
    for l in left {
        let key = pathkey::fold(l.name);
        let m = match rmap.get(&key) {
            None => Mark::OnlyLeft,
            Some(r) => pair(l, r),
        };
        out.left.insert(key.clone(), m);
        if m != Mark::OnlyLeft {
            out.right.insert(key, m);
        }
    }
    for r in right {
        out.right.entry(pathkey::fold(r.name)).or_insert(Mark::OnlyRight);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(name: &str, size: u64, modified_ms: i64) -> Item<'_> {
        Item { name, size, modified_ms, is_dir: false }
    }

    #[test]
    fn verdicts() {
        let left = [it("a.txt", 5, 10_000), it("b.txt", 5, 100_000), it("c.txt", 5, 10_000), it("d.txt", 7, 10_000), it("e.txt", 5, 11_500), it("L.txt", 1, 0)];
        let right = [it("a.txt", 5, 10_000), it("b.txt", 5, 10_000), it("c.txt", 5, 90_000), it("d.txt", 9, 10_000), it("e.txt", 5, 10_000), it("r.txt", 1, 0)];
        let r = compare(&left, &right);
        assert_eq!(r.left["a.txt"], Mark::Same);
        assert_eq!(r.left["b.txt"], Mark::NewerLeft);
        // (both sides carry the same verdict, which is always the left's view)
        assert_eq!(r.right["b.txt"], Mark::NewerLeft);
        assert_eq!(r.left["c.txt"], Mark::NewerRight);
        assert_eq!(r.left["d.txt"], Mark::DifferentSize);
        // within two seconds is the same time
        assert_eq!(r.left["e.txt"], Mark::Same);
        assert_eq!(r.right["r.txt"], Mark::OnlyRight);
        assert_eq!(r.left[&pathkey::fold("L.txt")], Mark::OnlyLeft);
        assert!(!r.right.contains_key(&pathkey::fold("L.txt")));
        let c = r.counts();
        assert_eq!((c.only_left, c.only_right, c.newer_left, c.newer_right, c.different, c.same), (1, 1, 1, 1, 1, 2));
    }

    #[test]
    fn names_follow_the_system() {
        let r = compare(&[it("Readme.md", 1, 0)], &[it("README.md", 1, 0)]);
        if cfg!(windows) {
            assert_eq!(r.left["readme.md"], Mark::Same);
        } else {
            assert_eq!(r.left["Readme.md"], Mark::OnlyLeft);
            assert_eq!(r.right["README.md"], Mark::OnlyRight);
        }
    }
}
