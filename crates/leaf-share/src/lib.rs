//! leaf-share: one folder kept the same on two PCs that both run leaf ("Share with a friend..."), without a server and without
//! another program. Built on what leaf has:
//!
//! - rf-nearby's identity (the same certificate and device id as Nearby) and its QUIC with TLS 1.3, where both sides prove their
//!   certificate; the six-digit code of a first meeting comes from the TLS session, as in Nearby's pairing;
//! - leaf-sync's planner and run (two-way with the state record; removals to the Trash or `.leaf-sync-trash`, never for good; the
//!   missing-side and 30 % refusals of a scheduled run, which here pause the share instead of guessing).
//!
//! How it goes: the PC that shares a folder makes an invite (a line of text: its device id, its address, the share, a one-time
//! secret, 24 hours). The friend pastes it and picks where the folder goes; its leaf dials that address, shows the secret, and the
//! two users confirm the same code once when the PCs never met. From then on both keep each other's device id for that share and
//! nothing else is answered. The friend's PC keeps dialing (with a growing wait while the other PC is off); the sharing PC drives
//! every sync over that connection, with the friend's folder as a remote `Fs` ([`remote`]) whose every path the friend's PC checks
//! ([`folder`]). Changes on either side (a folder watch, 5 s quiet) and every new connection start a sync.
//!
//! The transport is behind two small traits ([`transport::Transport`], [`transport::Link`]): today QUIC to a typed address (a
//! WireGuard or LAN address); a relayed one can be added for people without a VPN.

pub mod ext_msg;
pub mod folder;
pub mod remote;
mod service;
pub mod store;
mod sync;
pub mod transport;
pub mod wire;
#[cfg(test)]
mod tests;

pub use service::{Config, ShareInfo, ShareListener, ShareService, State};
pub use store::{Invite, Role};
pub use transport::own_addresses;

/// The port shares listen on unless the user picks another one (next to LocalSend's 53317).
pub const DEFAULT_PORT: u16 = 53318;

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}
