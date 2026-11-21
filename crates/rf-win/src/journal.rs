//! The crash-safe job journal: its format and its reader and writer are shared with the other
//! systems' engines and live in `leaf_copy::journal`.

pub use leaf_copy::journal::{Writer, in_use, read, rewrite};
