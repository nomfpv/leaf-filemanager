//! What the copy engines of every system share: the crash-safe job journal ([`journal`]: the file format rf-win has always written,
//! now read and written here for Windows and Linux alike) and the resume decision ([`resume`]: pure functions, no system calls).
//! The engines themselves stay per system (rf-win's CopyFile2 engine, leaf-unix's copy_file_range engine).

pub mod journal;
pub mod resume;
