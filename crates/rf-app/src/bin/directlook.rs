#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! leaf-look: Hızlı Bakış, rustyfiles'tan ayrı süreç.

fn main() {
    rf_app::look::app::main();
}
