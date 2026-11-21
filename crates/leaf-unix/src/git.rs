//! Linux backend of rf_win::git. Status: STUB

use anyhow::Result;
use leaf_common::{stub, stub_err};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

/// A data operation (it changes the repository): `Err`, and `on_progress` is never called.
pub fn fetch(_root: &Path, _on_progress: &mut dyn FnMut(&str)) -> Result<()> {
    stub_err!("git::fetch")
}

// TYPE-COPY(rf-win src/git.rs:16)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileState {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Ignored,
    Conflicted,
}

impl FileState {
    pub fn label(self) -> &'static str {
        stub!("git::FileState::label")
    }
}

// TYPE-COPY(rf-win src/git.rs:52)
#[derive(Debug, Clone, Default)]
pub struct GitStatus {
    pub root: PathBuf,
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub files: HashMap<PathBuf, FileState>,
    pub dirs: HashMap<PathBuf, FileState>,
}

impl GitStatus {
    pub fn reindex(&mut self) {
        stub!("git::GitStatus::reindex")
    }
    pub fn is_clean(&self) -> bool {
        stub!("git::GitStatus::is_clean")
    }
}

pub fn repo_root(_dir: &Path) -> Option<PathBuf> {
    stub!("git::repo_root")
}

pub fn status(_repo_root: &Path) -> Result<GitStatus> {
    stub_err!("git::status")
}

pub fn state_for_child(_status: &GitStatus, _dir: &Path, _name: &str) -> Option<FileState> {
    stub!("git::state_for_child")
}

pub fn states_in(_status: &GitStatus, _dir: &Path) -> Vec<FileState> {
    stub!("git::states_in")
}

// TYPE-COPY(rf-win src/git.rs:398)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub is_remote: bool,
    pub is_head: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

pub fn branches(_root: &Path) -> Result<Vec<Branch>> {
    stub_err!("git::branches")
}

// TYPE-COPY(rf-win src/git.rs:464)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutMode {
    Normal,
    Stash,
    Discard,
}

pub fn checkout(_root: &Path, _branch: &str, _mode: CheckoutMode) -> Result<()> {
    stub_err!("git::checkout")
}

pub fn is_valid_branch_name(_name: &str) -> bool {
    stub!("git::is_valid_branch_name")
}

pub fn create_branch(_root: &Path, _name: &str, _from: Option<&str>, _checkout: bool) -> Result<()> {
    stub_err!("git::create_branch")
}

pub fn delete_branch(_root: &Path, _name: &str, _force: bool) -> Result<()> {
    stub_err!("git::delete_branch")
}

pub fn pull(_root: &Path, _on_progress: &mut dyn FnMut(&str)) -> Result<()> {
    stub_err!("git::pull")
}

pub fn push(_root: &Path, _on_progress: &mut dyn FnMut(&str)) -> Result<()> {
    stub_err!("git::push")
}

pub fn sync(_root: &Path, _on_progress: &mut dyn FnMut(&str)) -> Result<()> {
    stub_err!("git::sync")
}

pub fn remote_url(_root: &Path) -> Option<String> {
    stub!("git::remote_url")
}

pub fn connect_remote(_root: &Path, _url: &str) -> Result<()> {
    stub_err!("git::connect_remote")
}

pub fn commit_all(_root: &Path, _message: &str) -> Result<()> {
    stub_err!("git::commit_all")
}

pub fn init(_dir: &Path) -> Result<()> {
    stub_err!("git::init")
}

pub fn clone(_url: &str, _dest_parent: &Path, _on_progress: &mut dyn FnMut(&str)) -> Result<PathBuf> {
    stub_err!("git::clone")
}

// TYPE-COPY(rf-win src/git.rs:818)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub message: String,
    pub author: String,
    pub time_ms: i64,
}

pub fn last_commits_of_children(_root: &Path, _dir: &Path, _names: &[String], _max_commits: usize) -> HashMap<String, Commit> {
    stub!("git::last_commits_of_children")
}
