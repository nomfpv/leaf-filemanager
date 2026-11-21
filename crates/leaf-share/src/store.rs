//! The shares this PC takes part in, and the invites it gave out: a small JSON file in the share state folder. No secret is kept
//! in the clear: an invite is kept as the SHA-256 of its secret.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE: &str = "shares.json";
/// An invite is good for this long, once.
pub const INVITE_LIFE_MS: i64 = 24 * 3600 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// This PC shared the folder: it drives the syncs.
    Host,
    /// This PC joined a friend's shared folder.
    Guest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRec {
    /// The other PC's device id and the fingerprint of the certificate it proved.
    pub id: String,
    pub fingerprint: String,
    pub name: String,
    /// Where it is reached (`host:port`): for a friend's share the address this PC dials; for a share of this PC where the
    /// friend was last seen.
    #[serde(default)]
    pub address: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRec {
    pub secret_sha256: String,
    pub expires_ms: i64,
    #[serde(default)]
    pub used: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareRec {
    /// The same on both PCs; names the sync state file.
    pub id: String,
    /// The folder's name as shown (the sharing PC's folder name).
    pub name: String,
    pub folder: PathBuf,
    pub role: Role,
    #[serde(default)]
    pub peer: Option<PeerRec>,
    /// On the sharing PC: the friend's changes come back ("They can change files"). On the friend's PC: what the sharing PC said.
    #[serde(default = "yes")]
    pub friend_can_write: bool,
    /// Why it is paused (`Some("")`: the user paused it).
    #[serde(default)]
    pub paused: Option<String>,
    #[serde(default)]
    pub last_sync_ms: i64,
    #[serde(default)]
    pub last_result: String,
    /// Globs left out on both sides, besides what `.leafignore` in the folder says.
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub invites: Vec<InviteRec>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub shares: Vec<ShareRec>,
    #[serde(skip)]
    path: PathBuf,
}

impl Store {
    pub fn load(dir: &Path) -> Store {
        let path = dir.join(FILE);
        let mut s: Store = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                log::warn!("share: {} is not readable ({e}); kept aside as .bad", path.display());
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                Store::default()
            }),
            Err(_) => Store::default(),
        };
        s.path = path;
        s
    }

    pub fn save(&self) {
        let r = (|| {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
            std::fs::rename(&tmp, &self.path)
        })();
        if let Err(e) = r {
            log::warn!("share: the shares could not be saved: {e}");
        }
    }

    pub fn get(&self, id: &str) -> Option<&ShareRec> {
        self.shares.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut ShareRec> {
        self.shares.iter_mut().find(|s| s.id == id)
    }

    /// Whether a device that proved `fp` is stored for any share (the devices then count as paired).
    pub fn knows(&self, fp: &str) -> bool {
        self.shares.iter().any(|s| s.peer.as_ref().is_some_and(|p| p.fingerprint == fp))
    }

    /// Whether any share of this PC has an invite that could still be taken.
    pub fn open_invites(&self, now: i64) -> bool {
        self.shares.iter().any(|s| s.role == Role::Host && s.peer.is_none() && s.invites.iter().any(|i| !i.used && i.expires_ms > now))
    }
}

pub fn sha256_hex(s: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, s.as_bytes()).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// `n` random bytes, hex.
pub fn random_hex(n: usize) -> String {
    use ring::rand::SecureRandom as _;
    let mut b = vec![0u8; n];
    ring::rand::SystemRandom::new().fill(&mut b).expect("the system's random numbers");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What a friend pastes to join: this PC's device id, where it is reached, the share and a one-time secret, and when it runs out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub device: String,
    pub share: String,
    pub secret: String,
    pub expires_ms: i64,
    pub address: String,
}

const MAGIC: &str = "leaf-share1";

impl Invite {
    /// One line: `leaf-share1 <device> <share> <secret> <expires> <address>`.
    pub fn to_text(&self) -> String {
        format!("{MAGIC} {} {} {} {} {}", self.device, self.share, self.secret, self.expires_ms, self.address)
    }

    /// Reads a pasted invite (spaces and line breaks around and between its parts do not matter).
    pub fn parse(text: &str) -> Result<Invite, String> {
        let f: Vec<&str> = text.split_whitespace().collect();
        let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
        match f.as_slice() {
            [m, device, share, secret, expires, address] if *m == MAGIC => {
                if !hex(device, 20) || !hex(share, 16) || !hex(secret, 32) {
                    return Err("The invite is damaged (a part of it is missing or changed)".into());
                }
                let expires_ms = expires.parse().map_err(|_| "The invite is damaged (its date cannot be read)".to_string())?;
                Ok(Invite { device: device.to_string(), share: share.to_string(), secret: secret.to_string(), expires_ms, address: address.to_string() })
            }
            _ => Err("This is not a leaf invite. Paste the whole line your friend sent (it starts with leaf-share1)".into()),
        }
    }
}
