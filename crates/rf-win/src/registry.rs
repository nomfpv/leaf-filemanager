//! What the app reads from the user's registry itself (moved from rf-app places.rs and quicklook.rs; Linux twin: leaf-unix
//! registry.rs, where neither exists).

/// The names of the WSL distributions registered for this user.
pub fn wsl_distributions() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(lxss) = windows_registry::CURRENT_USER.open(r"Software\Microsoft\Windows\CurrentVersion\Lxss") {
        if let Ok(keys) = lxss.keys() {
            for k in keys {
                if let Ok(d) = lxss.open(&k) {
                    if let Ok(name) = d.get_string("DistributionName") {
                        out.push(name);
                    }
                }
            }
        }
    }
    out
}

/// A Store package whose full name starts with `name` and ends with `publisher` is installed for this user (its key in the
/// package repository).
pub fn store_package_installed(name: &str, publisher: &str) -> bool {
    let Ok(packages) = windows_registry::CURRENT_USER.open(r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages")
    else {
        return false;
    };
    packages.keys().is_ok_and(|mut keys| keys.any(|k| k.starts_with(name) && k.ends_with(publisher)))
}
