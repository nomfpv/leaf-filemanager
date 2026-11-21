//! Linux backend of rf_win::look::keys. Status: STUB

// TYPE-COPY(rf-win look/keys.rs:40)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookAction {
    Toggle,
    RunAndClose,
    Close,
    Reload,
    Fullscreen,
    SelectionMoved,
}
