//! "Who locks this file": the neutral type of the answer. The lookup itself is the backends' (`locks::who_locks`).

/// One program that holds a file open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locker {
    /// The program's name as the system shows it (an application name, or the executable's name).
    pub name: String,
    pub pid: u32,
    pub kind: LockerKind,
}

/// What kind of program holds it (Windows tells; elsewhere it is `Other`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockerKind {
    /// A program with a window.
    Application,
    /// A Windows service.
    Service,
    /// Explorer itself (a preview, a thumbnail, a folder it has open).
    Explorer,
    Other,
}

/// "Name (PID 1234), Other (PID 77)": the second line the failure list shows, without its label.
pub fn describe(lockers: &[Locker]) -> String {
    lockers.iter().map(|l| format!("{} (PID {})", l.name, l.pid)).collect::<Vec<_>>().join(", ")
}
