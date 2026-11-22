//! leaf-bluray: an unprotected Blu-ray (a `BDMV` folder, a mounted disc or image) or AVCHD card, or loose `.m2ts` / `.mts` files,
//! to one normal video file. Pure Rust and no system calls besides reading and writing files and, for the optional re-encode,
//! starting `ffmpeg`.
//!
//! No AACS / BD+ code, no keys, nothing that decrypts: an encrypted stream is recognised ([`ts::looks_encrypted`]) and the
//! conversion refuses it ([`disc::OpenError::Protected`], [`remux::Error::Protected`]).

pub mod codec;
pub mod disc;
pub mod mkv;
pub mod mpls;
pub mod output;
pub mod remux;
pub mod ts;

/// What the user is told about an encrypted disc, and nothing else.
pub const PROTECTED: &str = "This disc is copy-protected. leaf converts unprotected Blu-rays only.";
