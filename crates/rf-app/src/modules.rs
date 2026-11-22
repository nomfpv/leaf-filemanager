//! The modules: the parts of leaf that can be switched off as a whole (Settings > Features, and the first-run setup). One table
//! ([`MODULES`]) says what each is, what it hides and what it needs; the rest of the app asks [`enabled`] (a read of the settings
//! in memory: no file, no registry).
//!
//! A module that is off: its commands are not offered ([`of_command`], used by `registry::CommandSpec::available`), its right-click
//! rows are left out ([`of_slot`]), its sidebar section and Home widgets are not drawn ([`of_widget`]), its Settings page is one line
//! with a "Turn on" button, and it starts no background work. Its data and its settings stay as they are.
//!
//! Core file management (browsing, copying, tabs, search, the trash) is never a module.

use gpui_kit::App;
use gpui_kit::assets::IconName as L;

use crate::menu_model::Slot;
use crate::settings::Settings;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Module {
    Servers,
    Sync,
    Shares,
    Downloads,
    VideoDl,
    Usenet,
    Vpn,
    Dev,
    Agents,
    Media,
    HomeStats,
    DiskUsage,
}

impl Module {
    pub const ALL: [Module; 12] = [
        Module::Servers,
        Module::Sync,
        Module::Shares,
        Module::Downloads,
        Module::VideoDl,
        Module::Usenet,
        Module::Vpn,
        Module::Dev,
        Module::Agents,
        Module::Media,
        Module::HomeStats,
        Module::DiskUsage,
    ];

    const fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// An optional program a module can use. Found on the PATH (or where Settings says); leaf installs none of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Helper {
    Git,
    YtDlp,
    Ffmpeg,
    Rclone,
}

impl Helper {
    pub fn name(self) -> &'static str {
        match self {
            Helper::Git => "git",
            Helper::YtDlp => "yt-dlp",
            Helper::Ffmpeg => "ffmpeg",
            Helper::Rclone => "rclone",
        }
    }

    /// What it unlocks (one sentence).
    pub fn unlocks(self) -> &'static str {
        match self {
            Helper::Git => "Git marks after names, Show changes, Review changes, worktrees and lanes.",
            Helper::YtDlp => "Video downloads from sites such as YouTube.",
            Helper::Ffmpeg => "Joining the video and audio of a video download, and turning Blu-ray discs into video files.",
            Helper::Rclone => "Cloud storage (Google Drive, S3 and many more) listed beside your hosts.",
        }
    }

    /// The command that installs it, shown as text (leaf never runs it).
    pub fn install(self) -> &'static str {
        match self {
            Helper::Git => "winget install Git.Git",
            Helper::YtDlp => "winget install yt-dlp",
            Helper::Ffmpeg => "winget install ffmpeg",
            Helper::Rclone => "winget install Rclone.Rclone",
        }
    }

    /// Whether it is installed, and its version when it is known. Looks at the PATH once per helper (a few directory reads, kept).
    pub fn found(self, s: &Settings) -> (bool, Option<String>) {
        match self {
            Helper::YtDlp => {
                let (exe, version, _) = crate::videodl::status(s);
                (exe.is_some(), version)
            }
            Helper::Ffmpeg => (crate::videodl::status(s).2, None),
            Helper::Rclone => (leaf_remote::rclone::find_exe().is_some(), None),
            Helper::Git => {
                static GIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                let on_path = *GIT.get_or_init(|| {
                    let names: &[&str] = if cfg!(windows) { &["git.exe"] } else { &["git"] };
                    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| names.iter().any(|n| d.join(n).is_file())))
                });
                (on_path, None)
            }
        }
    }
}

pub struct Spec {
    pub module: Module,
    /// The key in `Settings::modules`; never changes.
    pub id: &'static str,
    pub name: &'static str,
    /// One line: what it is.
    pub desc: &'static str,
    pub icon: L,
    pub default_on: bool,
    /// What switching it off hides (shown on the Features page).
    pub hides: &'static str,
    /// Works only while this one is on as well.
    pub needs: Option<Module>,
    pub helpers: &'static [Helper],
    /// Said next to the switch when part of the module only follows the switch at the next start.
    pub restart_note: &'static str,
    /// The first-use tip: two or three short pointers. Empty: the module has no page of its own to show it on.
    pub tips: &'static [&'static str],
}

const NONE: &[&str] = &[];

pub static MODULES: &[Spec] = &[
    Spec {
        module: Module::Servers,
        id: "servers",
        name: "Servers and hosts",
        desc: "SFTP, FTP, WebDAV and SMB hosts in the sidebar and on Home, with shell access for servers that have no SFTP.",
        icon: L::Server,
        default_on: true,
        hides: "The Hosts section of the sidebar, the Hosts widget on Home, the Hosts page and the commands that work on a server.",
        needs: None,
        helpers: &[Helper::Rclone],
        restart_note: "",
        tips: &["A host is a folder on another computer: add one under Settings > Hosts.", "Open it from the sidebar; copy and paste work as with any folder.", "Bookmark a server folder to get back to it in one click."],
    },
    Spec {
        module: Module::Sync,
        id: "sync",
        name: "Sync and backup jobs",
        desc: "Keep two folders equal, or back one up, by hand or on a schedule.",
        icon: L::FolderSync,
        default_on: true,
        hides: "Sync jobs and Sync with... (commands, menu and page). No job runs by itself while it is off; the jobs are kept.",
        needs: None,
        helpers: &[],
        restart_note: "",
        tips: &["A job is a pair of folders and a rule: mirror, update or back up.", "Right-click a folder > Sync with... makes one; Preview shows what would change first.", "Give a job a schedule and it runs while leaf runs."],
    },
    Spec {
        module: Module::Shares,
        id: "shares",
        name: "Shared folders",
        desc: "Share a folder with a friend's leaf, and send files to devices nearby.",
        icon: L::Users,
        default_on: true,
        hides: "Share with a friend, Join shared folder, Shared folders, Send to device, and Settings > Nearby. Nearby and the sharing service stop.",
        needs: None,
        helpers: &[],
        restart_note: "A service that was off starts again at the next start.",
        tips: &["Nearby shows this PC to devices around you; turn it on below.", "Right-click a folder > Share with a friend... shares it with another leaf.", "What you receive lands in the inbox folder set on this page."],
    },
    Spec {
        module: Module::Downloads,
        id: "downloads",
        name: "Downloads",
        desc: "The download manager: resumable, scheduled, with a limit, and the hand-over from your browser.",
        icon: L::Download,
        default_on: true,
        hides: "Download here and Download several, the browser hand-over, the download schedule. Video downloads and Usenet need this one.",
        needs: None,
        helpers: &[],
        restart_note: "Downloads that were not finished are taken up again at the next start after turning it on.",
        tips: NONE,
    },
    Spec {
        module: Module::VideoDl,
        id: "videodl",
        name: "Video downloads",
        desc: "Download videos and audio from sites, through yt-dlp.",
        icon: L::Film,
        default_on: true,
        hides: "Video links are no longer taken by the download manager; the yt-dlp card in Settings.",
        needs: Some(Module::Downloads),
        helpers: &[Helper::YtDlp, Helper::Ffmpeg],
        restart_note: "",
        tips: NONE,
    },
    Spec {
        module: Module::Usenet,
        id: "usenet",
        name: "Usenet",
        desc: "Download NZB files from your Usenet server.",
        icon: L::Download,
        default_on: true,
        hides: "Download with leaf (NZB), Pause or resume all Usenet downloads, the Usenet servers card. Nothing is fetched while it is off.",
        needs: Some(Module::Downloads),
        helpers: &[],
        restart_note: "Turning it on again takes effect at the next start.",
        tips: NONE,
    },
    Spec {
        module: Module::Vpn,
        id: "vpn",
        name: "VPN",
        desc: "leaf's own VPN, with a kill switch, and an option to send downloads through it.",
        icon: L::ShieldCheck,
        default_on: true,
        hides: "The VPN commands and Settings > VPN. A running tunnel is closed and nothing is kept polling.",
        needs: None,
        helpers: &[],
        restart_note: "",
        tips: &["Sign in with your Mullvad account number, once, on this page.", "The kill switch blocks the traffic that should use the VPN while the tunnel is down.", "A download can be sent through the VPN: see its options."],
    },
    Spec {
        module: Module::Dev,
        id: "dev",
        name: "Developer tools",
        desc: "Git changes and marks, worktrees, the build-junk finder, copy as context and checkpoints.",
        icon: L::GitBranch,
        default_on: true,
        hides: "Show changes, Checkpoint(s), Worktrees, Find build junk, Copy as context, Show project brain, Docker, the Git pulse and Reclaimable space widgets, and the Developer tools in Settings > Advanced.",
        needs: None,
        helpers: &[Helper::Git],
        restart_note: "",
        tips: &["Show changes lists what changed in this folder's repository.", "A checkpoint is a copy to go back to, taken before something risky.", "Ctrl+Shift+P finds every developer command."],
    },
    Spec {
        module: Module::Agents,
        id: "agents",
        name: "Agent layer",
        desc: "Shows what coding agents on this PC are doing, and lets you review what they changed.",
        icon: L::Brain,
        default_on: true,
        hides: "The Agents page, lanes, Review changes, the dot on folders and tabs and the status line. Nothing of the agents is read while it is off.",
        needs: None,
        helpers: &[],
        restart_note: "",
        tips: &["This page lists the agent sessions leaf can see; leaf only reads their files.", "Review opens a session's changes: accept or revert them file by file.", "Settings > Advanced has the budget ring and the lane folder."],
    },
    Spec {
        module: Module::Media,
        id: "media",
        name: "Media tools",
        desc: "Turn Blu-ray discs into video files, and mount or eject disc images.",
        icon: L::Disc3,
        default_on: true,
        hides: "Convert to video file, Mount image, Eject image, Mount discs in drives (commands and menu rows).",
        needs: None,
        helpers: &[Helper::Ffmpeg],
        restart_note: "",
        tips: NONE,
    },
    Spec {
        module: Module::HomeStats,
        id: "homestats",
        name: "Home dashboard widgets",
        desc: "The statistics on Home: transfers, your numbers, code stats, activity calendar, age rings, growth and photo timeline.",
        icon: L::ChartPie,
        default_on: true,
        hides: "Those widgets on Home and in Customize Home. Your layout is kept.",
        needs: None,
        helpers: &[],
        restart_note: "",
        tips: NONE,
    },
    Spec {
        module: Module::DiskUsage,
        id: "diskusage",
        name: "Disk usage",
        desc: "A map of where the space under a folder or drive goes.",
        icon: L::HardDrive,
        default_on: true,
        hides: "Show disk usage in the right-click menu and the disk usage page.",
        needs: None,
        helpers: &[],
        restart_note: "",
        tips: &["Bigger blocks are bigger folders; click a block to go into it.", "Switch between the list, the treemap and the rings at the top.", "Right-click a block to open it in a new tab or show it in its folder."],
    },
];

pub fn spec(m: Module) -> &'static Spec {
    &MODULES[m as usize]
}

/// Whether the module's own switch is on (its dependency not looked at).
pub fn own_on(s: &Settings, m: Module) -> bool {
    match m {
        // the one switch of the agent layer is the old one
        Module::Agents => s.ai_layer,
        _ => s.modules.get(spec(m).id).copied().unwrap_or(spec(m).default_on),
    }
}

/// Whether the module works: its switch is on and so is the one it needs.
pub fn enabled_in(s: &Settings, m: Module) -> bool {
    own_on(s, m) && spec(m).needs.is_none_or(|n| own_on(s, n))
}

/// [`enabled_in`] for the running app. Before the settings exist everything is on.
pub fn enabled(cx: &App, m: Module) -> bool {
    cx.try_global::<Settings>().is_none_or(|s| enabled_in(s, m))
}

pub fn set(s: &mut Settings, m: Module, on: bool) {
    if m == Module::Agents {
        s.ai_layer = on;
    } else {
        s.modules.insert(spec(m).id.to_string(), on);
    }
}

/// The modules that are off, one bit each (`MenuContext` carries it; 0: everything on).
pub fn off_mask(s: &Settings) -> u32 {
    Module::ALL.iter().filter(|m| !enabled_in(s, **m)).fold(0, |acc, m| acc | m.bit())
}

pub fn is_off(mask: u32, m: Module) -> bool {
    mask & m.bit() != 0
}

/// The module a registry command belongs to (`None`: core). The one place that says it, so no command row needs editing.
pub fn of_command(id: &str) -> Option<Module> {
    Some(match id {
        "BookmarkRemoteFolder" | "FollowRemoteFile" | "MapNetworkDrive" | "DisconnectNetworkDrive" => Module::Servers,
        "SyncJobs" | "SyncWith" => Module::Sync,
        "ShareFolder" | "JoinSharedFolder" | "SharedFolders" | "SendToDevice" => Module::Shares,
        "DownloadHere" | "DownloadMany" => Module::Downloads,
        "DownloadNzb" | "UsenetPauseAll" => Module::Usenet,
        "VpnConnect" | "VpnDisconnect" => Module::Vpn,
        "ShowChanges" | "Checkpoint" | "ShowCheckpoints" | "ShowWorktrees" | "NewWorktree" | "FindBuildJunk" | "CopyAsContext" | "CopyBasketAsContext"
        | "CopyBasketAsMentions" | "ShowProjectBrain" | "OpenDocker" | "ToggleHideIgnored" => Module::Dev,
        "ReviewChanges" | "ShowAgents" | "NewLane" | "EndLane" => Module::Agents,
        "ConvertToVideo" | "MountImage" | "EjectImage" | "MountDiscs" => Module::Media,
        _ => return None,
    })
}

/// The module a right-click row belongs to.
pub fn of_slot(slot: Slot) -> Option<Module> {
    Some(match slot {
        Slot::SyncWith => Module::Sync,
        Slot::ShareFolder | Slot::Nearby => Module::Shares,
        Slot::DiskUsage => Module::DiskUsage,
        Slot::CopyAsContext | Slot::FindBuildJunk | Slot::Checkpoint | Slot::Git | Slot::HideIgnored => Module::Dev,
        Slot::ConvertVideo | Slot::MountImage | Slot::EjectImage => Module::Media,
        _ => return None,
    })
}

/// The module a Home widget belongs to.
pub fn of_widget(id: &str) -> Option<Module> {
    Some(match id {
        "hosts" => Module::Servers,
        "pulse" | "reclaim" => Module::Dev,
        "codestats" | "activity" | "agerings" | "growth" | "photos" | "transfers" | "numbers" => Module::HomeStats,
        _ => return None,
    })
}

/// The module a Settings page belongs to (its cards collapse to one line when the module is off).
pub fn of_page(page: crate::settings_view::Page) -> Option<Module> {
    use crate::settings_view::Page;
    match page {
        Page::Hosts => Some(Module::Servers),
        Page::Vpn => Some(Module::Vpn),
        Page::Nearby => Some(Module::Shares),
        _ => None,
    }
}

/// The module a page of leaf's own belongs to (for the first-use tip).
pub fn of_location(loc: &rf_core::history::Location) -> Option<Module> {
    use rf_core::history::Location as Lo;
    match loc {
        Lo::SyncJobs => Some(Module::Sync),
        Lo::Agents => Some(Module::Agents),
        Lo::DiskUsage(_) => Some(Module::DiskUsage),
        Lo::Changes(_) | Lo::BuildJunk(_) | Lo::Brain(_) => Some(Module::Dev),
        _ => None,
    }
}

/// A setting changed (`settings::update`): what a module's switch starts is started. What it stops stops itself (each timer looks at
/// the switch on its next turn; the VPN and the services are told in their own `settings_changed` / `apply`).
pub fn changed(before: &Settings, cx: &mut App) {
    let now = |m| enabled(cx, m);
    let was = |m| enabled_in(before, m);
    let (sync, downloads) = (now(Module::Sync) && !was(Module::Sync), now(Module::Downloads) && !was(Module::Downloads));
    if sync {
        crate::sync_ui::start_scheduler(cx);
    }
    if downloads {
        crate::download_schedule::start(cx);
    }
}

// ------------------------------------------------------------------------------------------------ profiles

/// What the user uses leaf for (first-run setup): choosing one sets the module switches of the next page.
pub struct Profile {
    #[allow(dead_code)]
    pub id: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub icon: L,
    /// The modules that are on; every other one is off. `None`: all.
    pub on: Option<&'static [Module]>,
}

pub static PROFILES: &[Profile] = &[
    Profile { id: "files", name: "Just files", desc: "Browse, copy, search and tidy files. Nothing else shows up.", icon: L::Folder, on: Some(&[Module::DiskUsage]) },
    Profile {
        id: "home",
        name: "Home network",
        desc: "Files on a NAS or server, backups, shared folders, downloads and discs.",
        icon: L::House,
        on: Some(&[Module::Servers, Module::Sync, Module::Shares, Module::Downloads, Module::Media, Module::DiskUsage, Module::HomeStats]),
    },
    Profile {
        id: "dev",
        name: "Developer",
        desc: "Git, worktrees, checkpoints, servers and coding agents.",
        icon: L::GitBranch,
        on: Some(&[Module::Servers, Module::Sync, Module::Dev, Module::Agents, Module::DiskUsage, Module::HomeStats]),
    },
    Profile { id: "all", name: "Everything", desc: "Every part of leaf on.", icon: L::Sparkles, on: None },
];

impl Profile {
    pub fn includes(&self, m: Module) -> bool {
        self.on.is_none_or(|l| l.contains(&m))
    }
}

/// The switches a profile stands for: every module with its state.
pub fn profile_switches(profile: &Profile) -> Vec<(Module, bool)> {
    Module::ALL.iter().map(|m| (*m, profile.includes(*m))).collect()
}

/// Whether a tip strip is due for this module: tips on, not dismissed, and the module has tips.
pub fn tip_due(s: &Settings, m: Module) -> bool {
    !s.tips_off && !spec(m).tips.is_empty() && enabled_in(s, m) && !s.tips_seen.iter().any(|t| t == spec(m).id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings file from before the modules has everything on and counts the setup as done; a start with no file shows it; a profile
    /// sets exactly its modules.
    #[test]
    fn old_files_have_every_module_on_and_a_profile_sets_its_modules() {
        let old = Settings::parse_lenient_pub("theme = \"dark\"\nshow_hidden = true").unwrap();
        assert!(old.setup_done && old.modules.is_empty());
        assert!(Module::ALL.iter().all(|m| enabled_in(&old, *m)) && off_mask(&old) == 0);
        assert!(!Settings::default().setup_done);

        let mut s = Settings::default();
        let files = PROFILES.iter().find(|p| p.id == "files").unwrap();
        for (m, on) in profile_switches(files) {
            set(&mut s, m, on);
        }
        assert!(enabled_in(&s, Module::DiskUsage) && !enabled_in(&s, Module::Servers) && !s.ai_layer);
        // Downloads off takes Video downloads and Usenet with it, whatever their own switches say
        let mut s = Settings::default();
        set(&mut s, Module::Downloads, false);
        assert!(!enabled_in(&s, Module::Usenet) && !enabled_in(&s, Module::VideoDl));
        let all = PROFILES.iter().find(|p| p.id == "all").unwrap();
        assert!(Module::ALL.iter().all(|m| all.includes(*m)));
        // every command named for a module exists in the registry
        for id in ["SyncJobs", "DownloadNzb", "VpnConnect", "ShowChanges", "ShowAgents", "MountImage", "SharedFolders", "BookmarkRemoteFolder"] {
            assert!(of_command(id).is_some() && crate::registry::all().any(|c| c.id == id), "{id}");
        }
    }
}
