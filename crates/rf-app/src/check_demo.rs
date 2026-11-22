//! Made-up data for the look checks (`tools/TESTING.md`, `demo <what>` in a `LEAF_CHECK_SCRIPT`). Everything is a made-up name
//! (`user`, `example.com`, `192.0.2.x`) in the config folder the check runs with, nothing is dialled or downloaded, and nothing
//! here runs unless a script asks for it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::*;

use crate::settings::app_dir;

/// `demo home`: a few finished transfers, some counted numbers and two hosts, for the Home widgets.
pub(crate) fn home(cx: &mut App) {
    use rf_core::usage::Counter as C;
    use rf_core::xfer_log::{Kind, Outcome};
    let note = |kind, what: &str, from: &str, to: &str| crate::xfer_history::Note::new(kind, what.into(), from.into(), to.into());
    crate::xfer_history::record(note(Kind::Copy, "holiday-photos", "D:/Pictures", "E:/Backup"), 3_400_000_000, 41_000, Outcome::Done);
    crate::xfer_history::record(note(Kind::Download, "example-setup.exe", "https://example.com/files/example-setup.exe", "D:/Downloads"), 86_000_000, 9_000, Outcome::Done);
    crate::xfer_history::record(note(Kind::Upload, "report.pdf", "D:/Documents", "sftp://nas.example.com/docs"), 2_100_000, 1_300, Outcome::Done);
    crate::xfer_history::record(note(Kind::Move, "old-projects", "D:/Work", "E:/Archive"), 720_000_000, 18_000, Outcome::Failed);
    for (c, n) in [(C::FilesCopied, 1840), (C::FilesMoved, 312), (C::FilesDeleted, 95), (C::FilesRenamed, 240), (C::BytesCopied, 48_000_000_000), (C::Searches, 77)] {
        crate::usage::bump(c, n);
    }
    crate::hosts_ui::demo_hosts(cx);
}

fn git(dir: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    c.args(["-c", "user.name=user", "-c", "user.email=user@example.com", "-c", "commit.gpgsign=false"]).args(args).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    // (no console window: leaf has none of its own, so Windows would open one for every git started here)
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let _ = c.status();
}

/// "2026-11-22T10:11:12.000Z" for the time `secs` seconds ago.
fn iso_ago(secs: u64) -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()).saturating_sub(secs);
    let (days, rem) = (t / 86_400, t % 86_400);
    // civil date from days since 1970-01-01
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z", rem / 3600, rem / 60 % 60, rem % 60)
}

/// The made-up project folder of the agent checks (`<config>/demo/project`).
pub(crate) fn project_dir() -> Option<PathBuf> {
    app_dir().map(|d| d.join("demo").join("project"))
}

/// `demo agents`: a project with a tiny git repository (two commits, a second branch, one tracked file deleted afterwards for the
/// ghost rows) and two made-up session records of the agent in `$CLAUDE_CONFIG_DIR/projects` (only when that points at a throwaway
/// folder: the real data folder is never written to).
pub(crate) fn agents(cx: &mut App) {
    let Some(base) = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).filter(|p| p.is_absolute()) else {
        log::warn!("demo agents: CLAUDE_CONFIG_DIR is not set, nothing written");
        return;
    };
    let Some(proj) = project_dir() else { return };
    let _ = std::fs::create_dir_all(proj.join("src"));
    let _ = std::fs::write(proj.join("src/main.rs"), "fn main() {\n    println!(\"hello\");\n}\n");
    let _ = std::fs::write(proj.join("notes.txt"), "Ideas for later.\n");
    let _ = std::fs::write(proj.join("README.md"), "# Example project\n\nA made-up project for checks.\n");
    git(&proj, &["init", "-q", "-b", "main"]);
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-q", "-m", "First version"]);
    git(&proj, &["branch", "feature/settings-page"]);
    let _ = std::fs::write(proj.join("src/main.rs"), "fn main() {\n    println!(\"hello, world\");\n}\n");
    git(&proj, &["commit", "-q", "-am", "Greet the world"]);
    git(&proj, &["checkout", "-q", "-b", "feature/search"]);
    let _ = std::fs::write(proj.join("src/search.rs"), "pub fn search() {}\n");
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-q", "-m", "Start the search"]);
    // two lanes (worktrees on their own branches) for the branch map
    if let Some(demo) = proj.parent() {
        for (name, branch) in [("Search", "lane/search"), ("Greeting", "lane/greeting")] {
            let path = demo.join(format!("lane-{}", name.to_lowercase()));
            git(&proj, &["worktree", "add", "-q", "-b", branch, &path.to_string_lossy(), "main"]);
            let color = crate::lanes::next_color(cx);
            crate::lanes::add(crate::lanes::Lane { name: name.into(), repo: proj.clone(), path, branch: branch.into(), base: "main".into(), color, created_ms: 0 }, cx);
        }
    }
    // a tracked file that is gone from the folder since the last commit: the ghost row
    let _ = std::fs::remove_file(proj.join("notes.txt"));

    let slug: String = proj.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let dir = base.join("projects").join(slug);
    let _ = std::fs::create_dir_all(&dir);
    let cwd = serde_json::to_string(&*proj.to_string_lossy()).unwrap_or_default();
    let file = serde_json::to_string(&*proj.join("src/main.rs").to_string_lossy()).unwrap_or_default();
    let session = |id: &str, branch: &str, prompt: &str, ago: u64, reply: &str, out_tokens: u64| {
        let mut lines = vec![
            format!(r#"{{"type":"user","cwd":{cwd},"gitBranch":"{branch}","timestamp":"{}","message":{{"role":"user","content":"{prompt}"}}}}"#, iso_ago(ago + 600)),
            format!(
                r#"{{"type":"assistant","cwd":{cwd},"gitBranch":"{branch}","timestamp":"{}","message":{{"id":"{id}-a","role":"assistant","stop_reason":"tool_use","usage":{{"input_tokens":2400,"output_tokens":{out_tokens},"cache_creation_input_tokens":18000,"cache_read_input_tokens":52000}},"content":[{{"type":"text","text":"I will change the main file."}},{{"type":"tool_use","name":"Edit","input":{{"file_path":{file}}}}}]}}}}"#,
                iso_ago(ago + 300)
            ),
            format!(
                r#"{{"type":"assistant","cwd":{cwd},"gitBranch":"{branch}","timestamp":"{}","message":{{"id":"{id}-b","role":"assistant","stop_reason":"end_turn","usage":{{"input_tokens":900,"output_tokens":{},"cache_creation_input_tokens":4000,"cache_read_input_tokens":61000}},"content":[{{"type":"text","text":"{reply}"}}]}}}}"#,
                iso_ago(ago),
                out_tokens / 2
            ),
        ];
        lines.push(String::new());
        let path = dir.join(format!("{id}.jsonl"));
        let _ = std::fs::write(&path, lines.join("\n"));
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
            let _ = f.set_modified(SystemTime::now() - Duration::from_secs(ago));
        }
    };
    session("demo-session-one", "feature/search", "Add a search function to the example project", 7_200, "The search function is in place.", 3_100);
    session("demo-session-two", "main", "Make the greeting friendlier", 90_000, "The greeting now says hello to the world.", 1_400);
    log::info!("demo agents: {} and {}", proj.display(), dir.display());
    crate::agents::restart(cx);
}
