//! Sync jobs in the app (B3): the "Sync jobs" page (`Location::SyncJobs`: the saved jobs with their last run, Run / Preview /
//! Edit / Delete, Purge of the kept files in `.leaf-sync-trash`), the job editor (also opened by "Sync with…" with the other pane as the right side),
//! the plan in a compare-style list where each row can be skipped or turned, and the run as ONE Status Center job (progress,
//! pause, cancel) with a report at the end. The engine is `leaf_sync` (plan first, nothing deleted for good); jobs are kept in
//! `sync.toml` in the config folder, the state of the last run in leaf's data folder.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_sync::{Action, CompareBy, Counts, Job, Jobs, Mode, Plan, Report, RunKind, Schedule};
use leaf_vfs::{Fs, LocalFs, VPath};
use rf_core::format;

use crate::i18n::tx;
use crate::t;
use crate::theme::{self, Palette};

// ------------------------------------------------------------------------------------------------------------------------ store

struct Store(Jobs);
impl Global for Store {}

/// The saved jobs (read from the config folder at the first use).
pub fn jobs(cx: &mut App) -> Vec<Job> {
    if !cx.has_global::<Store>() {
        let jobs = crate::settings::app_dir().map(|d| Jobs::load(&d)).unwrap_or_default();
        cx.set_global(Store(jobs));
    }
    cx.global::<Store>().0.jobs.clone()
}

/// Changes the jobs and saves `sync.toml`.
fn change(cx: &mut App, f: impl FnOnce(&mut Jobs)) -> Result<(), String> {
    jobs(cx);
    let mut all = cx.global::<Store>().0.clone();
    f(&mut all);
    let dir = crate::settings::app_dir().ok_or_else(|| t!("There is no config folder").to_string())?;
    all.save(&dir).map_err(|e| format!("{} ({e})", t!("sync.toml could not be saved")))?;
    cx.set_global(Store(all));
    cx.refresh_windows();
    Ok(())
}

/// Where the state of the last run of each job is kept.
fn state_dir() -> PathBuf {
    crate::settings::local_data_dir().unwrap_or_else(std::env::temp_dir).join("sync-state")
}

/// A side as the user reads it: the folder's path for a local one, the address for a server.
pub fn shown_side(addr: &str) -> String {
    match VPath::parse(addr) {
        Ok(v) if v.scheme() == "file" => LocalFs::native(&v).display().to_string(),
        _ => addr.to_string(),
    }
}

/// What the user typed for a side, as an address: a local path becomes `file:///...`, a password is taken out.
fn address_of(text: &str) -> Result<String, String> {
    let text = text.trim().trim_matches('"');
    if text.is_empty() {
        return Err(t!("Enter both sides: a folder on this computer or a server address.").to_string());
    }
    if VPath::looks_like(text) {
        let a = leaf_sync::strip_secret(text);
        VPath::parse(&a).map_err(|e| format!("{} ({e})", t!("This is not a valid address")))?;
        return Ok(a);
    }
    let p = Path::new(text);
    if !p.is_absolute() {
        return Err(t!("Enter the full path of the folder (C:\\…) or a server address (sftp://…).").to_string());
    }
    Ok(LocalFs::address(p).to_string())
}

/// The address of what a tab shows, when it is a folder.
pub fn address_of_location(loc: &rf_core::Location) -> Option<String> {
    match loc {
        rf_core::Location::Path(p) => Some(LocalFs::address(p).to_string()),
        rf_core::Location::Remote(v) => Some(v.to_string()),
        _ => None,
    }
}

fn side_fs(v: &VPath) -> Result<Arc<dyn Fs>, leaf_sync::Error> {
    leaf_vfs::fs_for(v.scheme()).map_err(|e| leaf_sync::Error::new(format!("leaf cannot open {}:// places", v.scheme()), e.to_string()))
}

/// The words of a failure: leaf's sentence, then what the system said on a line of its own.
pub fn error_text(e: &leaf_sync::Error) -> String {
    if e.details.is_empty() { format!("{}.", e.message) } else { format!("{}.\n{}", e.message, e.details) }
}

fn make_plan(job: &Job, cancel: &AtomicBool) -> Result<Plan, leaf_sync::Error> {
    let (l, r) = job.sides()?;
    let (lf, rf) = (side_fs(&l)?, side_fs(&r)?);
    leaf_sync::plan(job, &*lf, &*rf, &state_dir(), &|| cancel.load(Ordering::Relaxed))
}

fn mode_text(m: Mode) -> &'static str {
    match m {
        Mode::Mirror => tx("Mirror"),
        Mode::Backup => tx("Backup"),
        Mode::TwoWay => tx("Two-way"),
    }
}

fn mode_help(m: Mode) -> &'static str {
    match m {
        Mode::Mirror => tx("The right side becomes like the left: new and changed files are copied, what the left does not have is removed on the right (to the Trash)."),
        Mode::Backup => tx("New and changed files are copied to the right. Nothing is ever removed there."),
        Mode::TwoWay => tx("Changes go both ways. A file changed on both sides is a conflict: both stay, and it is listed."),
    }
}

/// "12 to copy →, 4 to remove on the right, 1 conflict · 1.2 GB".
fn counts_text(c: &Counts) -> String {
    let mut v = Vec::new();
    if c.to_right > 0 {
        v.push(format!("{} {}", c.to_right, t!("to copy \u{2192}")));
    }
    if c.to_left > 0 {
        v.push(format!("{} {}", c.to_left, t!("to copy \u{2190}")));
    }
    if c.folders > 0 {
        v.push(format!("{} {}", c.folders, t!("folders to make")));
    }
    if c.delete_right > 0 {
        v.push(format!("{} {}", c.delete_right, t!("to remove on the right")));
    }
    if c.delete_left > 0 {
        v.push(format!("{} {}", c.delete_left, t!("to remove on the left")));
    }
    if c.conflicts > 0 {
        v.push(format!("{} {}", c.conflicts, if c.conflicts == 1 { t!("conflict") } else { t!("conflicts") }));
    }
    if c.skipped > 0 {
        v.push(format!("{} {}", c.skipped, t!("skipped")));
    }
    if v.is_empty() {
        return t!("Nothing to do: both sides are the same.").to_string();
    }
    let mut s = v.join(", ");
    if c.bytes > 0 {
        s.push_str(&format!(" \u{b7} {}", format::size(c.bytes)));
    }
    s
}

fn in_window(cx: &mut App, f: impl FnOnce(&mut Window, &mut App)) {
    if let Some(w) = cx.active_window().or_else(|| cx.windows().first().copied()) {
        let _ = w.update(cx, |_, window, cx| f(window, cx));
    }
}

// ------------------------------------------------------------------------------------------------------------------------ run

/// Asks, then runs: a plan with nothing to do runs at once (it only records the state), a plan that removes a large share of a
/// side's files asks with a warning.
fn confirm_and_run(plan: Plan, from_preview: bool, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    if plan.is_empty() {
        start_run(plan, cx);
        return;
    }
    let c = plan.counts();
    if from_preview && !plan.needs_second_ask() {
        start_run(plan, cx);
        return;
    }
    let risky = plan.needs_second_ask();
    let share = (plan.delete_share() * 100.0).round() as u32;
    let mut message = format!("{}\n{}", plan.job.name, counts_text(&c));
    if risky {
        message.push_str(&format!(
            "\n\n{} {share}% {}",
            t!("This run removes"),
            t!("of the files of one side. Removed files go to the Trash (on a server into .leaf-sync-trash), but check the preview if this is not what you expect.")
        ));
    }
    let plan = Rc::new(std::cell::RefCell::new(Some(plan)));
    let (p1, p2) = (plan.clone(), plan.clone());
    let run = Button::new(if risky { t!("Run anyway") } else { t!("Run") }, if risky { Kind::Danger } else { Kind::Primary }, move |_, _, cx| {
        if let Some(p) = p1.borrow_mut().take() {
            start_run(p, cx);
        }
        Ok(())
    });
    let mut buttons = vec![run];
    if !from_preview {
        buttons.push(Button::new(t!("Preview"), Kind::Normal, move |_, window, cx| {
            if let Some(p) = p2.borrow_mut().take() {
                show_preview_of(p, window, cx);
            }
            Ok(())
        }));
    }
    buttons.push(Button::cancel());
    crate::prompt::show(if risky { t!("This run removes many files") } else { t!("Run sync job?") }, Some(message), None, buttons, window, cx);
}

/// The jobs whose run is going (two runs of one job would write the same files and the same state file).
static RUNNING: Mutex<Vec<String>> = Mutex::new(Vec::new());

// ------------------------------------------------------------------------------------------------------------------------ schedule

/// When each job was last started by the schedule in this session (a run that was stopped does not change `last_run_ms`, and must
/// not be tried again every minute).
static TRIED: Mutex<Vec<(String, i64)>> = Mutex::new(Vec::new());

/// Starts the clock of the scheduled jobs: a look at them shortly after leaf starts (runs missed while it was closed, jobs set to
/// "when leaf starts"), then every minute. Nothing runs while leaf is closed; there is no system task.
pub fn start_scheduler(cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_secs(20)).await;
        let mut first = true;
        loop {
            let at_start = std::mem::replace(&mut first, false);
            let _ = cx.update(|cx| tick(at_start, cx));
            cx.background_executor().timer(std::time::Duration::from_secs(60)).await;
        }
    })
    .detach();
}

fn tick(at_start: bool, cx: &mut App) {
    let now = leaf_sync_now();
    let offset = i64::from(chrono::Local::now().offset().local_minus_utc());
    for job in jobs(cx) {
        if job.schedule.kind == RunKind::Manual || is_running(&job.id) {
            continue;
        }
        let tried = TRIED.lock().ok().and_then(|g| g.iter().find(|(i, _)| *i == job.id).map(|(_, t)| *t)).unwrap_or(0);
        if !job.schedule.due(job.last_run_ms.max(tried), now, offset, at_start) {
            continue;
        }
        if let Ok(mut g) = TRIED.lock() {
            g.retain(|(i, _)| *i != job.id);
            g.push((job.id.clone(), now));
        }
        scheduled_run(job, cx);
    }
}

/// One scheduled run: it never asks. A refusal (a side missing or empty, anything else the plan refuses) or a plan that would
/// need the second confirmation ends as a failed card in the Status Center, and nothing is changed.
fn scheduled_run(job: Job, cx: &mut App) {
    let name = job.name.clone();
    let work = cx.background_executor().spawn(async move {
        let (l, r) = job.sides()?;
        let (lf, rf) = (side_fs(&l)?, side_fs(&r)?);
        leaf_sync::plan_scheduled(&job, &*lf, &*rf, &state_dir(), &|| false)
    });
    cx.spawn(async move |cx| {
        let got = work.await;
        let _ = cx.update(|cx| match got {
            Ok(plan) if is_running(&plan.job.id) => {}
            Ok(plan) => start_run(plan, cx),
            Err(e) => {
                let text = error_text(&e);
                crate::status_center::start_with_id(cx, format!("{} {name}", t!("Scheduled sync stopped:")), move |_| Err(anyhow::anyhow!(text)), None);
            }
        });
    })
    .detach();
}

fn is_running(job_id: &str) -> bool {
    RUNNING.lock().is_ok_and(|g| g.iter().any(|j| j == job_id))
}

/// Takes the job off [`RUNNING`] when dropped: when the run's work ends, also by a panic, or when it never started.
struct Running(String);

impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(mut g) = RUNNING.lock() {
            g.retain(|j| *j != self.0);
        }
    }
}

fn refuse_running(cx: &mut App) {
    in_window(cx, |window, cx| {
        crate::prompt::notice(t!("This job is running"), t!("Wait until its run ends, or cancel it in the Status Center.").to_string(), window, cx)
    });
}

/// The run, as one Status Center job; the report is on its card at the end.
fn start_run(plan: Plan, cx: &mut App) {
    let name = plan.job.name.clone();
    let job_id = plan.job.id.clone();
    match RUNNING.lock() {
        Ok(mut g) if !g.contains(&job_id) => g.push(job_id.clone()),
        _ => return refuse_running(cx),
    }
    let running = Running(job_id.clone());
    let plan = Arc::new(plan);
    let out: Arc<Mutex<Option<Result<Report, leaf_sync::Error>>>> = Arc::new(Mutex::new(None));
    let id_cell = Rc::new(std::cell::Cell::new(0u64));
    let (out2, id2) = (out.clone(), id_cell.clone());
    let p2 = plan.clone();
    let id = crate::status_center::start_with_id(
        cx,
        format!("{} {name}", t!("Sync:")),
        move |tracker| {
            let _running = running;
            let r = (|| {
                let (lf, rf) = (side_fs(&p2.left_root)?, side_fs(&p2.right_root)?);
                let cancelled = || tracker.cancel.load(Ordering::Relaxed);
                let paused = || tracker.paused.load(Ordering::Relaxed);
                let mut last_row = u64::MAX;
                let mut progress = |p: &leaf_sync::Progress| {
                    tracker.bytes.set(p.bytes_done, p.bytes_total);
                    // the name only when the row changes (names always reach the card, the counts are thinned out)
                    let cur = (p.rows_done != last_row).then(|| p.current.to_string());
                    last_row = p.rows_done;
                    (tracker.on_progress)(p.rows_total.max(1) as u32, p.rows_done as u32, cur);
                };
                let trash = |paths: &[PathBuf]| crate::checkpoint_ui::to_trash(paths);
                let bin_takes = |p: &Path| bin_takes(p);
                let mut ctl =
                    leaf_sync::Control { cancelled: &cancelled, paused: &paused, progress: &mut progress, trash_local: &trash, bin_takes: &bin_takes };
                leaf_sync::run(&p2, &*lf, &*rf, &state_dir(), &mut ctl)
            })();
            let res = match &r {
                Ok(rep) => Ok(!rep.cancelled),
                Err(e) => Err(anyhow::anyhow!(error_text(e))),
            };
            if let Ok(mut g) = out2.lock() {
                *g = Some(r);
            }
            res
        },
        Some(Box::new(move |_, cx| {
            let got = out.lock().ok().and_then(|mut g| g.take());
            finish(&job_id, &name, got, id2.get(), cx);
        })),
    );
    id_cell.set(id);
}

/// Will the Trash take `p` by a move? Not on a network drive or a stick without a Recycle Bin, not when the drive's bin is off or
/// too small for it (Windows), not on a mount without a usable trash (Linux): the shell would delete for good there, or ask
/// whether to in the middle of the run. The run then keeps the file in `.leaf-sync-trash` in the job's folder.
fn bin_takes(p: &Path) -> bool {
    use leaf_sys::organize::Recycler as _;
    let tracker = Arc::new(leaf_sys::fileops::Tracker::new(Box::new(|_, _, _| {}), Arc::new(AtomicBool::new(false))));
    let bin = leaf_sys::organize::ShellRecycler { tracker };
    bin.can_recycle(p) && bin.bin_takes(p)
}

fn finish(job_id: &str, name: &str, got: Option<Result<Report, leaf_sync::Error>>, id: u64, cx: &mut App) {
    let line = match &got {
        Some(Ok(rep)) => report_line(rep),
        Some(Err(e)) => e.message.clone(),
        None => t!("The run ended unexpectedly").to_string(),
    };
    let when = leaf_sync_now();
    let _ = change(cx, |all| {
        if let Some(j) = all.jobs.iter_mut().find(|j| j.id == job_id) {
            j.last_run_ms = when;
            j.last_result = line.clone();
        }
    });
    if let Some(Ok(rep)) = got {
        let title = format!("{name}: {line}");
        let rep = Rc::new(rep);
        let name = name.to_string();
        let action: crate::status_center::ResultAction = (
            t!("Report").to_string(),
            Rc::new(move |cx: &mut App| {
                let (rep, name) = (rep.clone(), name.clone());
                in_window(cx, move |window, cx| crate::prompt::notice(format!("{} {name}", t!("Sync report:")), report_text(&rep), window, cx));
            }),
        );
        crate::status_center::set_result(cx, id, Some(title), Some(action));
    }
}

fn leaf_sync_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn report_line(rep: &Report) -> String {
    let mut v = vec![format!("{} {}", rep.copied, t!("copied"))];
    if rep.folders > 0 {
        v.push(format!("{} {}", rep.folders, t!("folders made")));
    }
    if rep.removed > 0 {
        v.push(format!("{} {}", rep.removed, t!("removed")));
    }
    if !rep.conflicts.is_empty() {
        v.push(format!("{} {}", rep.conflicts.len(), if rep.conflicts.len() == 1 { t!("conflict") } else { t!("conflicts") }));
    }
    if !rep.failed.is_empty() {
        v.push(format!("{} {}", rep.failed.len(), t!("failed")));
    }
    if rep.cancelled {
        v.push(t!("cancelled").to_string());
    }
    v.join(", ")
}

fn report_text(rep: &Report) -> String {
    const MAX: usize = 15;
    let mut s = format!("{}: {}", t!("Copied"), rep.copied);
    s.push_str(&format!(" ({})", format::size(rep.bytes)));
    if rep.verified > 0 {
        s.push_str(&format!(", {} {}", rep.verified, t!("checked by hash")));
    }
    s.push_str(&format!("\n{}: {}", t!("Removed"), rep.removed));
    if rep.removed > 0 {
        s.push_str(&format!(" ({} .leaf-sync-trash/{})", t!("to the Trash; on a server, or where the Trash would not take them, into"), rep.trash_stamp));
    }
    if rep.folders > 0 {
        s.push_str(&format!("\n{}: {}", t!("Folders made"), rep.folders));
    }
    let list = |s: &mut String, title: &str, items: Vec<String>| {
        if items.is_empty() {
            return;
        }
        s.push_str(&format!("\n\n{title} ({}):", items.len()));
        for i in items.iter().take(MAX) {
            s.push_str(&format!("\n  {i}"));
        }
        if items.len() > MAX {
            s.push_str(&format!("\n  \u{2026} {} {}", items.len() - MAX, t!("more")));
        }
    };
    list(&mut s, t!("Conflicts (both files stay as they are)"), rep.conflicts.clone());
    list(&mut s, t!("Failed"), rep.failed.iter().map(|(p, e)| format!("{p}: {e}")).collect());
    list(&mut s, t!("Folders kept (something in them stays)"), rep.kept.clone());
    if rep.cancelled {
        s.push_str(&format!("\n\n{}", t!("The run was cancelled; what was done is kept, the next run carries on.")));
    }
    s
}

// ------------------------------------------------------------------------------------------------------------------------ editor

/// The job editor. `current`: the job being edited (`None`: a new one, prefilled with `left` and `right`). `after` runs once the
/// job is saved (opens the Sync jobs page when the editor came from "Sync with…").
pub fn edit(current: Option<Job>, left: String, right: String, after: Option<Rc<dyn Fn(&mut Window, &mut App)>>, window: &mut Window, cx: &mut App) {
    let init = current.clone().unwrap_or_else(|| {
        let name_of = |a: &str| VPath::parse(a).ok().map(|v| v.name().to_string()).filter(|n| !n.is_empty()).unwrap_or_default();
        let name = match (name_of(&left), name_of(&right)) {
            (l, r) if !l.is_empty() && !r.is_empty() => format!("{l} \u{2192} {r}"),
            (l, _) => l,
        };
        Job::new(&name, &left, &right)
    });
    let name = cx.new(|cx| InputState::new(window, cx).default_value(init.name.clone()).placeholder(tx("Name")));
    let left_in = cx.new(|cx| InputState::new(window, cx).default_value(if init.left.is_empty() { String::new() } else { shown_side(&init.left) }).placeholder("C:\\Photos"));
    let right_in = cx.new(|cx| InputState::new(window, cx).default_value(if init.right.is_empty() { String::new() } else { shown_side(&init.right) }).placeholder("D:\\Backup\\Photos  or  sftp://nas/backup"));
    let include = cx.new(|cx| InputState::new(window, cx).default_value(init.include.join(", ")).placeholder("*.jpg, docs/**"));
    let exclude = cx.new(|cx| InputState::new(window, cx).default_value(init.exclude.join(", ")).placeholder("*.tmp, node_modules"));
    let mode: Entity<Mode> = cx.new(|_| init.mode);
    let flags: Entity<[bool; 5]> = cx.new(|_| [init.skip_hidden, init.gitignore, init.compare == CompareBy::Hash, init.keep_overwritten, init.verify]);
    let sched: Entity<RunKind> = cx.new(|_| init.schedule.kind);
    let every = cx.new(|cx| InputState::new(window, cx).default_value(init.schedule.hours.to_string()).placeholder("6"));
    let at = cx.new(|cx| InputState::new(window, cx).default_value(format!("{:02}:{:02}", init.schedule.minute / 60, init.schedule.minute % 60)).placeholder("03:00"));
    let error: Entity<Option<String>> = cx.new(|_| None);
    let focus_first = std::cell::Cell::new(true);
    window.open_alert_dialog(cx, move |dialog, window, cx| {
        if focus_first.replace(false) {
            name.focus_handle(cx).focus(window, cx);
        }
        let pal = Palette::for_window(window);
        let now = *mode.read(cx);
        let mut modes = h_flex().gap_1();
        for (i, m) in [Mode::Mirror, Mode::Backup, Mode::TwoWay].into_iter().enumerate() {
            let ent = mode.clone();
            modes = modes.child(
                div()
                    .id(("sync-mode", i))
                    .px_3()
                    .py_1()
                    .rounded(theme::RADIUS_SM)
                    .border_1()
                    .border_color(if now == m { pal.accent } else { pal.border })
                    .when(now == m, |d| d.bg(pal.selection))
                    .cursor_pointer()
                    .text_sm()
                    .child(mode_text(m))
                    .on_click(move |_, _, cx| {
                        ent.update(cx, |v, cx| {
                            *v = m;
                            cx.notify();
                        });
                        cx.refresh_windows();
                    }),
            );
        }
        let sched_now = *sched.read(cx);
        let mut sched_row = h_flex().gap_1().items_center();
        for (i, (k, label)) in [(RunKind::Manual, tx("Manually")), (RunKind::Hours, tx("Every N hours")), (RunKind::Daily, tx("Daily at")), (RunKind::Start, tx("When leaf starts"))].into_iter().enumerate() {
            let ent = sched.clone();
            sched_row = sched_row.child(
                div()
                    .id(("sync-sched", i))
                    .px_3()
                    .py_1()
                    .rounded(theme::RADIUS_SM)
                    .border_1()
                    .border_color(if sched_now == k { pal.accent } else { pal.border })
                    .when(sched_now == k, |d| d.bg(pal.selection))
                    .cursor_pointer()
                    .text_sm()
                    .child(label)
                    .on_click(move |_, _, cx| {
                        ent.update(cx, |v, cx| {
                            *v = k;
                            cx.notify();
                        });
                        cx.refresh_windows();
                    }),
            );
        }
        if sched_now == RunKind::Hours {
            sched_row = sched_row.child(div().w(px(70.)).child(Input::new(&every)));
        }
        if sched_now == RunKind::Daily {
            sched_row = sched_row.child(div().w(px(90.)).child(Input::new(&at)));
        }
        let f = *flags.read(cx);
        let flag = |i: usize, label: &'static str| {
            let ent = flags.clone();
            h_flex()
                .gap_2()
                .items_center()
                .child(crate::settings_view::switch(("sync-flag", i), &pal, f[i], move |_, cx| {
                    ent.update(cx, |b, cx| {
                        b[i] = !b[i];
                        cx.notify();
                    });
                    cx.refresh_windows();
                }))
                .child(div().text_sm().child(crate::i18n::t(label)))
        };
        let (name_v, left_v, right_v, inc_v, exc_v, mode_v, flags_v, err) =
            (name.clone(), left_in.clone(), right_in.clone(), include.clone(), exclude.clone(), mode.clone(), flags.clone(), error.clone());
        let (sched_v, every_v, at_v) = (sched.clone(), every.clone(), at.clone());
        let (current, init, after) = (current.clone(), init.clone(), after.clone());
        dialog
            .title(if current.is_some() { tx("Sync job") } else { tx("New sync job") })
            .bg(pal.card_bg)
            .width(px(600.))
            .show_cancel(true)
            .child(
                v_flex()
                    .gap_2()
                    .child(tx("Name"))
                    .child(Input::new(&name))
                    .child(tx("Left (the source for Mirror and Backup)"))
                    .child(Input::new(&left_in))
                    .child(tx("Right"))
                    .child(Input::new(&right_in))
                    .child(div().text_xs().text_color(pal.text_secondary).child(tx("A folder on this computer, or the address of a saved host (sftp://, ftp://). Logins stay with the host; this file never holds a password.")))
                    .child(modes)
                    .child(div().text_xs().text_color(pal.text_secondary).child(mode_help(now)))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(v_flex().gap_2().flex_1().min_w_0().child(tx("Only these files (optional)")).child(Input::new(&include)))
                            .child(v_flex().gap_2().flex_1().min_w_0().child(tx("Leave out (optional)")).child(Input::new(&exclude))),
                    )
                    .child(tx("Run"))
                    .child(sched_row)
                    .child(div().text_xs().text_color(pal.text_secondary).child(tx("Scheduled runs happen only while leaf is running; a time missed while it was closed is made up once at the next start. A scheduled run never asks: if it would need a confirmation or a side is missing, it stops and leaves a notice in the Status Center. A job is first run by hand.")))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_x_6()
                            .gap_y_2()
                            .child(flag(0, "Skip hidden items"))
                            .child(flag(1, "Follow .gitignore"))
                            .child(flag(2, "Compare by content (hash)"))
                            .child(flag(3, "Keep replaced files in the Trash"))
                            .child(flag(4, "Verify copies by hash")),
                    )
                    .children(error.read(cx).clone().map(|m| div().text_color(pal.danger).child(m))),
            )
            .on_ok(move |_, window, cx| {
                let split = |s: &str| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>();
                let fail = |m: String, cx: &mut App| {
                    err.update(cx, |e, cx| {
                        *e = Some(m);
                        cx.notify();
                    });
                    cx.refresh_windows();
                    false
                };
                let (l, r) = match (address_of(&left_v.read(cx).value()), address_of(&right_v.read(cx).value())) {
                    (Ok(l), Ok(r)) => (l, r),
                    (Err(e), _) | (_, Err(e)) => return fail(e, cx),
                };
                let f = *flags_v.read(cx);
                let mut job = init.clone();
                job.name = name_v.read(cx).value().trim().to_string();
                if job.name.is_empty() {
                    return fail(t!("Give the job a name.").to_string(), cx);
                }
                // a run that ended while the editor was open: its time and result are the stored ones, not the editor's copy
                if let Some(stored) = jobs(cx).into_iter().find(|j| j.id == job.id) {
                    job.last_run_ms = stored.last_run_ms;
                    job.last_result = stored.last_result;
                }
                let moved = job.left != l || job.right != r;
                job.left = l;
                job.right = r;
                job.mode = *mode_v.read(cx);
                job.include = split(&inc_v.read(cx).value());
                job.exclude = split(&exc_v.read(cx).value());
                job.skip_hidden = f[0];
                job.gitignore = f[1];
                job.compare = if f[2] { CompareBy::Hash } else { CompareBy::Time };
                job.keep_overwritten = f[3];
                job.verify = f[4];
                let mut schedule = Schedule { kind: *sched_v.read(cx), ..init.schedule };
                if schedule.kind == RunKind::Hours {
                    match every_v.read(cx).value().trim().parse::<u32>() {
                        Ok(h) if (1..=720).contains(&h) => schedule.hours = h,
                        _ => return fail(t!("Give the number of hours between runs (1 to 720).").to_string(), cx),
                    }
                }
                if schedule.kind == RunKind::Daily {
                    let text = at_v.read(cx).value();
                    let parsed = text.trim().split_once(':').and_then(|(h, m)| Some((h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?)));
                    match parsed {
                        Some((h, m)) if h < 24 && m < 60 => schedule.minute = h * 60 + m,
                        _ => return fail(t!("Give the time of day as HH:MM, for example 03:00.").to_string(), cx),
                    }
                }
                job.schedule = schedule;
                if let Err(e) = job.sides() {
                    return fail(error_text(&e), cx);
                }
                if (moved || job.mode != init.mode) && current.is_some() && is_running(&job.id) {
                    // the run would write the old state back over the one forgotten here
                    return fail(t!("This job is running. Wait until its run ends, then change its folders or mode.").to_string(), cx);
                }
                if (moved || job.mode != init.mode) && current.is_some() {
                    // other folders: what the last run saw there means nothing now; another mode: a Backup's state still has files
                    // the left side deleted since, which Two-way would read as "removed on the left" and remove on the right
                    leaf_sync::forget_state(&state_dir(), &job.id);
                }
                if let Err(e) = change(cx, |all| all.put(job)) {
                    return fail(e, cx);
                }
                if let Some(a) = after.as_ref() {
                    a(window, cx);
                }
                true
            })
    });
}

// ------------------------------------------------------------------------------------------------------------------------ page

/// A plan the preview page should show next (handed over by the Run question's "Preview").
static NEXT_PREVIEW: Mutex<Option<Plan>> = Mutex::new(None);

/// A side is missing or empty although the last run saw files there. The refusal stays; for this one manual run the user may say
/// it is meant: "Run anyway" makes the plan without that check and shows it as a preview first (its Run button starts it).
fn side_gone(job: Job, e: &leaf_sync::Error, window: &mut Window, cx: &mut App) {
    use crate::prompt::{Button, Kind};
    let go = Button::new(t!("Run anyway"), Kind::Danger, move |_, window, cx| {
        let job = job.clone();
        let work = cx.background_spawn(async move {
            let (l, r) = job.sides()?;
            let (lf, rf) = (side_fs(&l)?, side_fs(&r)?);
            leaf_sync::plan_anyway(&job, &*lf, &*rf, &state_dir(), &|| false)
        });
        window
            .spawn(cx, async move |cx| {
                let got = work.await;
                let _ = cx.update(|window, cx| match got {
                    Ok(plan) => show_preview_of(plan, window, cx),
                    Err(e) => crate::prompt::notice(t!("The sync job cannot run"), error_text(&e), window, cx),
                });
            })
            .detach();
        Ok(())
    });
    let message = format!("{}

{}", error_text(e), t!("If this side is empty on purpose, Run anyway shows what the run would do first; you start it from that preview. Only this run is affected."));
    crate::prompt::show(t!("The sync job cannot run"), Some(message), None, vec![go, Button::cancel()], window, cx);
}

fn show_preview_of(plan: Plan, window: &mut Window, cx: &mut App) {
    if let Ok(mut g) = NEXT_PREVIEW.lock() {
        *g = Some(plan);
    }
    // the page picks it up when it is drawn: the one open in a tab, or a new tab
    let _ = window;
    cx.refresh_windows();
}

struct Preview {
    job: Job,
    plan: Option<Plan>,
    error: Option<String>,
    cancel: Arc<AtomicBool>,
}

pub struct SyncView {
    preview: Option<Preview>,
    /// A job whose plan is being made for Run (its row says so).
    busy: Option<String>,
    hover: Option<usize>,
    /// Preview and Run each have their own: a Preview after Run must not drop the Run's question.
    _preview_task: Option<Task<()>>,
    _run_task: Option<Task<()>>,
}

impl SyncView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        jobs(cx);
        Self { preview: None, busy: None, hover: None, _preview_task: None, _run_task: None }
    }

    pub fn status_text(&self, cx: &App) -> String {
        if let Some(Preview { plan: Some(p), .. }) = &self.preview {
            return counts_text(&p.counts());
        }
        let n = cx.try_global::<Store>().map_or(0, |s| s.0.jobs.len());
        if n == 1 { t!("1 sync job").to_string() } else { format!("{n} {}", t!("sync jobs")) }
    }

    fn preview(&mut self, job: Job, cx: &mut Context<Self>) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.preview = Some(Preview { job: job.clone(), plan: None, error: None, cancel: cancel.clone() });
        let work = cx.background_spawn(async move { make_plan(&job, &cancel) });
        self._preview_task = Some(cx.spawn(async move |this, cx| {
            let got = work.await;
            let _ = this.update(cx, |v, cx| {
                if let Some(pv) = v.preview.as_mut() {
                    match got {
                        Ok(p) => pv.plan = Some(p),
                        Err(e) => pv.error = Some(error_text(&e)),
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn close_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(pv) = self.preview.take() {
            pv.cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    /// Run: the plan first (off the UI thread), then the question.
    fn run_job(&mut self, job: Job, window: &mut Window, cx: &mut Context<Self>) {
        if is_running(&job.id) {
            return refuse_running(cx);
        }
        self.busy = Some(job.id.clone());
        let job_again = job.clone();
        let work = cx.background_spawn(async move { make_plan(&job, &AtomicBool::new(false)) });
        self._run_task = Some(cx.spawn_in(window, async move |this, cx| {
            let got = work.await;
            let _ = this.update_in(cx, |v, window, cx| {
                v.busy = None;
                match got {
                    Ok(p) => confirm_and_run(p, false, window, cx),
                    Err(e) if e.side_gone => side_gone(job_again, &e, window, cx),
                    Err(e) => crate::prompt::notice(t!("The sync job cannot run"), error_text(&e), window, cx),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn delete_job(&mut self, job: Job, window: &mut Window, cx: &mut Context<Self>) {
        use crate::prompt::{Button, Kind};
        let id = job.id.clone();
        let go = Button::new(t!("Delete job"), Kind::Danger, move |_, window, cx| {
            if let Err(e) = change(cx, |all| all.remove(&id)) {
                crate::prompt::notice(t!("The job could not be deleted"), e, window, cx);
            }
            leaf_sync::forget_state(&state_dir(), &id);
            Ok(())
        });
        crate::prompt::show(t!("Delete this sync job?"), Some(format!("{}. {}", job.name, t!("Your files are not touched."))), None, vec![go, Button::cancel()], window, cx);
    }

    fn purge_job(&mut self, job: Job, window: &mut Window, cx: &mut Context<Self>) {
        use crate::prompt::{Button, Kind};
        let go = Button::new(t!("Purge"), Kind::Danger, move |_, window, cx| {
            let job = job.clone();
            let work = cx.background_spawn(async move {
                let (l, r) = job.sides()?;
                let mut n = 0;
                for v in [l, r] {
                    if leaf_sync::purge(&*side_fs(&v)?, &v)? {
                        n += 1;
                    }
                }
                Ok::<_, leaf_sync::Error>(n)
            });
            window
                .spawn(cx, async move |cx| {
                    let got = work.await;
                    let _ = cx.update(|window, cx| match got {
                        Ok(0) => crate::prompt::notice(t!("Purge"), t!("There was nothing to purge.").to_string(), window, cx),
                        Ok(_) => crate::prompt::notice(t!("Purge"), t!("The kept files of earlier runs in .leaf-sync-trash are deleted.").to_string(), window, cx),
                        Err(e) => crate::prompt::notice(t!("Purge failed"), error_text(&e), window, cx),
                    });
                })
                .detach();
            Ok(())
        });
        crate::prompt::show(
            t!("Purge kept files?"),
            Some(t!("Deletes .leaf-sync-trash in both folders for good: the files earlier runs removed or replaced on a server, or on a drive whose Trash would not take them. Files that went to the Trash stay there.").to_string()),
            None,
            vec![go, Button::cancel()],
            window,
            cx,
        );
    }
}

fn small_button(id: impl Into<ElementId>, label: impl Into<SharedString>, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded(theme::RADIUS_SM)
        .text_xs()
        .whitespace_nowrap()
        .border_1()
        .border_color(p.border)
        .cursor_pointer()
        .text_color(p.text)
        .hover(|s| s.bg(p.hover))
        .child(label.into())
}

fn action_color(a: Action, p: &Palette) -> Hsla {
    match a {
        Action::ToRight => p.compare[1],
        Action::ToLeft => p.compare[2],
        Action::DeleteRight | Action::DeleteLeft => p.danger,
        Action::Conflict => p.warning,
        Action::Skip => p.text_tertiary,
    }
}

impl Render for SyncView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_window(window);
        if let Some(plan) = NEXT_PREVIEW.lock().ok().and_then(|mut g| g.take()) {
            self.preview = Some(Preview { job: plan.job.clone(), plan: Some(plan), error: None, cancel: Arc::new(AtomicBool::new(false)) });
        }
        if self.preview.is_some() {
            return self.render_preview(&p, cx).into_any_element();
        }
        self.render_list(&p, cx).into_any_element()
    }
}

impl SyncView {
    fn render_list(&mut self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let list = jobs(cx);
        let top = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(Icon::new(L::FolderSync).size(px(16.)).text_color(p.accent))
            .child(div().flex_1().min_w_0().text_sm().text_color(p.text).child(t!("Sync jobs")))
            .child(small_button("sync-new", t!("New job"), p).on_click(cx.listener(|_, _, window, cx| edit(None, String::new(), String::new(), None, window, cx))));
        let mut rows = v_flex().id("sync-jobs").flex_1().min_h_0().overflow_y_scroll().p_1();
        if list.is_empty() {
            rows = rows.child(
                div()
                    .px_3()
                    .py_3()
                    .text_sm()
                    .text_color(p.text_secondary)
                    .child(t!("No sync jobs yet. A job keeps two folders alike: Mirror, Backup or Two-way, on this computer or with a server. Start one with New job, or with Sync with… on a folder.")),
            );
        }
        for (i, job) in list.into_iter().enumerate() {
            let busy = self.busy.as_deref() == Some(job.id.as_str());
            let last = if job.last_run_ms > 0 {
                format!("{} {} \u{2014} {}", t!("Last run"), format::datetime(job.last_run_ms), job.last_result)
            } else {
                t!("Never run").to_string()
            };
            let (j1, j2, j3, j4, j5) = (job.clone(), job.clone(), job.clone(), job.clone(), job.clone());
            rows = rows.child(
                h_flex()
                    .id(("sync-job", i))
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .when(self.hover == Some(i), |d| d.bg(p.hover))
                    .on_hover(cx.listener(move |v, on: &bool, _, cx| {
                        let want = on.then_some(i);
                        if *on || v.hover == Some(i) {
                            v.hover = want;
                            cx.notify();
                        }
                    }))
                    .child(Icon::new(L::FolderSync).size(px(16.)).flex_shrink_0().text_color(p.text_secondary))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().text_color(p.text).truncate().child(format!("{} \u{b7} {}{}", job.name, mode_text(job.mode), if job.schedule.kind == RunKind::Manual { String::new() } else { format!(" \u{b7} {}", job.schedule.text()) })))
                            .child(div().text_xs().text_color(p.text_secondary).truncate().child(format!(
                                "{}  {}  {}",
                                shown_side(&job.left),
                                if job.mode == Mode::TwoWay { "\u{2194}" } else { "\u{2192}" },
                                shown_side(&job.right)
                            )))
                            .child(div().text_xs().text_color(p.text_tertiary).truncate().child(if busy { t!("Comparing\u{2026}").to_string() } else { last })),
                    )
                    .child(small_button(("sync-run", i), t!("Run"), p).on_click(cx.listener(move |v, _, window, cx| v.run_job(j1.clone(), window, cx))))
                    .child(small_button(("sync-preview", i), t!("Preview"), p).on_click(cx.listener(move |v, _, _, cx| v.preview(j2.clone(), cx))))
                    .child(small_button(("sync-edit", i), t!("Edit"), p).on_click(cx.listener(move |_, _, window, cx| edit(Some(j3.clone()), String::new(), String::new(), None, window, cx))))
                    .child(small_button(("sync-purge", i), t!("Purge"), p).on_click(cx.listener(move |v, _, window, cx| v.purge_job(j5.clone(), window, cx))))
                    .child(small_button(("sync-delete", i), t!("Delete"), p).on_click(cx.listener(move |v, _, window, cx| v.delete_job(j4.clone(), window, cx)))),
            );
        }
        v_flex().size_full().child(top).child(rows)
    }

    fn render_preview(&mut self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let Some(pv) = self.preview.as_ref() else { return div() };
        let title = format!("{} \u{2014} {}", t!("Preview"), pv.job.name);
        let summary = match (&pv.plan, &pv.error) {
            (_, Some(e)) => e.clone(),
            (Some(plan), None) => counts_text(&plan.counts()),
            (None, None) => t!("Comparing both sides\u{2026}").to_string(),
        };
        let can_run = pv.plan.is_some();
        let top = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(Icon::new(L::FolderSync).size(px(16.)).text_color(p.accent))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().text_color(p.text).truncate().child(title))
                    .child(div().text_xs().text_color(if pv.error.is_some() { p.danger } else { p.text_secondary }).child(summary)),
            )
            .when(can_run, |d| {
                d.child(small_button("sync-preview-run", t!("Run"), p).on_click(cx.listener(|v, _, window, cx| {
                    if let Some(plan) = v.preview.take().and_then(|pv| pv.plan) {
                        confirm_and_run(plan, true, window, cx);
                    }
                    cx.notify();
                })))
            })
            .child(small_button("sync-preview-back", t!("Back"), p).on_click(cx.listener(|v, _, _, cx| v.close_preview(cx))));
        let mut page = v_flex().size_full().child(top);
        if let Some(plan) = &pv.plan {
            if !plan.refused.is_empty() {
                page = page.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(p.warning)
                        .child(format!("{} {} ({})", plan.refused.len(), t!("names were left out because they cannot be used here or could not be read"), plan.refused.iter().take(3).cloned().collect::<Vec<_>>().join("; "))),
                );
            }
            let n = plan.rows.len();
            page = page.child(
                div().px_3().py_1().text_xs().text_color(p.text_tertiary).child(t!("Click what a row does to change it: skip it, or copy the other way.")),
            );
            let body = uniform_list(
                "sync-plan-rows",
                n,
                cx.processor(move |this, range: std::ops::Range<usize>, window, cx| {
                    let p = Palette::for_window(window);
                    range.filter_map(|i| this.render_row(i, &p, cx)).collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .min_h_0()
            .p_1();
            page = page.child(body);
        }
        page
    }

    fn render_row(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let plan = self.preview.as_ref()?.plan.as_ref()?;
        let r = plan.rows.get(i)?;
        let size = |s: Option<leaf_sync::Info>| s.filter(|i| !i.dir && !i.link).map_or(String::new(), |i| format::size(i.size));
        let what = if r.whole && r.is_dir() { format!("{} ({} {})", r.rel, r.files, t!("files")) } else if r.is_dir() { format!("{}/", r.rel) } else { r.rel.clone() };
        let changed = r.action != r.proposed;
        let color = action_color(r.action, p);
        Some(
            h_flex()
                .id(("sync-plan-row", i))
                .w_full()
                .h(px(28.))
                .px_2()
                .gap_3()
                .items_center()
                .rounded(theme::RADIUS_SM)
                .hover(|s| s.bg(p.hover))
                .child(
                    div()
                        .id(("sync-plan-action", i))
                        .w(px(150.))
                        .flex_shrink_0()
                        .px_2()
                        .rounded(theme::RADIUS_SM)
                        .border_1()
                        .border_color(if changed { p.accent } else { p.border })
                        .text_xs()
                        .text_color(color)
                        .truncate()
                        .cursor_pointer()
                        .child(crate::i18n::t(r.action.text()))
                        .on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(plan) = v.preview.as_mut().and_then(|pv| pv.plan.as_mut()) {
                                let choices = plan.choices(i);
                                let now = plan.rows[i].action;
                                let at = choices.iter().position(|a| *a == now).map_or(0, |k| (k + 1) % choices.len().max(1));
                                if let Some(a) = choices.get(at) {
                                    plan.set_action(i, *a);
                                }
                            }
                            cx.notify();
                        })),
                )
                .child(div().flex_1().min_w_0().text_sm().text_color(p.text).truncate().child(what))
                .child(div().w(px(260.)).flex_shrink_0().text_xs().text_color(p.text_secondary).truncate().child(crate::i18n::t(r.why)))
                .child(div().w(px(70.)).flex_shrink_0().text_xs().text_color(p.text_tertiary).text_right().child(size(r.left)))
                .child(div().w(px(70.)).flex_shrink_0().text_xs().text_color(p.text_tertiary).text_right().child(size(r.right)))
                .into_any_element(),
        )
    }
}

/// "Sync with…": a new job from the folder shown, with the other pane's folder as the right side when there is one; saved, the
/// Sync jobs page opens.
pub fn sync_with(left: Option<String>, right: Option<String>, open_page: Rc<dyn Fn(&mut Window, &mut App)>, window: &mut Window, cx: &mut App) {
    let Some(left) = left else {
        crate::prompt::notice(t!("Sync with\u{2026}"), t!("Open a folder first: it becomes the left side of the job.").to_string(), window, cx);
        return;
    };
    edit(None, left, right.unwrap_or_default(), Some(open_page), window, cx);
}
