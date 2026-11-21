//! Linux backend of rf_win::single_instance. Status: STUB
//!
//! The handle types are written in A4 (handover's stubs take them too, from another batch); the functions come with batch B2.

/// What starting up found (rf-win single_instance.rs:34).
pub enum Role {
    /// We are the first instance. Keep the guard alive for the process lifetime.
    Primary(InstanceGuard),
    /// Another instance is running and received our message.
    Forwarded,
}

/// Held while this process is the first instance (rf-win: a named mutex). Here: nothing to hold yet.
pub struct InstanceGuard {
    _p: (),
}

impl InstanceGuard {
    /// The guard a stub hands out: holds nothing.
    pub fn inert() -> Self {
        InstanceGuard { _p: () }
    }
}

/// Never calls `on_message` (serve style): no other launch reaches this process yet.
pub fn serve_app(_app: &str, _on_message: impl Fn(String) + Send + Sync + 'static) {
    stub!("single_instance::serve_app")
}
