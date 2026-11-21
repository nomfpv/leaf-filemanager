//! Unix permissions as the Security tab shows them: the nine read/write/execute bits as a 3x3 grid, the three special bits, and the
//! octal value. Pure (no file access), so the view model is the same on every system and the tests need no files.

/// Who a column of the grid is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Who {
    Owner,
    Group,
    Others,
}

/// What a row of the grid is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Perm {
    Read,
    Write,
    Execute,
}

pub const WHO: [Who; 3] = [Who::Owner, Who::Group, Who::Others];
pub const PERMS: [Perm; 3] = [Perm::Read, Perm::Write, Perm::Execute];

/// The permission bits of a file (`st_mode & 0o7777`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mode(pub u32);

impl Mode {
    pub fn new(bits: u32) -> Mode {
        Mode(bits & 0o7777)
    }

    fn bit(who: Who, perm: Perm) -> u32 {
        let shift = match who {
            Who::Owner => 6,
            Who::Group => 3,
            Who::Others => 0,
        };
        let p = match perm {
            Perm::Read => 4,
            Perm::Write => 2,
            Perm::Execute => 1,
        };
        p << shift
    }

    pub fn get(self, who: Who, perm: Perm) -> bool {
        self.0 & Self::bit(who, perm) != 0
    }

    pub fn set(&mut self, who: Who, perm: Perm, on: bool) {
        let b = Self::bit(who, perm);
        if on {
            self.0 |= b;
        } else {
            self.0 &= !b;
        }
    }

    /// The grid with one box flipped.
    pub fn toggled(self, who: Who, perm: Perm) -> Mode {
        let mut m = self;
        m.set(who, perm, !self.get(who, perm));
        m
    }

    pub fn setuid(self) -> bool {
        self.0 & 0o4000 != 0
    }
    pub fn setgid(self) -> bool {
        self.0 & 0o2000 != 0
    }
    pub fn sticky(self) -> bool {
        self.0 & 0o1000 != 0
    }

    /// "755" (three digits; four when a special bit is set: "1777").
    pub fn octal(self) -> String {
        if self.0 & 0o7000 != 0 { format!("{:04o}", self.0) } else { format!("{:03o}", self.0 & 0o777) }
    }

    /// An octal value as typed ("644", "0755", "1777"); `None` for anything else.
    pub fn parse_octal(text: &str) -> Option<Mode> {
        let t = text.trim();
        if t.is_empty() || t.len() > 4 || !t.chars().all(|c| ('0'..='7').contains(&c)) {
            return None;
        }
        u32::from_str_radix(t, 8).ok().map(Mode::new)
    }

    /// "rwxr-xr-x", with the special bits as `ls` writes them (s, S, t, T).
    pub fn symbolic(self) -> String {
        let mut s = String::with_capacity(9);
        for (i, who) in WHO.iter().enumerate() {
            s.push(if self.get(*who, Perm::Read) { 'r' } else { '-' });
            s.push(if self.get(*who, Perm::Write) { 'w' } else { '-' });
            let x = self.get(*who, Perm::Execute);
            let special = match i {
                0 => self.setuid(),
                1 => self.setgid(),
                _ => self.sticky(),
            };
            s.push(match (special, x, i) {
                (true, true, 2) => 't',
                (true, false, 2) => 'T',
                (true, true, _) => 's',
                (true, false, _) => 'S',
                (false, true, _) => 'x',
                (false, false, _) => '-',
            });
        }
        s
    }

    /// The special bits as words, for the text under the grid ("setuid, sticky"); empty when none is set.
    pub fn special_words(self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.setuid() {
            v.push("setuid");
        }
        if self.setgid() {
            v.push("setgid");
        }
        if self.sticky() {
            v.push("sticky");
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_and_the_octal_value_agree() {
        let m = Mode::new(0o640);
        assert!(m.get(Who::Owner, Perm::Read) && m.get(Who::Owner, Perm::Write) && !m.get(Who::Owner, Perm::Execute));
        assert!(m.get(Who::Group, Perm::Read) && !m.get(Who::Group, Perm::Write));
        assert!(!m.get(Who::Others, Perm::Read));
        assert_eq!(m.octal(), "640");
        assert_eq!(m.toggled(Who::Others, Perm::Read).octal(), "644");
        assert_eq!(m.toggled(Who::Owner, Perm::Execute).symbolic(), "rwxr-----");
        assert_eq!(Mode::parse_octal("0755"), Some(Mode(0o755)));
        assert_eq!(Mode::parse_octal("1777").unwrap().octal(), "1777");
        assert_eq!(Mode::parse_octal("789"), None);
        assert_eq!(Mode::parse_octal(""), None);
        assert_eq!(Mode::parse_octal("12345"), None);
    }

    #[test]
    fn special_bits_are_written_as_ls_writes_them() {
        assert_eq!(Mode::new(0o4755).symbolic(), "rwsr-xr-x");
        assert_eq!(Mode::new(0o2644).symbolic(), "rw-r-Sr--");
        assert_eq!(Mode::new(0o1777).symbolic(), "rwxrwxrwt");
        assert_eq!(Mode::new(0o1776).symbolic(), "rwxrwxrwT");
        assert_eq!(Mode::new(0o6000).special_words(), vec!["setuid", "setgid"]);
    }
}
