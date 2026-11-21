//! Linux backend of the leaf-sys module `registry` (Windows side: rf-win registry.rs). Status: REAL
//!
//! Both answers are about Windows things that do not exist here: no WSL distributions, no Store packages.

/// The names of the WSL distributions registered for this user: none on Linux.
pub fn wsl_distributions() -> Vec<String> {
    Vec::new()
}

/// A Store package is installed for this user: never on Linux.
pub fn store_package_installed(_name: &str, _publisher: &str) -> bool {
    false
}
