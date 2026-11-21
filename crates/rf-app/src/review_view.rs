//! Review mode (the "Review" side of the change feed page, command "Review changes"; behind the agent layer switch): every changed
//! file of the folder's repository in one list on the left with its state and +/- counts, the chosen file's difference on the right,
//! inline or side by side, hunk by hunk. Outside a repository the comparison is with a checkpoint (the newest, or an older one chosen
//! with the "Since" button).
//!
//! Per file and per hunk: Accept (in a repository: staged; elsewhere: marked as reviewed for this session) and Revert (a file: the
//! "Revert this file" path, the current content to the Trash first; a hunk: `leaf_dev::review::revert_hunk`, refused when the file
//! changed since the difference was shown, the previous content to the Trash first). Commit commits what is staged with the message
//! typed, nothing added to it; leaf never pushes. Keys: j / k next and previous hunk, a accept, r revert (asked once per session),
//! Enter opens the file. All git and file work runs off the UI thread (`leaf_dev::review`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::assets::IconName as L;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use leaf_dev::checkpoint::{self, Checkpoint, Plan, Store};
use leaf_dev::gitread::{self, Kind, LineKind};
use leaf_dev::review::{self, RDiff, RHunk, Stamp};
use rf_core::format;

use crate::t;
use crate::theme::{self, Palette};

/// Rows of a diff drawn before "Show all" is offered.
const CAP_ROWS: usize = 1500;
const ROW_H: f32 = 18.;
/// "r" asked once per session; after a yes it reverts straight away.
static REVERT_ASKED: AtomicBool = AtomicBool::new(false);

/// The agent layer is on (the master switch of everything AI-workflow: Settings, Developer).
pub fn enabled(cx: &App) -> bool {
    cx.try_global::<crate::settings::Settings>().is_some_and(|s| s.ai_layer)
}

/// The folder whose change feed page should open on its Review side (set just before the page is opened).
static WANT: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Whether the page being made for `dir` opens on the Review side (asked once).
pub fn take_want(dir: &Path) -> bool {
    let Ok(mut g) = WANT.lock() else { return false };
    if g.as_deref() == Some(dir) {
        *g = None;
        return true;
    }
    false
}

/// "Review changes": the Review side of `dir`'s change feed page, on the tab itself when it shows that page, else in a new tab.
pub fn show(dir: PathBuf, tab: WeakEntity<crate::folder_view::FolderView>, cx: &mut App) {
    let _ = tab.update(cx, |v, cx| {
        if matches!(v.location(), rf_core::Location::Changes(p) if *p == dir)
            && let Some(cv) = v.changes_view()
        {
            cv.update(cx, |cv, cx| cv.show_review(cx));
            return;
        }
        if let Ok(mut g) = WANT.lock() {
            *g = Some(dir.clone());
        }
        cx.emit(crate::folder_view::FolderEvent::OpenInNewTab(rf_core::Location::Changes(dir)));
    });
}

#[derive(Clone)]
struct FileItem {
    rel: String,
    from: Option<String>,
    path: PathBuf,
    kind: Kind,
    untracked: bool,
    /// Everything of it is staged (git), or it was marked reviewed (checkpoint).
    accepted: bool,
    /// Something of it is staged.
    staged: bool,
    counts: Option<(u32, u32)>,
}

enum Source {
    Loading,
    Git(PathBuf),
    Cp { store: Arc<Store>, list: Vec<Checkpoint>, at: usize, plan: Arc<Plan> },
    /// Neither a repository nor a checkpoint to compare with: the sentence to show.
    Nothing(String),
}

/// The chosen file's difference as it was read.
struct Shown {
    rel: String,
    diff: RDiff,
    /// The work file as the diff saw it (a hunk revert checks it).
    stamp: Option<Stamp>,
    /// The diff is what is staged (the file is accepted): nothing to accept or revert by hunk.
    staged_view: bool,
    /// A picture: before and after (either can be missing).
    picture: Option<(Option<PathBuf>, Option<PathBuf>)>,
}

#[derive(Clone)]
enum DRow {
    Hunk(usize),
    Line { old: Option<u32>, new: Option<u32>, kind: LineKind, text: SharedString, hunk: usize },
    Pair { left: Option<(u32, LineKind, SharedString)>, right: Option<(u32, LineKind, SharedString)>, hunk: usize },
    Note(SharedString),
    ShowAll(usize),
}

pub struct ReviewView {
    dir: PathBuf,
    src: Source,
    files: Vec<FileItem>,
    selected: Option<String>,
    shown: Option<Shown>,
    rows: std::rc::Rc<Vec<DRow>>,
    /// Row of each hunk header.
    hunk_rows: Vec<usize>,
    hunk: usize,
    side_by_side: bool,
    show_all: bool,
    /// A partly staged file: its staged part is shown instead of what is not staged yet.
    view_staged: bool,
    /// Checkpoint mode: files and hunks (file, hunk header) marked reviewed.
    reviewed: HashSet<String>,
    reviewed_hunks: HashSet<(String, String)>,
    message: Entity<InputState>,
    /// The last thing to say: (is an error, text, details).
    note: Option<(bool, String, String)>,
    busy: bool,
    hover: Option<usize>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    _list: Option<Task<()>>,
    _diff: Option<Task<()>>,
    _act: Option<Task<()>>,
}

fn is_picture(rel: &str) -> bool {
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "svg")
}

/// Bytes to a file under the temp folder named by their content (so a picture shown is never a stale cached one).
fn to_temp(bytes: &[u8], rel: &str) -> Option<PathBuf> {
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
    let dir = std::env::temp_dir().join("leaf-review");
    std::fs::create_dir_all(&dir).ok()?;
    let s = Stamp::of_bytes(bytes);
    let p = dir.join(format!("{:016x}{:x}.{ext}", s.hash, s.len));
    if !p.exists() {
        std::fs::write(&p, bytes).ok()?;
    }
    Some(p)
}

fn line_count(path: &Path) -> Option<u32> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(1024 * 1024).read_to_end(&mut buf).ok()?;
    if buf.iter().take(8192).any(|&b| b == 0) {
        return Some(0);
    }
    let n = buf.iter().filter(|&&b| b == b'\n').count() + usize::from(buf.last().is_some_and(|&b| b != b'\n'));
    Some(n as u32)
}

fn load_git(root: &Path) -> Option<Vec<FileItem>> {
    let changes = gitread::status(root)?;
    let un = review::numstat(root, false).unwrap_or_default();
    let st = review::numstat(root, true).unwrap_or_default();
    let mut out: Vec<FileItem> = changes
        .into_iter()
        .map(|c| {
            let path = root.join(&c.path);
            let u = un.get(&c.path).copied();
            let s = st.get(&c.path).copied();
            let counts = if c.untracked { line_count(&path).map(|n| (n, 0)) } else { u.or(s) };
            let accepted = !c.untracked && c.kind != Kind::Conflicted && u.is_none() && c.staged;
            FileItem { rel: c.path, from: c.from, path, kind: c.kind, untracked: c.untracked, accepted, staged: c.staged, counts }
        })
        .collect();
    out.sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
    Some(out)
}

fn cp_items(store: &Store, plan: &Plan, reviewed: &HashSet<String>) -> Vec<FileItem> {
    plan.items
        .iter()
        .map(|i| {
            let kind = match i.change {
                checkpoint::Change::Changed => Kind::Changed,
                checkpoint::Change::Back => Kind::Deleted,
                checkpoint::Change::Added => Kind::New,
            };
            FileItem {
                rel: i.rel.clone(),
                from: None,
                path: store.folder().join(&i.rel),
                kind,
                untracked: false,
                accepted: reviewed.contains(&i.rel),
                staged: false,
                counts: None,
            }
        })
        .collect()
}

impl ReviewView {
    pub fn new(dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| InputState::new(window, cx).placeholder(t!("Commit message")));
        cx.observe(&message, |_, _, cx| cx.notify()).detach();
        let mut this = Self {
            dir,
            src: Source::Loading,
            files: Vec::new(),
            selected: None,
            shown: None,
            rows: Default::default(),
            hunk_rows: Vec::new(),
            hunk: 0,
            side_by_side: true,
            show_all: false,
            view_staged: false,
            reviewed: HashSet::new(),
            reviewed_hunks: HashSet::new(),
            message,
            note: None,
            busy: false,
            hover: None,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            _list: None,
            _diff: None,
            _act: None,
        };
        this.refresh(cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    fn root(&self) -> Option<PathBuf> {
        if let Source::Git(r) = &self.src { Some(r.clone()) } else { None }
    }

    /// The status line: "7 files to review, 2 accepted".
    pub fn status_text(&self) -> String {
        if self.files.is_empty() {
            return String::new();
        }
        let acc = self.files.iter().filter(|f| f.accepted).count();
        format!("{} {}, {} {}", self.files.len(), if self.files.len() == 1 { t!("file to review") } else { t!("files to review") }, acc, t!("accepted"))
    }

    /// Reads the list again (and then the chosen file's difference), off the UI thread.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let dir = self.dir.clone();
        // (a checkpoint already chosen stays chosen)
        let keep = match &self.src {
            Source::Cp { store, list, at, .. } => list.get(*at).cloned().map(|cp| (store.clone(), cp)),
            _ => None,
        };
        let work = cx.background_spawn(async move {
            if let Some(root) = gitread::repo_root(&dir) {
                return match load_git(&root) {
                    Some(files) => Ok((Source::Git(root), files)),
                    None => Err(t!("git could not be run for this repository.").to_string()),
                };
            }
            let store = match &keep {
                Some((s, _)) => s.clone(),
                None => Arc::new(crate::checkpoint_ui::store_of(&dir).map_err(|e| crate::checkpoint_ui::error_text(&e))?),
            };
            let list = checkpoint::list(&store).map_err(|e| crate::checkpoint_ui::error_text(&e))?;
            if list.is_empty() {
                return Err(t!("This folder is not in a git repository and has no checkpoint yet: take one (command Checkpoint), and the review shows what changed since.").to_string());
            }
            let at = keep.and_then(|(_, cp)| list.iter().position(|c| c.id == cp.id)).unwrap_or(0);
            let plan = Arc::new(checkpoint::plan(&store, &list[at]).map_err(|e| crate::checkpoint_ui::error_text(&e))?);
            Ok((Source::Cp { store, list, at, plan }, Vec::new()))
        });
        self._list = Some(cx.spawn(async move |this, cx| {
            let got = work.await;
            let _ = this.update(cx, |v, cx| {
                match got {
                    Ok((src, files)) => {
                        let files = match &src {
                            Source::Cp { store, plan, .. } => {
                                let mut f = cp_items(store, plan, &v.reviewed);
                                // (counts once known stay)
                                for it in &mut f {
                                    it.counts = v.files.iter().find(|o| o.rel == it.rel).and_then(|o| o.counts);
                                }
                                f
                            }
                            _ => files,
                        };
                        v.src = src;
                        v.files = files;
                    }
                    Err(e) => {
                        v.src = Source::Nothing(e);
                        v.files.clear();
                    }
                }
                if v.selected.as_ref().is_some_and(|s| !v.files.iter().any(|f| &f.rel == s)) {
                    v.selected = None;
                }
                if v.selected.is_none() {
                    v.selected = v.files.iter().find(|f| !f.accepted).or(v.files.first()).map(|f| f.rel.clone());
                }
                v.load_diff(cx);
                cx.notify();
            });
        }));
    }

    fn select(&mut self, rel: String, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(rel.as_str()) {
            self.selected = Some(rel);
            self.hunk = 0;
            self.show_all = false;
            self.view_staged = false;
            self.load_diff(cx);
        }
        cx.notify();
    }

    fn set_view_staged(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.view_staged != on {
            self.view_staged = on;
            self.hunk = 0;
            self.shown = None;
            self.rebuild();
            self.load_diff(cx);
        }
        cx.notify();
    }

    fn load_diff(&mut self, cx: &mut Context<Self>) {
        let Some(rel) = self.selected.clone() else {
            self.shown = None;
            self.rebuild();
            return;
        };
        let Some(item) = self.files.iter().find(|f| f.rel == rel).cloned() else { return };
        enum How {
            Git(PathBuf),
            Cp(Arc<Store>, Arc<Plan>),
        }
        let how = match &self.src {
            Source::Git(r) => How::Git(r.clone()),
            Source::Cp { store, plan, .. } => How::Cp(store.clone(), plan.clone()),
            _ => return,
        };
        // (a fully accepted file shows its staged diff anyway; a partly staged one when asked)
        let view_staged = self.view_staged && !item.accepted;
        let work = cx.background_spawn(async move {
            let stamp = Stamp::of_file(&item.path);
            let pic = is_picture(&rel);
            match how {
                How::Git(root) => {
                    let (diff, staged_view) = if item.untracked {
                        (review::untracked(&item.path).unwrap_or_default(), false)
                    } else if view_staged && item.staged {
                        (review::staged(&root, &rel, item.from.as_deref()).unwrap_or_default(), true)
                    } else {
                        let un = review::unstaged(&root, &rel).unwrap_or_default();
                        if un.hunks.is_empty() && !un.binary && item.staged {
                            (review::staged(&root, &rel, item.from.as_deref()).unwrap_or_default(), true)
                        } else {
                            (un, false)
                        }
                    };
                    let picture = pic.then(|| {
                        let before = if item.untracked { None } else { review::blob(&root, &format!("{}:{}", if staged_view { "HEAD" } else { "" }, item.from.as_deref().unwrap_or(&rel))) };
                        (before.and_then(|b| to_temp(&b, &rel)), item.path.is_file().then(|| item.path.clone()))
                    });
                    Shown { rel, diff, stamp, staged_view, picture }
                }
                How::Cp(store, plan) => {
                    let diff = checkpoint::diff(&store, &plan, &rel, crate::diff_view::CONTEXT).map(|d| review::from_file_diff(d, true)).unwrap_or_default();
                    let picture = pic.then(|| (checkpoint::saved_file(&plan, &rel), item.path.is_file().then(|| item.path.clone())));
                    Shown { rel, diff, stamp, staged_view: false, picture }
                }
            }
        });
        self._diff = Some(cx.spawn(async move |this, cx| {
            let shown = work.await;
            let _ = this.update(cx, |v, cx| {
                if v.selected.as_deref() != Some(shown.rel.as_str()) {
                    return;
                }
                if let Some(f) = v.files.iter_mut().find(|f| f.rel == shown.rel)
                    && f.counts.is_none()
                {
                    f.counts = Some((shown.diff.added, shown.diff.removed));
                }
                v.shown = Some(shown);
                v.rebuild();
                cx.notify();
            });
        }));
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let mut hunk_rows = Vec::new();
        if let Some(s) = &self.shown {
            let d = &s.diff;
            if d.binary && s.picture.is_none() {
                rows.push(DRow::Note(t!("A binary file: no lines to show.").into()));
            }
            for (hi, h) in d.hunks.iter().enumerate() {
                hunk_rows.push(rows.len());
                rows.push(DRow::Hunk(hi));
                let text = |t: &str| -> SharedString { t.replace('\t', "    ").into() };
                if self.side_by_side {
                    let mut i = 0;
                    while i < h.lines.len() {
                        let l = &h.lines[i];
                        if l.kind == LineKind::Context {
                            let t = text(&l.text);
                            rows.push(DRow::Pair { left: Some((l.old_no.unwrap_or(0), LineKind::Context, t.clone())), right: Some((l.new_no.unwrap_or(0), LineKind::Context, t)), hunk: hi });
                            i += 1;
                            continue;
                        }
                        let mut rem = Vec::new();
                        while i < h.lines.len() && h.lines[i].kind == LineKind::Removed {
                            rem.push(&h.lines[i]);
                            i += 1;
                        }
                        let mut add = Vec::new();
                        while i < h.lines.len() && h.lines[i].kind == LineKind::Added {
                            add.push(&h.lines[i]);
                            i += 1;
                        }
                        for k in 0..rem.len().max(add.len()) {
                            rows.push(DRow::Pair {
                                left: rem.get(k).map(|l| (l.old_no.unwrap_or(0), LineKind::Removed, text(&l.text))),
                                right: add.get(k).map(|l| (l.new_no.unwrap_or(0), LineKind::Added, text(&l.text))),
                                hunk: hi,
                            });
                        }
                    }
                } else {
                    for l in &h.lines {
                        rows.push(DRow::Line { old: l.old_no, new: l.new_no, kind: l.kind, text: text(&l.text), hunk: hi });
                    }
                }
            }
            if d.truncated {
                rows.push(DRow::Note(t!("The rest of this diff is not shown: it is too long.").into()));
            }
            if !self.show_all && rows.len() > CAP_ROWS {
                let hidden = rows.len() - CAP_ROWS;
                rows.truncate(CAP_ROWS);
                hunk_rows.retain(|&r| r < CAP_ROWS);
                rows.push(DRow::ShowAll(hidden));
            }
        }
        self.hunk = self.hunk.min(hunk_rows.len().saturating_sub(1));
        self.hunk_rows = hunk_rows;
        self.rows = std::rc::Rc::new(rows);
    }

    fn set_side_by_side(&mut self, on: bool, cx: &mut Context<Self>) {
        self.side_by_side = on;
        self.rebuild();
        cx.notify();
    }

    fn go_hunk(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.hunk_rows.len();
        if n == 0 {
            // (no hunk left in this file: on to the next file)
            self.next_file(delta, cx);
            return;
        }
        let next = self.hunk as isize + delta;
        if next < 0 || next >= n as isize {
            self.next_file(delta, cx);
            return;
        }
        self.hunk = next as usize;
        self.scroll.scroll_to_item(self.hunk_rows[self.hunk], ScrollStrategy::Top);
        cx.notify();
    }

    fn next_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        let at = self.selected.as_ref().and_then(|s| self.files.iter().position(|f| &f.rel == s));
        let next = match at {
            Some(i) => i as isize + delta,
            None => 0,
        };
        if next >= 0
            && let Some(f) = self.files.get(next as usize)
        {
            let rel = f.rel.clone();
            self.select(rel, cx);
        }
    }

    fn say(&mut self, err: bool, text: impl Into<String>, details: impl Into<String>, cx: &mut Context<Self>) {
        self.note = Some((err, text.into(), details.into()));
        cx.notify();
    }

    /// Runs one git write off the UI thread, then reads everything again. `fail` is leaf's sentence when it goes wrong.
    fn act(&mut self, fail: &'static str, work: impl FnOnce() -> Result<(), String> + Send + 'static, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        // (the difference shown is out of date from now on: a and r wait for the new one, so the same hunk is never sent twice)
        self.shown = None;
        self.rebuild();
        let task = cx.background_spawn(async move { work() });
        self._act = Some(cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(()) => v.note = None,
                    Err(e) => v.say(true, crate::i18n::t(fail), e, cx),
                }
                v.refresh(cx);
            });
        }));
        cx.notify();
    }

    /// A revert job starts: busy until it is over, and the difference shown (with its stamp) is dropped, so a second key press
    /// cannot act on the same look. `false` when something already runs.
    fn hold(&mut self, cx: &mut Context<Self>) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        self.shown = None;
        self.rebuild();
        cx.notify();
        true
    }

    fn current_item(&self) -> Option<FileItem> {
        let s = self.selected.as_ref()?;
        self.files.iter().find(|f| &f.rel == s).cloned()
    }

    /// Accept the chosen file.
    fn accept_file(&mut self, cx: &mut Context<Self>) {
        let Some(it) = self.current_item() else { return };
        match self.root() {
            Some(root) if it.accepted => self.act("The file could not be taken out of what is staged.", move || review::unstage_file(&root, &it.rel), cx),
            Some(root) => self.act("The file could not be staged.", move || review::stage_file(&root, &it.rel), cx),
            None => {
                if !self.reviewed.remove(&it.rel) {
                    self.reviewed.insert(it.rel.clone());
                }
                let now = self.reviewed.contains(&it.rel);
                if let Some(f) = self.files.iter_mut().find(|f| f.rel == it.rel) {
                    f.accepted = now;
                }
                if now {
                    self.next_unreviewed(cx);
                }
                cx.notify();
            }
        }
    }

    fn next_unreviewed(&mut self, cx: &mut Context<Self>) {
        if let Some(f) = self.files.iter().find(|f| !f.accepted) {
            let rel = f.rel.clone();
            self.select(rel, cx);
        }
    }

    /// Accept the hunk `hi` of the chosen file (`a`).
    fn accept_hunk(&mut self, hi: usize, cx: &mut Context<Self>) {
        let (Some(it), Some(s)) = (self.current_item(), self.shown.as_ref()) else { return };
        if s.staged_view {
            return;
        }
        let Some(h) = s.diff.hunks.get(hi).cloned() else {
            // a file without hunks (binary, deleted, a mode change): accepting the hunk is accepting the file
            self.accept_file(cx);
            return;
        };
        match self.root() {
            Some(root) => {
                let whole = it.untracked || matches!(it.kind, Kind::New | Kind::Deleted | Kind::Renamed) || h.raw.is_none();
                if whole {
                    self.act("The file could not be staged.", move || review::stage_file(&root, &it.rel), cx);
                } else {
                    self.act("This change could not be staged.", move || review::stage_hunk(&root, &it.rel, &h), cx);
                }
            }
            None => {
                let key = (it.rel.clone(), h.header.clone());
                if !self.reviewed_hunks.remove(&key) {
                    self.reviewed_hunks.insert(key);
                    self.go_hunk(1, cx);
                }
                cx.notify();
            }
        }
    }

    /// Revert the chosen file (asked first).
    fn ask_revert_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(it) = self.current_item() else { return };
        let name = rf_core::history::display_name(&it.path);
        let me = cx.entity().downgrade();
        let (text, run): (String, Box<dyn Fn(&mut App)>) = match &self.src {
            Source::Git(root) => {
                if !matches!(it.kind, Kind::Changed | Kind::Deleted) || it.untracked {
                    self.say(false, t!("Only a changed or deleted file of the last commit can be reverted here. A new file can be moved to the Trash in its folder."), "", cx);
                    return;
                }
                let (root, rel) = (root.clone(), it.rel.clone());
                (
                    format!("\"{name}\" {}", t!("goes back to the last commit. Your current version is moved to the Trash first.")),
                    Box::new(move |cx: &mut App| crate::status_center::revert_file(cx, root.clone(), rel.clone())),
                )
            }
            Source::Cp { store, plan, .. } => {
                let (store, plan, rel, path) = (store.clone(), plan.clone(), it.rel.clone(), it.path.clone());
                (
                    format!("\"{name}\" {}", t!("goes back to how the checkpoint saw it. Your current version is moved to the Trash first.")),
                    Box::new(move |cx: &mut App| {
                        let (store, plan, rel, me) = (store.clone(), plan.clone(), rel.clone(), me.clone());
                        if me.update(cx, |v, cx| v.hold(cx)).ok() != Some(true) {
                            return;
                        }
                        let done: crate::status_center::OnDone = Box::new(move |_, cx| {
                            let _ = me.update(cx, |v, cx| {
                                v.busy = false;
                                v.refresh(cx);
                            });
                        });
                        crate::status_center::revert_with_trash(
                            cx,
                            path.clone(),
                            move |put_away| {
                                let trash = |ps: &[PathBuf]| ps.iter().try_for_each(|p| put_away(p));
                                checkpoint::restore_one(&store, &plan, &rel, &trash)
                            },
                            Some(done),
                        );
                    }),
                )
            }
            _ => return,
        };
        crate::prompt::show(
            t!("Revert this file?"),
            Some(text),
            None,
            vec![
                crate::prompt::Button::new(t!("Revert"), crate::prompt::Kind::Danger, move |_, _, cx| {
                    run(cx);
                    Ok(())
                }),
                crate::prompt::Button::cancel(),
            ],
            window,
            cx,
        );
    }

    /// Revert the hunk `hi` (`r`): asked the first time in a session.
    fn ask_revert_hunk(&mut self, hi: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (Some(it), Some(s)) = (self.current_item(), self.shown.as_ref()) else { return };
        if s.staged_view || s.diff.binary {
            return;
        }
        let Some(h) = s.diff.hunks.get(hi).cloned() else { return };
        let Some(stamp) = s.stamp else {
            self.say(false, t!("The file is not there: revert the whole file instead."), "", cx);
            return;
        };
        let me = cx.entity().downgrade();
        let path = it.path.clone();
        let run = move |cx: &mut App| {
            let me = me.clone();
            // (from now until the job is over and the diff is read again, nothing else is reverted or accepted with this look)
            if me.update(cx, |v, cx| v.hold(cx)).ok() != Some(true) {
                return;
            }
            let done: crate::status_center::OnDone = Box::new(move |_, cx| {
                let _ = me.update(cx, |v, cx| {
                    v.busy = false;
                    v.refresh(cx);
                });
            });
            let (p2, h) = (path.clone(), h.clone());
            crate::status_center::revert_with_trash(cx, path.clone(), move |put_away| review::revert_hunk(&p2, stamp, &h, put_away), Some(done));
        };
        if REVERT_ASKED.load(Ordering::Relaxed) {
            run(cx);
            return;
        }
        crate::prompt::show(
            t!("Revert this change?"),
            Some(t!("Its lines go back to how they were; the rest of the file stays as it is. The file's current version is moved to the Trash first. leaf does not ask again until it is restarted.").to_string()),
            None,
            vec![
                crate::prompt::Button::new(t!("Revert"), crate::prompt::Kind::Danger, move |_, _, cx| {
                    REVERT_ASKED.store(true, Ordering::Relaxed);
                    run(cx);
                    Ok(())
                }),
                crate::prompt::Button::cancel(),
            ],
            window,
            cx,
        );
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root() else { return };
        let msg = self.message.read(cx).value().to_string();
        if msg.trim().is_empty() {
            self.say(true, t!("Type a commit message first."), "", cx);
            return;
        }
        if !self.files.iter().any(|f| f.staged) {
            self.say(true, t!("Nothing is staged: accept a file or a change first."), "", cx);
            return;
        }
        if self.busy {
            return;
        }
        self.busy = true;
        let task = cx.background_spawn(async move { review::commit(&root, &msg) });
        self._act = Some(cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |v, window, cx| {
                v.busy = false;
                match r {
                    Ok(_) => {
                        v.message.update(cx, |s, cx| s.set_value("", window, cx));
                        v.say(false, t!("Committed. Nothing was pushed."), "", cx);
                    }
                    Err(e) => v.say(true, t!("The commit was not made."), e, cx),
                }
                v.refresh(cx);
            });
        }));
        cx.notify();
    }

    fn open_file(&mut self, cx: &mut Context<Self>) {
        let Some(it) = self.current_item() else { return };
        if let Err(e) = leaf_sys::openrules::open(&it.path) {
            self.say(true, t!("The file could not be opened."), format!("{e:#}"), cx);
        }
    }

    /// Checkpoint mode: compare with the next older checkpoint (after the oldest: the newest again).
    fn next_checkpoint(&mut self, cx: &mut Context<Self>) {
        let Source::Cp { store, list, at, .. } = &self.src else { return };
        if list.len() < 2 {
            return;
        }
        let (store, cp, list, at) = (store.clone(), list[(*at + 1) % list.len()].clone(), list.clone(), (*at + 1) % list.len());
        let work = cx.background_spawn(async move { checkpoint::plan(&store, &cp).map(|p| (store, p)) });
        self._list = Some(cx.spawn(async move |this, cx| {
            let got = work.await;
            let _ = this.update(cx, |v, cx| {
                match got {
                    Ok((store, plan)) => {
                        let plan = Arc::new(plan);
                        v.reviewed.clear();
                        v.reviewed_hunks.clear();
                        v.files = cp_items(&store, &plan, &v.reviewed);
                        v.src = Source::Cp { store, list, at, plan };
                        v.selected = v.files.first().map(|f| f.rel.clone());
                        v.shown = None;
                        v.load_diff(cx);
                    }
                    Err(e) => v.say(true, crate::checkpoint_ui::error_text(&e), "", cx),
                }
                cx.notify();
            });
        }));
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // (typing in the commit box is not a command)
        if !self.focus.is_focused(window) {
            return;
        }
        let m = &ev.keystroke.modifiers;
        if m.control || m.alt || m.platform || m.function {
            return;
        }
        match ev.keystroke.key.as_str() {
            "j" | "down" if !m.shift => self.go_hunk(1, cx),
            "k" | "up" if !m.shift => self.go_hunk(-1, cx),
            "a" if !m.shift => self.accept_hunk(self.hunk, cx),
            "r" if !m.shift => self.ask_revert_hunk(self.hunk, window, cx),
            "enter" => self.open_file(cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

fn kind_color(k: Kind, p: &Palette) -> Hsla {
    match k {
        Kind::New => p.success,
        Kind::Changed => p.warning,
        Kind::Deleted | Kind::Conflicted => p.danger,
        Kind::Renamed => p.accent,
    }
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, p: &Palette) -> Stateful<Div> {
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

fn small(id: impl Into<ElementId>, label: impl Into<SharedString>, p: &Palette) -> Stateful<Div> {
    div().id(id).px_1p5().rounded(theme::RADIUS_SM).text_xs().whitespace_nowrap().cursor_pointer().text_color(p.text_secondary).hover(|s| s.bg(p.pressed).text_color(p.text)).child(label.into())
}

fn seg(id: &'static str, label: &'static str, on: bool, p: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .text_xs()
        .whitespace_nowrap()
        .cursor_pointer()
        .rounded(theme::RADIUS_SM)
        .when(on, |d| d.bg(p.selection).text_color(p.text))
        .when(!on, |d| d.text_color(p.text_secondary).hover(|s| s.bg(p.hover)))
        .child(label)
}

impl ReviewView {
    /// The page below the change feed's top bar (`switch` is its Changes / Checkpoints / Review switch).
    pub fn render_page(&mut self, switch: Div, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let summary = match &self.src {
            Source::Loading => t!("Looking at the changes…").to_string(),
            Source::Nothing(e) => e.clone(),
            Source::Git(_) if self.files.is_empty() => t!("Nothing to review: the files match the last commit.").to_string(),
            Source::Cp { .. } if self.files.is_empty() => t!("Nothing to review: the files are as the checkpoint saw them.").to_string(),
            _ => self.status_text(),
        };
        let since = match &self.src {
            Source::Cp { list, at, .. } => list.get(*at).map(|c| format!("{} {}", t!("Since"), format::datetime(c.time_ms))),
            _ => None,
        };
        let top = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(p.divider)
            .child(Icon::new(L::GitCompare).size(px(16.)).text_color(p.accent))
            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(summary))
            .when_some(since, |d, s| d.child(button("review-since", s, p).on_click(cx.listener(|v, _, _, cx| v.next_checkpoint(cx)))))
            .child(switch)
            .child(button("review-refresh", t!("Refresh"), p).on_click(cx.listener(|v, _, _, cx| v.refresh(cx))));
        let mut page = v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|v, ev: &KeyDownEvent, window, cx| v.on_key(ev, window, cx)))
            .child(top);
        if let Some((err, text, details)) = self.note.clone() {
            page = page.child(
                h_flex()
                    .px_3()
                    .py_1p5()
                    .gap_2()
                    .items_start()
                    .border_b_1()
                    .border_color(p.divider)
                    .child(Icon::new(if err { L::CircleAlert } else { L::Info }).size(px(14.)).text_color(if err { p.danger } else { p.text_secondary }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .child(div().text_color(p.text).whitespace_normal().child(text))
                            .when(!details.is_empty(), |d| d.child(div().text_color(p.text_tertiary).whitespace_normal().child(details))),
                    )
                    .child(small("review-note-close", t!("Close"), p).on_click(cx.listener(|v, _, _, cx| {
                        v.note = None;
                        cx.notify();
                    }))),
            );
        }
        let list = self.render_list(cx);
        let right = self.render_diff(p, cx);
        page = page.child(
            h_flex()
                .flex_1()
                .min_h_0()
                .items_start()
                .child(div().w(px(300.)).h_full().flex_shrink_0().border_r_1().border_color(p.divider).child(list))
                .child(div().flex_1().min_w_0().h_full().child(right))
                .on_mouse_down(MouseButton::Left, cx.listener(|v, _, window, cx| v.focus(window, cx))),
        );
        if self.root().is_some() {
            page = page.child(self.render_commit(p, cx));
        }
        page.into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.files.is_empty() {
            return div().into_any_element();
        }
        uniform_list(
            "review-files",
            self.files.len(),
            cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                let p = Palette::for_window(window);
                range.map(|i| this.render_file(i, &p, cx)).collect::<Vec<_>>()
            }),
        )
        .size_full()
        .p_1()
        .into_any_element()
    }

    fn render_file(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let f = &self.files[i];
        let on = self.selected.as_deref() == Some(f.rel.as_str());
        let hov = self.hover == Some(i);
        let (dir, name) = f.rel.rsplit_once('/').map_or((String::new(), f.rel.clone()), |(d, n)| (d.replace('/', "\\"), n.to_string()));
        let color = kind_color(f.kind, p);
        let rel = f.rel.clone();
        h_flex()
            .id(("review-file", i))
            .w_full()
            .h(px(36.))
            .px_2()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .rounded(theme::RADIUS_SM)
            .cursor_pointer()
            .when(hov && !on, |d| d.bg(p.hover))
            .when(on, |d| d.bg(p.selection))
            .on_hover(cx.listener(move |v, h: &bool, _, cx| {
                let want = h.then_some(i);
                if *h && v.hover != want || !*h && v.hover == Some(i) {
                    v.hover = want;
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |v, _, window, cx| {
                v.focus(window, cx);
                v.select(rel.clone(), cx);
            }))
            .child(div().w(px(14.)).flex_shrink_0().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(color).child(f.kind.letter(f.untracked).to_string()))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().text_color(p.text).child(crate::rtl::display_text(&name)))
                    .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_tertiary).child({
                        let place = if dir.is_empty() { crate::i18n::t(f.kind.label()).to_string() } else { dir };
                        if f.staged && !f.accepted && !f.untracked && self.root().is_some() { format!("{place} \u{00b7} {}", t!("partly staged")) } else { place }
                    })),
            )
            .when_some(f.counts, |d, (a, r)| {
                d.child(
                    h_flex()
                        .gap_1()
                        .flex_shrink_0()
                        .text_xs()
                        .child(div().text_color(p.success).child(format!("+{a}")))
                        .child(div().text_color(p.danger).child(format!("\u{2212}{r}"))),
                )
            })
            .child(div().w(px(16.)).flex_shrink_0().when(f.accepted, |d| d.child(Icon::new(L::Check).size(px(14.)).text_color(p.success))))
            .into_any_element()
    }

    fn render_diff(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(it) = self.current_item() else {
            return div().size_full().into_any_element();
        };
        let git = self.root().is_some();
        let staged_view = self.shown.as_ref().is_some_and(|s| s.staged_view);
        let accept_label = match (git, it.accepted) {
            (true, true) => t!("Unstage"),
            (true, false) => t!("Accept (stage)"),
            (false, true) => t!("Not reviewed"),
            (false, false) => t!("Mark reviewed"),
        };
        let partly = git && it.staged && !it.accepted && !it.untracked;
        let state = if it.accepted {
            if git { t!("Accepted: staged for the commit") } else { t!("Reviewed") }
        } else if partly {
            t!("Partly staged: the staged part goes into the commit")
        } else {
            crate::i18n::t(it.kind.label())
        };
        let head = h_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .border_b_1()
            .border_color(p.divider)
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(p.text).child(it.rel.replace('/', "\\")))
            .child(div().flex_shrink_0().text_xs().text_color(if it.accepted { p.success } else { kind_color(it.kind, p) }).child(state))
            .child(div().flex_1())
            .when(partly, |d| {
                d.child(
                    h_flex()
                        .gap_0p5()
                        .flex_shrink_0()
                        .child(seg("review-unstaged", t!("Not staged"), !self.view_staged, p).on_click(cx.listener(|v, _, _, cx| v.set_view_staged(false, cx))))
                        .child(seg("review-staged", t!("Staged"), self.view_staged, p).on_click(cx.listener(|v, _, _, cx| v.set_view_staged(true, cx)))),
                )
            })
            .child(
                h_flex()
                    .gap_0p5()
                    .flex_shrink_0()
                    .child(seg("review-inline", t!("Inline"), !self.side_by_side, p).on_click(cx.listener(|v, _, _, cx| v.set_side_by_side(false, cx))))
                    .child(seg("review-sbs", t!("Side by side"), self.side_by_side, p).on_click(cx.listener(|v, _, _, cx| v.set_side_by_side(true, cx)))),
            )
            .child(button("review-accept-file", accept_label, p).when(self.busy, |d| d.opacity(0.5)).on_click(cx.listener(|v, _, _, cx| v.accept_file(cx))))
            .child(button("review-revert-file", t!("Revert file…"), p).on_click(cx.listener(|v, _, window, cx| v.ask_revert_file(window, cx))))
            .child(button("review-open", t!("Open"), p).on_click(cx.listener(|v, _, _, cx| v.open_file(cx))));
        let body: AnyElement = match &self.shown {
            None => div().flex_1().flex().items_center().justify_center().text_xs().text_color(p.text_tertiary).child(t!("Loading…")).into_any_element(),
            Some(s) if s.rel != it.rel => div().flex_1().flex().items_center().justify_center().text_xs().text_color(p.text_tertiary).child(t!("Loading…")).into_any_element(),
            Some(s) if s.picture.is_some() && (s.diff.binary || s.diff.hunks.is_empty()) => {
                let (before, after) = s.picture.clone().unwrap_or_default();
                let side = |label: &'static str, pic: Option<PathBuf>| {
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .gap_1()
                        .p_2()
                        .child(div().text_xs().text_color(p.text_secondary).child(label))
                        .child(match pic {
                            Some(pth) => div().flex_1().min_h_0().child(img(pth).size_full().object_fit(ObjectFit::Contain)).into_any_element(),
                            None => div().flex_1().flex().items_center().justify_center().text_xs().text_color(p.text_tertiary).child(t!("(none)")).into_any_element(),
                        })
                };
                h_flex().flex_1().min_h_0().gap_2().child(side(t!("Before"), before)).child(side(t!("After"), after)).into_any_element()
            }
            Some(s) if s.diff.hunks.is_empty() && !s.diff.binary => {
                div().flex_1().flex().items_center().justify_center().text_xs().text_color(p.text_tertiary).child(t!("No line changes (only the file's mode or name changed).")).into_any_element()
            }
            Some(s) => self.render_rows(s, staged_view, git, p, cx),
        };
        v_flex().size_full().child(head).child(body).into_any_element()
    }

    fn render_rows(&self, s: &Shown, staged_view: bool, git: bool, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.rows.clone();
        let count = rows.len();
        let [c_add, c_rem, c_hunk] = p.diff;
        let top = rows
            .iter()
            .filter_map(|r| match r {
                DRow::Line { old, new, .. } => (*old).max(*new),
                DRow::Pair { left, right, .. } => left.as_ref().map(|l| l.0).max(right.as_ref().map(|r| r.0)),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let num_w = (top.to_string().len().max(2) as f32) * 7. + 8.;
        let headers: Vec<SharedString> = s.diff.hunks.iter().map(|h| SharedString::from(h.header.clone())).collect();
        let reviewed: Vec<bool> = s.diff.hunks.iter().map(|h| self.reviewed_hunks.contains(&(s.rel.clone(), h.header.clone()))).collect();
        let complete: Vec<bool> = s.diff.hunks.iter().map(|h: &RHunk| h.complete).collect();
        let exact: Vec<bool> = s.diff.hunks.iter().map(|h| h.exact).collect();
        let actionable = !staged_view && !s.diff.binary;
        let can_revert = actionable && s.stamp.is_some();
        let sel = self.hunk;
        let me = cx.entity().downgrade();
        let p = *p;
        let list = uniform_list("review-rows", count, move |range: std::ops::Range<usize>, _window, _cx| {
            let num = move |n: Option<u32>| div().w(px(num_w)).flex_shrink_0().px_1().text_right().text_color(p.text_tertiary).child(n.map(|n| n.to_string()).unwrap_or_default());
            let text_cell = |t: SharedString| div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(p.text).child(t);
            range
                .map(|i| match &rows[i] {
                    DRow::Hunk(hi) => {
                        let hi = *hi;
                        let on = hi == sel;
                        let (m1, m2, m3) = (me.clone(), me.clone(), me.clone());
                        h_flex()
                            .id(("review-row", i))
                            .h(px(ROW_H))
                            .w_full()
                            .px_2()
                            .gap_2()
                            .items_center()
                            .bg(c_hunk)
                            .when(on, |d| d.border_l_2().border_color(p.accent))
                            .cursor_pointer()
                            .on_click(move |_, _, cx| {
                                let _ = m1.update(cx, |v, cx| {
                                    v.hunk = hi;
                                    cx.notify();
                                });
                            })
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(p.text_secondary).child(headers[hi].clone()))
                            .when(reviewed[hi], |d| d.child(div().text_color(p.success).child(t!("reviewed"))))
                            .when(actionable && complete[hi], |d| {
                                d.child(small(("review-hunk-accept", hi), if git { t!("Accept") } else if reviewed[hi] { t!("Not reviewed") } else { t!("Mark reviewed") }, &p).on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    let _ = m2.update(cx, |v, cx| {
                                        v.hunk = hi;
                                        v.accept_hunk(hi, cx);
                                    });
                                }))
                            })
                            .when(can_revert && complete[hi] && exact[hi], |d| {
                                d.child(small(("review-hunk-revert", hi), t!("Revert"), &p).on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    let _ = m3.update(cx, |v, cx| {
                                        v.hunk = hi;
                                        v.ask_revert_hunk(hi, window, cx);
                                    });
                                }))
                            })
                            .into_any_element()
                    }
                    DRow::Note(n) => div().id(("review-row", i)).h(px(ROW_H)).w_full().px_2().flex().items_center().text_color(p.text_tertiary).child(n.clone()).into_any_element(),
                    DRow::ShowAll(hidden) => {
                        let m = me.clone();
                        h_flex()
                            .id(("review-row", i))
                            .h(px(ROW_H))
                            .w_full()
                            .px_2()
                            .gap_2()
                            .items_center()
                            .text_color(p.text_tertiary)
                            .child(format!("{hidden} {}", t!("more lines are not shown.")))
                            .child(small("review-show-all", t!("Show all"), &p).on_click(move |_, _, cx| {
                                let _ = m.update(cx, |v, cx| {
                                    v.show_all = true;
                                    v.rebuild();
                                    cx.notify();
                                });
                            }))
                            .into_any_element()
                    }
                    DRow::Line { old, new, kind, text, hunk } => {
                        let (bg, sign) = match kind {
                            LineKind::Added => (Some(c_add), "+"),
                            LineKind::Removed => (Some(c_rem), "\u{2212}"),
                            LineKind::Context => (None, " "),
                        };
                        h_flex()
                            .id(("review-row", i))
                            .h(px(ROW_H))
                            .w_full()
                            .items_center()
                            .when_some(bg, |d, c| d.bg(c))
                            .when(*hunk == sel, |d| d.border_l_2().border_color(p.accent))
                            .child(num(*old))
                            .child(num(*new))
                            .child(div().w(px(12.)).flex_shrink_0().text_color(p.text_secondary).child(sign))
                            .child(text_cell(text.clone()))
                            .into_any_element()
                    }
                    DRow::Pair { left, right, hunk } => {
                        let half = |cell: &Option<(u32, LineKind, SharedString)>| {
                            let bg = match cell.as_ref().map(|c| c.1) {
                                Some(LineKind::Added) => Some(c_add),
                                Some(LineKind::Removed) => Some(c_rem),
                                Some(LineKind::Context) => None,
                                None => Some(p.hover),
                            };
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .items_center()
                                .overflow_hidden()
                                .when_some(bg, |d, c| d.bg(c))
                                .child(num(cell.as_ref().map(|c| c.0)))
                                .child(text_cell(cell.as_ref().map(|c| c.2.clone()).unwrap_or_default()))
                        };
                        h_flex()
                            .id(("review-row", i))
                            .h(px(ROW_H))
                            .w_full()
                            .when(*hunk == sel, |d| d.border_l_2().border_color(p.accent))
                            .child(half(left))
                            .child(div().w(px(1.)).h_full().flex_shrink_0().bg(p.divider))
                            .child(half(right))
                            .into_any_element()
                    }
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.scroll)
        .flex_1()
        .min_h_0()
        .text_xs()
        .font_family(crate::platform::fonts::mono());
        div().flex_1().min_h_0().flex().flex_col().bg(p.tile_bg).child(list).into_any_element()
    }

    fn render_commit(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let names: Vec<&str> = self.files.iter().filter(|f| f.staged).map(|f| f.rel.rsplit('/').next().unwrap_or(&f.rel)).collect();
        let staged = names.len();
        let line = match staged {
            0 => t!("Nothing staged yet").to_string(),
            n if n <= 3 => format!("{} {}", t!("Staged:"), names.join(", ")),
            n => format!("{} {} {} {}", t!("Staged:"), names[..2].join(", "), t!("and"), format!("{} {}", n - 2, t!("more"))),
        };
        h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(p.divider)
            .child(div().flex_shrink_0().max_w(px(320.)).overflow_hidden().text_ellipsis().whitespace_nowrap().text_xs().text_color(p.text_secondary).child(line))
            .child(div().flex_1().min_w_0().child(Input::new(&self.message)))
            .child(
                button("review-commit", t!("Commit"), p)
                    .bg(p.accent)
                    .text_color(p.accent_text)
                    .border_color(p.accent)
                    .when(self.busy || staged == 0, |d| d.opacity(0.5))
                    .on_click(cx.listener(|v, _, window, cx| v.commit(window, cx))),
            )
            .into_any_element()
    }
}
