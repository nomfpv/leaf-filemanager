//! Linux backend of rf_win::apps. Status: STUB

use leaf_common::{stub, stub_err};

// TYPE-COPY(rf-win src/apps.rs:16)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub name: String,
    pub id: String,
}

impl App {
    pub fn parsing(&self) -> String {
        stub!("apps::App::parsing")
    }
}

pub fn list() -> anyhow::Result<Vec<App>> {
    stub_err!("apps::list")
}

pub fn launch(_app: &App) -> anyhow::Result<()> {
    stub_err!("apps::launch")
}
