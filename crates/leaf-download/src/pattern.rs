//! Address patterns for a batch: `file[001-100].zip` (numbers, zero-padded when the first has leading zeros or both have the same
//! width) and `[a-z]` / `[A-F]` (letters, either direction). Several groups multiply. A bracket that is not such a group stays as it
//! is (an IPv6 host's `[::1]`).

/// The most addresses one pattern makes.
pub const MAX: usize = 5000;

enum Group {
    Num { from: u64, to: u64, width: usize },
    Letters { from: u8, to: u8 },
}

impl Group {
    fn parse(inner: &str) -> Option<Self> {
        let (a, b) = inner.split_once('-')?;
        if !a.is_empty() && !b.is_empty() && a.bytes().all(|c| c.is_ascii_digit()) && b.bytes().all(|c| c.is_ascii_digit()) {
            let width = if a.len() == b.len() || a.starts_with('0') { a.len() } else { 0 };
            return Some(Self::Num { from: a.parse().ok()?, to: b.parse().ok()?, width });
        }
        let (a, b) = (a.as_bytes(), b.as_bytes());
        match (a, b) {
            ([x], [y]) if x.is_ascii_lowercase() && y.is_ascii_lowercase() || x.is_ascii_uppercase() && y.is_ascii_uppercase() => Some(Self::Letters { from: *x, to: *y }),
            _ => None,
        }
    }

    fn values(&self) -> Vec<String> {
        match *self {
            Self::Num { from, to, width } => {
                let nums: Vec<u64> = if from <= to { (from..=to).take(MAX + 1).collect() } else { (to..=from).rev().take(MAX + 1).collect() };
                nums.into_iter().map(|n| format!("{n:0width$}")).collect()
            }
            Self::Letters { from, to } => {
                let v: Vec<u8> = if from <= to { (from..=to).collect() } else { (to..=from).rev().collect() };
                v.into_iter().map(|c| (c as char).to_string()).collect()
            }
        }
    }
}

/// The addresses `text` stands for: itself when it holds no group. `None` when it would make more than [`MAX`].
pub fn expand(text: &str) -> Option<Vec<String>> {
    let mut out = vec![String::new()];
    let mut rest = text;
    while !rest.is_empty() {
        let group = rest.find('[').and_then(|i| {
            let j = rest[i..].find(']')? + i;
            Some((i, j, Group::parse(&rest[i + 1..j])?))
        });
        let Some((i, j, g)) = group else {
            for s in &mut out {
                s.push_str(rest);
            }
            break;
        };
        let values = g.values();
        if out.len().saturating_mul(values.len()) > MAX {
            return None;
        }
        out = out.iter().flat_map(|s| values.iter().map(move |v| format!("{s}{}{v}", &rest[..i]))).collect();
        rest = &rest[j + 1..];
    }
    Some(out)
}
