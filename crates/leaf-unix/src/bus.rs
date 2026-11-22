//! The user's session bus, connected once and shared by what talks over it (notifications, the launcher entry's progress). A
//! failed connection (no session bus: a console login, a container) is remembered: those features then stay quiet.

use std::sync::OnceLock;

use zbus::blocking::Connection;

pub(crate) fn session() -> Option<&'static Connection> {
    static BUS: OnceLock<Option<Connection>> = OnceLock::new();
    BUS.get_or_init(|| Connection::session().map_err(|e| log::info!("no session bus: {e}")).ok()).as_ref()
}

/// The system bus (udisks2's drive properties), connected once; `None` where there is none.
pub(crate) fn system() -> Option<&'static Connection> {
    static BUS: OnceLock<Option<Connection>> = OnceLock::new();
    BUS.get_or_init(|| Connection::system().map_err(|e| log::info!("no system bus: {e}")).ok()).as_ref()
}
