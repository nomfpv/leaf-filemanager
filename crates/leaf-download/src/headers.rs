//! Request headers for one download (what a browser sent with it: Cookie, Authorization, Referer, User-Agent). They are credentials:
//! the type has no `Serialize`, and `Debug` / `Display` show the names only, so a value cannot reach a log line, the part state or
//! the saved list by accident.

use std::fmt;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Headers(Vec<(String, String)>);

impl Headers {
    /// Adds a header (a second one of the same name replaces the first). Names or values with a line break are dropped.
    pub fn set(&mut self, name: &str, value: &str) {
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() || value.is_empty() || name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
            return;
        }
        self.0.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self.0.push((name.to_string(), value.to_string()));
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(n, _)| n.as_str())
    }

    /// Cookie or Authorization (or a proxy's): what may be sent to the address's own host only.
    pub fn is_credential(name: &str) -> bool {
        ["cookie", "authorization", "proxy-authorization"].iter().any(|c| name.eq_ignore_ascii_case(c))
    }

    /// Holds a Cookie or an Authorization.
    pub fn has_credentials(&self) -> bool {
        self.names().any(Self::is_credential)
    }

    /// The same without Cookie and Authorization (for a redirect to another origin: scheme, host or port).
    pub fn without_credentials(&self) -> Headers {
        Headers(self.0.iter().filter(|(n, _)| !Self::is_credential(n)).cloned().collect())
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }
}

impl fmt::Debug for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Headers[")?;
        for (i, n) in self.names().enumerate() {
            write!(f, "{}{n}", if i > 0 { ", " } else { "" })?;
        }
        write!(f, "]")
    }
}

impl fmt::Display for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "headers: {} (names only)", self.len())
    }
}
