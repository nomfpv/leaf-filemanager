#![windows_subsystem = "windows"]

//! leaf-open: what Windows starts for folders and Win+E while leaf is the default file manager (see the leaf-open crate).

fn main() {
    leaf_open::main();
}
