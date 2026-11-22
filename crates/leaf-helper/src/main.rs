//! leaf-helper: see the crate's documentation (lib.rs). No window, no console: it only talks to leaf.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    // a panic ends the helper at once (no unwinding through half-done work, nothing caught and carried on)
    std::panic::set_hook(Box::new(|_| std::process::abort()));
    let args: Vec<String> = match std::env::args_os().skip(1).map(|a| a.into_string()).collect::<Result<_, _>>() {
        Ok(a) => a,
        Err(_) => std::process::exit(2),
    };
    std::process::exit(leaf_helper::os::main(args))
}
