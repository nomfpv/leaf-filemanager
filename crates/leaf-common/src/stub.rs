//! Stubs: what a backend answers for something it does not do yet.
//!
//! `stub!("m::f")` gives the type's default value (`None`, empty, 0, `false`, nothing); `stub_err!("m::f")` gives
//! `Err(Unsupported)`, converted into the function's error type. Each is both the marker and the counter: with `LEAF_STUBS=1` every
//! stub that runs is logged once per call. A data operation (copy, move, delete, rename, write) never answers success from a stub.

use std::sync::atomic::{AtomicU8, Ordering};

/// The error of something this system cannot do yet. The text is the lookup key of the translated message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Not available on this system yet")]
pub struct Unsupported(pub &'static str);

impl From<Unsupported> for std::io::Error {
    fn from(u: Unsupported) -> Self {
        std::io::Error::new(std::io::ErrorKind::Unsupported, u)
    }
}

impl From<Unsupported> for crate::listing::ListError {
    fn from(_: Unsupported) -> Self {
        Self::Unsupported
    }
}

/// 0: not read yet, 1: off, 2: on.
static TRACE: AtomicU8 = AtomicU8::new(0);

fn trace_on() -> bool {
    match TRACE.load(Ordering::Relaxed) {
        0 => {
            let on = std::env::var_os("LEAF_STUBS").is_some_and(|v| v == "1");
            TRACE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
        v => v == 2,
    }
}

/// Called by every stub as it runs: logs `name` when `LEAF_STUBS=1`.
pub fn hit(name: &'static str) {
    if trace_on() {
        log::info!("stub: {name}");
    }
}

/// The default value of the function's return type, after [`hit`].
#[macro_export]
macro_rules! stub {
    ($n:literal) => {{
        $crate::stub::hit($n);
        ::core::default::Default::default()
    }};
}

/// `Err(Unsupported)` converted into the function's error type, after [`hit`].
#[macro_export]
macro_rules! stub_err {
    ($n:literal) => {{
        $crate::stub::hit($n);
        Err($crate::stub::Unsupported($n).into())
    }};
}

#[cfg(test)]
mod tests {
    use super::Unsupported;
    use crate::listing::ListError;

    fn opt() -> Option<u32> {
        crate::stub!("t::opt")
    }
    fn list() -> Vec<String> {
        crate::stub!("t::list")
    }
    fn unit() {
        crate::stub!("t::unit")
    }
    fn any() -> anyhow::Result<u32> {
        crate::stub_err!("t::any")
    }
    fn io() -> std::io::Result<()> {
        crate::stub_err!("t::io")
    }
    fn listing() -> Result<usize, ListError> {
        crate::stub_err!("t::listing")
    }

    #[test]
    fn stubs_give_the_default_or_unsupported() {
        assert_eq!(opt(), None);
        assert!(list().is_empty());
        unit();
        let e = any().unwrap_err();
        assert_eq!(e.downcast_ref::<Unsupported>(), Some(&Unsupported("t::any")));
        assert_eq!(e.to_string(), "Not available on this system yet");
        assert_eq!(io().unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        assert!(matches!(listing(), Err(ListError::Unsupported)));
        assert_eq!(ListError::Unsupported.to_string(), "Not available on this system yet");
    }
}
