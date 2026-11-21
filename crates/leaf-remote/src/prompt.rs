//! The questions a connection asks the user. The app answers them with dialogs; the tests answer them in code.
//!
//! Every question comes with a `reply` the asker waits for. Dropping the reply without calling it counts as "Cancel".
//! The replies are called from any thread.

use zeroize::Zeroizing;

/// A server whose key the known_hosts files do not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyQuestion {
    /// `example.com` or `[example.com]:2222` (as known_hosts writes it).
    pub host: String,
    /// `ssh-ed25519`, `ecdsa-sha2-nistp256`, `ssh-rsa`, ...
    pub key_type: String,
    /// `SHA256:...`
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyAnswer {
    /// Connect, and add the key to `~/.ssh/known_hosts`.
    TrustAndRemember,
    /// Connect; keep the key in memory until leaf closes.
    TrustOnce,
    Cancel,
}

/// A secret the login needs. It stays in memory for this run of leaf only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretQuestion {
    /// The passphrase of an encrypted key file.
    Passphrase { key_file: String, retry: bool },
    /// The password of `user@host`. `unencrypted`: it will cross the network as plain text (plain `ftp://`).
    Password { account: String, retry: bool, unencrypted: bool },
    /// A question of the server's (keyboard-interactive login), with the server's own text.
    Server { account: String, prompt: String, echo: bool },
}

pub type Secret = Zeroizing<String>;

/// An FTPS server whose certificate the system does not trust (self-signed, another name, expired...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertQuestion {
    /// `host:port`.
    pub host: String,
    /// `SHA256:...` of the certificate.
    pub fingerprint: String,
    /// Why the system refused it, in leaf's words.
    pub reason: String,
    /// The fingerprints the user trusted for this server before (empty: none). Not empty: the certificate changed; the only
    /// way past is to replace the trusted one (`TrustAndRemember`), never "once".
    pub trusted_before: Vec<String>,
}

pub trait Prompter: Send + Sync {
    fn host_key(&self, q: HostKeyQuestion, reply: Box<dyn FnOnce(HostKeyAnswer) + Send>);
    /// `None`: the user cancelled.
    fn secret(&self, q: SecretQuestion, reply: Box<dyn FnOnce(Option<Secret>) + Send>);
    /// `TrustAndRemember`: keep the fingerprint for this server; `TrustOnce`: for this run. A prompter that does not ask
    /// answers Cancel.
    fn certificate(&self, _q: CertQuestion, reply: Box<dyn FnOnce(HostKeyAnswer) + Send>) {
        reply(HostKeyAnswer::Cancel)
    }
}

/// Answers nothing (every question is cancelled): what a connection gets when no app has set a prompter.
pub struct NoPrompter;

impl Prompter for NoPrompter {
    fn host_key(&self, _: HostKeyQuestion, reply: Box<dyn FnOnce(HostKeyAnswer) + Send>) {
        reply(HostKeyAnswer::Cancel)
    }
    fn secret(&self, _: SecretQuestion, reply: Box<dyn FnOnce(Option<Secret>) + Send>) {
        reply(None)
    }
}

/// Asks and waits (async) for the answer; a dropped reply is "Cancel".
pub(crate) async fn ask_host_key(p: &dyn Prompter, q: HostKeyQuestion) -> HostKeyAnswer {
    let (tx, rx) = tokio::sync::oneshot::channel();
    p.host_key(
        q,
        Box::new(move |a| {
            let _ = tx.send(a);
        }),
    );
    rx.await.unwrap_or(HostKeyAnswer::Cancel)
}

pub(crate) async fn ask_secret(p: &dyn Prompter, q: SecretQuestion) -> Option<Secret> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    p.secret(
        q,
        Box::new(move |s| {
            let _ = tx.send(s);
        }),
    );
    rx.await.ok().flatten()
}

/// Asks and waits on this (worker) thread; a dropped reply is "Cancel".
pub(crate) fn ask_secret_blocking(p: &dyn Prompter, q: SecretQuestion) -> Option<Secret> {
    let (tx, rx) = std::sync::mpsc::channel();
    p.secret(
        q,
        Box::new(move |s| {
            let _ = tx.send(s);
        }),
    );
    rx.recv().ok().flatten()
}

pub(crate) fn ask_certificate_blocking(p: &dyn Prompter, q: CertQuestion) -> HostKeyAnswer {
    let (tx, rx) = std::sync::mpsc::channel();
    p.certificate(
        q,
        Box::new(move |a| {
            let _ = tx.send(a);
        }),
    );
    rx.recv().unwrap_or(HostKeyAnswer::Cancel)
}
