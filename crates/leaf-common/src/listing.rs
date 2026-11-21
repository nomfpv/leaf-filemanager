//! The error of a folder listing, the same on every system. The texts are the lookup keys of the translated messages.

#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error("Folder not found")]
    NotFound,
    #[error("Access denied")]
    AccessDenied,
    #[error("Invalid folder name")]
    InvalidName,
    #[error("The drive is not ready")]
    NotReady,
    #[error("Network path not found")]
    NetworkPath,
    /// The system's own error code (a Win32 error or an errno), for the message only.
    #[error("Listing error (code {0})")]
    Os(u32),
    #[error("Cancelled")]
    Cancelled,
    #[error("Not available on this system yet")]
    Unsupported,
}
