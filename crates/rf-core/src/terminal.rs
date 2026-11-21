//! Which terminal "Open in terminal" starts. Platform-neutral: the list names the terminals of every system, each backend
//! (rf-win now, leaf-unix later) says which of them it can start and how.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Terminal {
    /// What the system would use: Windows Terminal, else Windows PowerShell.
    #[default]
    Auto,
    WindowsTerminal,
    /// PowerShell 7 (`pwsh`).
    Pwsh,
    WindowsPowerShell,
    Cmd,
    Wsl,
    GitBash,
    /// The user's own command line, with `{path}` standing for the folder.
    Custom,
    // Linux (their backend comes later)
    Konsole,
    Kitty,
    Alacritty,
    Foot,
    WezTerm,
    Ghostty,
    /// The program named by `$TERMINAL`.
    EnvTerminal,
}

impl Terminal {
    /// Every terminal, in the order a list shows them (Automatic first, the custom command last).
    pub const ALL: [Terminal; 15] = [
        Terminal::Auto,
        Terminal::WindowsTerminal,
        Terminal::Pwsh,
        Terminal::WindowsPowerShell,
        Terminal::Cmd,
        Terminal::Wsl,
        Terminal::GitBash,
        Terminal::Konsole,
        Terminal::Kitty,
        Terminal::Alacritty,
        Terminal::Foot,
        Terminal::WezTerm,
        Terminal::Ghostty,
        Terminal::EnvTerminal,
        Terminal::Custom,
    ];

    /// The name the settings list shows (English).
    pub fn label(self) -> &'static str {
        match self {
            Terminal::Auto => "Automatic",
            Terminal::WindowsTerminal => "Windows Terminal",
            Terminal::Pwsh => "PowerShell 7",
            Terminal::WindowsPowerShell => "Windows PowerShell",
            Terminal::Cmd => "Command Prompt",
            Terminal::Wsl => "WSL",
            Terminal::GitBash => "Git Bash",
            Terminal::Custom => "Custom command",
            Terminal::Konsole => "Konsole",
            Terminal::Kitty => "kitty",
            Terminal::Alacritty => "Alacritty",
            Terminal::Foot => "foot",
            Terminal::WezTerm => "WezTerm",
            Terminal::Ghostty => "Ghostty",
            Terminal::EnvTerminal => "$TERMINAL",
        }
    }
}
