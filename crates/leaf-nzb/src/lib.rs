//! Usenet (NZB) downloads for leaf: the NZB file, the servers (several, by priority, with connection pools), pipelined NNTP
//! over leaf's dial seam (so the VPN rule holds for every connection), yEnc with CRC checks, parts written straight into place,
//! resume after a restart, par2 verify and repair, unpack, cleanup and the move into the category folder. Blocking threads
//! only, no async runtime; leaf's Status Center is the face (rf-app's `usenet.rs`).

pub mod dial;
pub mod engine;
pub mod job;
pub mod nntp;
pub mod nzb;
pub mod par;
pub mod post;
pub mod queue;
pub mod server;
pub mod yenc;
#[cfg(test)]
mod tests;

pub use job::{Done, Error, Host, Spec, Step, run};
pub use nzb::{Nzb, NzbFile, ParseError, Segment, parse};
pub use server::{Password, Priority, Server};

/// Reads and parses an NZB file (its name gives the title and a `{{password}}`).
pub fn read(path: &std::path::Path) -> Result<Nzb, ParseError> {
    let bytes = std::fs::read(path).map_err(|e| ParseError::NotNzb(e.to_string()))?;
    let text = String::from_utf8_lossy(&bytes);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    parse(&text, &name)
}
