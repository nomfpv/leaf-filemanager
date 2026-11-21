//! A command line's arguments, split as programs split theirs (moved from rf-app program_launch.rs; Linux twin: leaf-unix args.rs).

use std::ffi::{OsStr, OsString};

/// `text` (the arguments only, without a program) as the list of arguments a program gets: `CommandLineToArgvW`'s rules.
pub fn split(text: &str) -> anyhow::Result<Vec<OsString>> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::UI::Shell::CommandLineToArgvW;
    use windows::core::PCWSTR;

    // Windows parses argv[0] differently, so supply a dummy executable name.
    anyhow::ensure!(!text.contains('\0'), "Invalid command arguments");
    let line = format!("program {text}");
    let wide: Vec<u16> = OsStr::new(&line).encode_wide().chain([0]).collect();
    let mut count = 0;
    unsafe {
        let argv = CommandLineToArgvW(PCWSTR(wide.as_ptr()), &mut count);
        if argv.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let args = std::slice::from_raw_parts(argv, count as usize).iter().skip(1)
            .map(|p| OsString::from_wide(p.as_wide())).collect();
        let _ = LocalFree(Some(HLOCAL(argv.cast())));
        Ok(args)
    }
}
