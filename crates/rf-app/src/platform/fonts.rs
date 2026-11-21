//! The typefaces the app asks for by name. The themes and a few views name Windows' faces (Segoe UI Variable Text, Cascadia Mono);
//! elsewhere those are not installed and every lookup would fail with a warning. Off Windows a name that is not installed becomes the
//! desktop's own face: KDE's or GNOME's setting when it can be read, else fontconfig's `sans-serif` / `monospace`.

/// The face a view with no theme key wants for code and numbers.
pub(crate) fn mono() -> &'static str {
    imp::mono()
}

/// The face of the interface when nothing else is chosen.
pub(crate) fn ui() -> &'static str {
    imp::ui()
}

/// `wanted` when it is installed (or empty), else the desktop's UI face, or its monospace face for a name that is one.
pub(crate) fn resolve(wanted: &'static str) -> &'static str {
    imp::resolve(wanted)
}

/// Whether the family is installed (always true on Windows, where GPUI looks it up itself).
pub(crate) fn installed(name: &str) -> bool {
    imp::installed(name)
}

#[cfg(windows)]
mod imp {
    pub fn mono() -> &'static str {
        "Cascadia Mono"
    }
    pub fn ui() -> &'static str {
        "Segoe UI Variable Text"
    }
    pub fn resolve(wanted: &'static str) -> &'static str {
        wanted
    }
    pub fn installed(_name: &str) -> bool {
        true
    }
}

#[cfg(not(windows))]
mod imp {
    use std::collections::HashSet;
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;

    const SYSTEM_UI_FONT: &str = ".SystemUIFont";

    fn run(cmd: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(cmd).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
    }

    fn families() -> &'static HashSet<String> {
        static SET: OnceLock<HashSet<String>> = OnceLock::new();
        SET.get_or_init(|| {
            let list = run("fc-list", &[":", "family"]).unwrap_or_default();
            list.lines().flat_map(|l| l.split(',')).map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect()
        })
    }

    pub fn installed(name: &str) -> bool {
        families().contains(name)
    }

    /// "Noto Sans,10,-1,5,400,0,0,0,0,0,0,0,0,0,0,1" (KDE) -> "Noto Sans"; "Cantarell 11" (GNOME) -> "Cantarell".
    fn family_of(setting: &str) -> String {
        let s = setting.trim().trim_matches('\'');
        if let Some((f, _)) = s.split_once(',') {
            return f.trim().to_string();
        }
        let mut words: Vec<&str> = s.split_whitespace().collect();
        if words.last().is_some_and(|w| w.parse::<f32>().is_ok()) {
            words.pop();
        }
        words.join(" ")
    }

    fn kde(key: &str) -> Option<String> {
        let dir = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
        let text = std::fs::read_to_string(dir.join("kdeglobals")).ok()?;
        text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=').map(family_of))
    }

    fn gnome(key: &str) -> Option<String> {
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase();
        if !(desktop.contains("gnome") || desktop.contains("unity") || desktop.contains("cinnamon") || desktop.contains("mate")) {
            return None;
        }
        run("gsettings", &["get", "org.gnome.desktop.interface", key]).map(|s| family_of(&s))
    }

    fn fontconfig(generic: &str) -> Option<String> {
        run("fc-match", &["-f", "%{family[0]}", generic])
    }

    fn pick(candidates: [Option<String>; 3], fallback: &'static str) -> &'static str {
        let found = candidates.into_iter().flatten().find(|c| installed(c));
        match found {
            Some(f) => Box::leak(f.into_boxed_str()),
            None => fallback,
        }
    }

    pub fn ui() -> &'static str {
        static F: OnceLock<&'static str> = OnceLock::new();
        F.get_or_init(|| pick([kde("font"), gnome("font-name"), fontconfig("sans-serif")], SYSTEM_UI_FONT))
    }

    pub fn mono() -> &'static str {
        static F: OnceLock<&'static str> = OnceLock::new();
        F.get_or_init(|| pick([kde("fixed"), gnome("monospace-font-name"), fontconfig("monospace")], ui()))
    }

    fn is_mono_name(n: &str) -> bool {
        let n = n.to_ascii_lowercase();
        ["mono", "consolas", "courier", "code", "terminal"].iter().any(|k| n.contains(k))
    }

    pub fn resolve(wanted: &'static str) -> &'static str {
        if wanted.is_empty() || wanted == SYSTEM_UI_FONT || installed(wanted) {
            wanted
        } else if is_mono_name(wanted) {
            mono()
        } else {
            ui()
        }
    }
}
