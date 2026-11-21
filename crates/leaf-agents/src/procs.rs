//! Agent programs running now and the folders they work in (sysinfo; the working folder of another process is not always readable,
//! then that process is not counted in any folder).

use std::collections::HashMap;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::{Adapter, folder_key};

/// What runs now.
#[derive(Debug, Default, Clone)]
pub struct Live {
    /// Per agent id, per folder key ([`folder_key`]): how many program instances work there.
    by_folder: HashMap<(&'static str, String), usize>,
    /// The process list could be read at all. When not, a session's state comes from its file alone.
    pub available: bool,
}

impl Live {
    /// How many instances of the agent run in this folder.
    pub fn count(&self, agent: &'static str, folder: &std::path::Path) -> usize {
        self.by_folder.get(&(agent, folder_key(folder))).copied().unwrap_or(0)
    }
}

/// Lists the programs that some adapter says is its agent. Blocks for a moment (a process list and a few working folders).
pub fn scan(adapters: &[Box<dyn Adapter>]) -> Live {
    let mut sys = System::new();
    if sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new()) == 0 {
        return Live::default();
    }
    // names first (cheap), then folders and arguments of the few that may be an agent
    let maybe: Vec<Pid> = sys
        .processes()
        .iter()
        .filter(|(_, p)| {
            let n = stem(&p.name().to_string_lossy());
            n == "node" || n == "bun" || adapters.iter().any(|a| a.is_agent_process(&n, &[]))
        })
        .map(|(pid, _)| *pid)
        .collect();
    if maybe.is_empty() {
        return Live { available: true, ..Live::default() };
    }
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&maybe),
        ProcessRefreshKind::new().with_cwd(UpdateKind::Always).with_cmd(UpdateKind::Always),
    );
    let mut found: Vec<(Pid, Option<Pid>, &'static str, String)> = Vec::new();
    for pid in &maybe {
        let Some(p) = sys.process(*pid) else { continue };
        let Some(cwd) = p.cwd() else { continue };
        let name = stem(&p.name().to_string_lossy());
        let args: Vec<String> = p.cmd().iter().take(4).map(|a| a.to_string_lossy().into_owned()).collect();
        if let Some(a) = adapters.iter().find(|a| a.is_agent_process(&name, &args)) {
            found.push((*pid, p.parent(), a.id(), folder_key(cwd)));
        }
    }
    // a program that starts helpers of its own name counts once: only those whose parent is not one of them
    let pids: Vec<Pid> = found.iter().map(|f| f.0).collect();
    let mut live = Live { available: true, ..Live::default() };
    for (_, parent, agent, folder) in &found {
        if parent.is_some_and(|p| pids.contains(&p)) {
            continue;
        }
        *live.by_folder.entry((agent, folder.clone())).or_insert(0) += 1;
    }
    live
}

/// "Claude.EXE" -> "claude".
fn stem(name: &str) -> String {
    let lower = name.to_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}
