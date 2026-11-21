//! The Claude Code adapter: reads `<claude data folder>/projects/<folder-slug>/<session>.jsonl` (the data folder is `~/.claude`, or
//! `CLAUDE_CONFIG_DIR`). One JSON record per line; the format is not documented, so only a few fields are looked at and every line or
//! file that does not fit is skipped. Files are big (tens of MB), so only the first and the last bytes are read, and the result is
//! kept per file until the file changes.
//!
//! What is used: `cwd` and `gitBranch` (the folder and branch of the session), `aiTitle` (the agent's own title), the first prompt
//! (`type: user` with plain text), and the last records (`assistant` with a `stop_reason`, `system` with `subtype: turn_duration`) to tell
//! a turn in progress from one that ended. The state also comes from the file's age and from the programs running now.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;

use crate::procs::Live;
use crate::{Adapter, Session, State, folder_key, ms_of, title_of, worktree_repo};

const HEAD: u64 = 96 * 1024;
const TAIL: u64 = 192 * 1024;

/// A working turn with no new record for this long is taken as waiting for the user (a permission, a question, a stall).
const STALL_SECS: i64 = 60;
/// After this long without any record an open session is idle.
const IDLE_SECS: i64 = 10 * 60;
/// Without process information, a session untouched for this long has ended.
const GONE_SECS: i64 = 30 * 60;

pub struct Claude {
    root: PathBuf,
    cache: Mutex<HashMap<PathBuf, (i64, u64, Parsed)>>,
}

impl Claude {
    pub fn new() -> Claude {
        let base = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
        Claude { root: base.unwrap_or_default().join("projects"), cache: Mutex::new(HashMap::new()) }
    }

    /// For tests: another projects folder.
    pub fn at(root: PathBuf) -> Claude {
        Claude { root, cache: Mutex::new(HashMap::new()) }
    }
}

impl Default for Claude {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// A prompt, a tool call or a tool result is the last thing: the agent is (or should be) at work.
    Working,
    /// The agent's turn ended.
    Done,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Parsed {
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub ai_title: Option<String>,
    pub first_prompt: Option<String>,
    pub phase: Phase,
}

#[derive(Deserialize)]
struct Rec {
    #[serde(rename = "type")]
    kind: Option<String>,
    subtype: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    #[serde(rename = "aiTitle")]
    ai_title: Option<String>,
    message: Option<MsgOrOther>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MsgOrOther {
    Msg(Msg),
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct Msg {
    stop_reason: Option<String>,
    content: Option<Content>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Parts(Vec<Part>),
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct Part {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

impl Content {
    /// The text of a prompt a person wrote: plain text, or the first text part. Not what the program wraps around (`<command-...>`,
    /// `<system-reminder>`, a tool's result).
    fn prompt(&self) -> Option<&str> {
        let t = match self {
            Content::Text(t) => t.as_str(),
            Content::Parts(ps) => ps.iter().find(|p| p.kind.as_deref() == Some("text")).and_then(|p| p.text.as_deref())?,
            Content::Other(_) => return None,
        };
        let t = t.trim_start();
        (!t.is_empty() && !t.starts_with('<') && !t.starts_with("[Request interrupted") && !t.starts_with("Caveat:")).then_some(t)
    }

    fn interrupted(&self) -> bool {
        matches!(self, Content::Text(t) if t.trim_start().starts_with("[Request interrupted"))
            || matches!(self, Content::Parts(ps) if ps.iter().any(|p| p.text.as_deref().is_some_and(|t| t.trim_start().starts_with("[Request interrupted"))))
    }
}

/// Reads the records of the first and last bytes of one file's text. Lines that do not parse are skipped.
pub(crate) fn parse(head: &str, tail: &str) -> Parsed {
    let mut out = Parsed { cwd: None, branch: None, ai_title: None, first_prompt: None, phase: Phase::Unknown };
    let mut head_cwd = None;
    let mut head_branch = None;
    for line in head.lines() {
        let Ok(r) = serde_json::from_str::<Rec>(line) else { continue };
        if head_cwd.is_none() {
            head_cwd = r.cwd.clone();
        }
        if head_branch.is_none() {
            head_branch = r.git_branch.clone().filter(|b| !b.is_empty());
        }
        if out.ai_title.is_none() {
            out.ai_title = r.ai_title.clone().filter(|t| !t.trim().is_empty());
        }
        if out.first_prompt.is_none() && r.kind.as_deref() == Some("user") && r.is_meta != Some(true) && r.is_sidechain != Some(true) {
            if let Some(MsgOrOther::Msg(Msg { content: Some(c), .. })) = &r.message {
                out.first_prompt = c.prompt().map(title_of).filter(|t| !t.is_empty());
            }
        }
    }
    for line in tail.lines() {
        let Ok(r) = serde_json::from_str::<Rec>(line) else { continue };
        if r.cwd.is_some() {
            out.cwd = r.cwd.clone();
        }
        if let Some(b) = r.git_branch.clone().filter(|b| !b.is_empty()) {
            out.branch = Some(b);
        }
        if let Some(t) = r.ai_title.clone().filter(|t| !t.trim().is_empty()) {
            out.ai_title = Some(t);
        }
        if r.is_sidechain == Some(true) {
            continue;
        }
        match r.kind.as_deref() {
            Some("user") if r.is_meta != Some(true) => {
                let done = matches!(&r.message, Some(MsgOrOther::Msg(Msg { content: Some(c), .. })) if c.interrupted());
                out.phase = if done { Phase::Done } else { Phase::Working };
            }
            Some("assistant") => {
                let stop = match &r.message {
                    Some(MsgOrOther::Msg(m)) => m.stop_reason.as_deref(),
                    _ => None,
                };
                out.phase = match stop {
                    Some("end_turn" | "stop_sequence" | "max_tokens" | "refusal") => Phase::Done,
                    _ => Phase::Working,
                };
            }
            Some("system") if r.subtype.as_deref() == Some("turn_duration") => out.phase = Phase::Done,
            _ => {}
        }
    }
    out.cwd = out.cwd.or(head_cwd);
    out.branch = out.branch.or(head_branch);
    out
}

/// The state without the running programs' say: from the last records and the age in seconds.
pub(crate) fn state_by_file(phase: Phase, age_secs: i64) -> State {
    match phase {
        Phase::Working if age_secs < STALL_SECS => State::Running,
        Phase::Working | Phase::Done if age_secs < IDLE_SECS => State::Waiting,
        _ => State::Idle,
    }
}

fn read_window(path: &Path, len: u64) -> Option<(String, String)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; HEAD.min(len) as usize];
    f.read_exact(&mut buf).ok()?;
    let mut head = String::from_utf8_lossy(&buf).into_owned();
    if len > HEAD {
        // the last line is cut
        if let Some(i) = head.rfind('\n') {
            head.truncate(i);
        }
    }
    let start = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(TAIL).read_to_end(&mut buf).ok()?;
    let mut tail = String::from_utf8_lossy(&buf).into_owned();
    if start > 0 {
        // the first line is cut
        match tail.find('\n') {
            Some(i) => tail.drain(..=i).for_each(drop),
            None => tail.clear(),
        }
    }
    Some((head, tail))
}

impl Adapter for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }

    fn is_agent_process(&self, name: &str, args: &[String]) -> bool {
        match name {
            "claude" => true,
            // the program run by node or bun: a script of the claude package
            "node" | "bun" => args.iter().skip(1).any(|a| {
                let a = a.replace('\\', "/").to_lowercase();
                a.contains("/claude-code/") || a.contains("/@anthropic-ai/claude")
            }),
            _ => false,
        }
    }

    fn sessions(&self, live: &Live, max_age: Duration, limit: usize) -> Vec<Session> {
        let now_ms = ms_of(std::time::SystemTime::now());
        let min_ms = now_ms - max_age.as_millis() as i64;
        // the files, newest first (a stat each, no reading)
        let mut files: Vec<(i64, u64, PathBuf)> = Vec::new();
        let Ok(dirs) = std::fs::read_dir(&self.root) else { return Vec::new() };
        for d in dirs.flatten() {
            let Ok(rd) = std::fs::read_dir(d.path()) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "jsonl") {
                    continue;
                }
                let Ok(m) = e.metadata() else { continue };
                if !m.is_file() || m.len() == 0 {
                    continue;
                }
                let Some(mt) = m.modified().ok().map(ms_of).filter(|t| *t >= min_ms) else { continue };
                files.push((mt, m.len(), p));
            }
        }
        files.sort_by(|a, b| b.0.cmp(&a.0));
        files.truncate(limit);

        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut fresh: HashMap<PathBuf, (i64, u64, Parsed)> = HashMap::new();
        let mut found: Vec<(Session, Phase)> = Vec::new();
        for (mt, len, path) in files {
            let parsed = match cache.remove(&path) {
                Some((cm, cl, p)) if cm == mt && cl == len => p,
                _ => match read_window(&path, len) {
                    Some((h, t)) => parse(&h, &t),
                    None => continue,
                },
            };
            fresh.insert(path.clone(), (mt, len, parsed.clone()));
            let Some(cwd) = parsed.cwd.as_deref().filter(|c| !c.is_empty()) else { continue };
            let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
            let folder = PathBuf::from(cwd);
            let title = parsed.ai_title.as_deref().map(title_of).filter(|t| !t.is_empty()).or(parsed.first_prompt.clone()).unwrap_or_default();
            found.push((
                Session {
                    agent: "claude",
                    id,
                    repo: worktree_repo(&folder),
                    folder,
                    branch: parsed.branch.clone(),
                    state: State::Ended,
                    last_activity_ms: mt,
                    title,
                },
                parsed.phase,
            ));
        }
        *cache = fresh;
        drop(cache);

        // found is newest first. Of the sessions in one folder, as many as programs run there are open: the newest ones.
        let mut open: HashMap<String, usize> = HashMap::new();
        let mut out = Vec::with_capacity(found.len());
        for (mut s, phase) in found {
            let age = (now_ms - s.last_activity_ms) / 1000;
            let key = folder_key(&s.folder);
            let is_open = if live.available {
                let used = open.entry(key).or_insert(0);
                let room = *used < live.count("claude", &s.folder);
                if room {
                    *used += 1;
                }
                room
            } else {
                age < GONE_SECS
            };
            s.state = if is_open { state_by_file(phase, age) } else { State::Ended };
            out.push(s);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // a made-up sample in the shape of the format (no real content)
    const HEAD: &str = concat!(
        "{\"type\":\"mode\",\"mode\":\"x\",\"sessionId\":\"s1\"}\n",
        "not json at all\n",
        "{\"type\":\"user\",\"isMeta\":true,\"cwd\":\"C:\\\\proj\\\\app\",\"gitBranch\":\"main\",\"message\":{\"role\":\"user\",\"content\":\"<system-reminder>skip</system-reminder>\"}}\n",
        "{\"type\":\"user\",\"cwd\":\"C:\\\\proj\\\\app\",\"gitBranch\":\"main\",\"message\":{\"role\":\"user\",\"content\":\"  \\nAdd a sample feature to the sample app and write the notes\"}}\n",
        "{\"type\":\"future-kind\",\"anything\":[1,2,3]}\n",
    );
    const TAIL: &str = concat!(
        "{\"type\":\"ai-title\",\"aiTitle\":\"Sample title\",\"sessionId\":\"s1\"}\n",
        "{\"type\":\"assistant\",\"cwd\":\"C:\\\\proj\\\\app\",\"gitBranch\":\"feature\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"tool_use\",\"content\":[{\"type\":\"tool_use\"}]}}\n",
        "{\"type\":\"user\",\"cwd\":\"C:\\\\proj\\\\app\",\"gitBranch\":\"feature\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\"}]}}\n",
        "{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"stop_reason\":\"end_turn\",\"content\":[]}}\n",
        "{\"type\":\"assistant\",\"cwd\":\"C:\\\\proj\\\\app\",\"gitBranch\":\"feature\",\"message\":{\"role\":\"assistant\",\"stop_reason\":\"end_turn\",\"content\":[{\"type\":\"text\"}]}}\n",
        "{\"type\":\"system\",\"subtype\":\"turn_duration\",\"cwd\":\"C:\\\\proj\\\\app\"}\n",
    );

    #[test]
    fn the_folder_branch_title_and_phase_come_from_a_made_up_session_and_odd_lines_are_skipped() {
        let p = parse(HEAD, TAIL);
        assert_eq!(p.cwd.as_deref(), Some("C:\\proj\\app"));
        assert_eq!(p.branch.as_deref(), Some("feature"), "the last record's branch wins");
        assert_eq!(p.ai_title.as_deref(), Some("Sample title"));
        assert_eq!(p.first_prompt.as_deref(), Some("Add a sample feature to the sample app and write the notes"));
        assert_eq!(p.phase, Phase::Done);

        // a turn in progress: the last record is a tool result
        let working = parse(HEAD, TAIL.lines().take(3).collect::<Vec<_>>().join("\n").as_str());
        assert_eq!(working.phase, Phase::Working);
        // nothing readable at all
        assert_eq!(parse("", "{").phase, Phase::Unknown);
        assert!(parse("", "{").cwd.is_none());
    }
}
