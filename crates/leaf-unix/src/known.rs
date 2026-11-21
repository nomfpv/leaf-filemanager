//! Linux backend of rf_win::known. Status: REAL (the XDG user folders through `dirs`)

use std::path::PathBuf;

// TYPE-COPY(rf-win known.rs:10): the same folders and titles; `path` asks XDG instead of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KnownFolder {
    Desktop,
    Downloads,
    Documents,
    Pictures,
    Music,
    Videos,
    Profile,
}

impl KnownFolder {
    pub const PINNED_DEFAULTS: [KnownFolder; 6] = [
        KnownFolder::Desktop,
        KnownFolder::Downloads,
        KnownFolder::Documents,
        KnownFolder::Pictures,
        KnownFolder::Music,
        KnownFolder::Videos,
    ];

    pub fn title(self) -> &'static str {
        match self {
            KnownFolder::Desktop => "Desktop",
            KnownFolder::Downloads => "Downloads",
            KnownFolder::Documents => "Documents",
            KnownFolder::Pictures => "Pictures",
            KnownFolder::Music => "Music",
            KnownFolder::Videos => "Videos",
            KnownFolder::Profile => "User profile",
        }
    }

    /// The folder from `user-dirs.dirs` (XDG), the home folder for `Profile`. `None` when it is not set.
    pub fn path(self) -> Option<PathBuf> {
        match self {
            KnownFolder::Desktop => dirs::desktop_dir(),
            KnownFolder::Downloads => dirs::download_dir(),
            KnownFolder::Documents => dirs::document_dir(),
            KnownFolder::Pictures => dirs::picture_dir(),
            KnownFolder::Music => dirs::audio_dir(),
            KnownFolder::Videos => dirs::video_dir(),
            KnownFolder::Profile => dirs::home_dir(),
        }
    }
}

pub fn known_folders() -> Vec<(KnownFolder, PathBuf)> {
    KnownFolder::PINNED_DEFAULTS.iter().filter_map(|k| k.path().map(|p| (*k, p))).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_profile_is_the_home_folder() {
        assert_eq!(super::KnownFolder::Profile.path(), dirs::home_dir());
        // every folder listed is one XDG named
        for (k, p) in super::known_folders() {
            assert_eq!(k.path(), Some(p));
        }
    }
}
