//! Request headers for one download (what a browser sent with it: Cookie, Authorization, Referer, User-Agent). They are credentials:
//! the type has no `Serialize`, and `Debug` / `Display` show the names only, so a value cannot reach a log line, the part state or
//! the saved list by accident.

use std::fmt;
use std::sync::Arc;

use crate::auth::WebAuth;

#[derive(Clone, Default)]
pub struct Headers(Vec<(String, String)>, Option<Arc<WebAuth>>);

impl PartialEq for Headers {
    fn eq(&self, o: &Self) -> bool {
        self.0 == o.0 && match (&self.1, &o.1) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for Headers {}

impl Headers {
    /// The sign-in the user gave for this download (see [`WebAuth`]): sent as `Authorization`, to the address's own origin only.
    pub fn set_web_auth(&mut self, auth: WebAuth) {
        self.1 = Some(Arc::new(auth));
    }

    pub(crate) fn web_auth(&self) -> Option<&WebAuth> {
        self.1.as_deref()
    }

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
        self.0.is_empty() && self.1.is_none()
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
        self.1.is_some() || self.names().any(Self::is_credential)
    }

    /// The same without Cookie and Authorization (for a redirect to another origin: scheme, host or port). A web sign-in stays: it
    /// is tied to the origin that asked for it and is sent only there.
    pub fn without_credentials(&self) -> Headers {
        Headers(self.0.iter().filter(|(n, _)| !Self::is_credential(n)).cloned().collect(), self.1.clone())
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
        if self.1.is_some() {
            write!(f, "{}Authorization", if self.0.is_empty() { "" } else { ", " })?;
        }
        write!(f, "]")
    }
}

impl fmt::Display for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "headers: {} (names only)", self.len() + usize::from(self.1.is_some()))
    }
}
