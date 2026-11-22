// The helper runs elevated: Windows loads the DLLs it needs only from System32, never from the folder it is in
// (`/DEPENDENTLOADFLAG:0x800` = LOAD_LIBRARY_SEARCH_SYSTEM32), so a DLL put beside it is not picked up.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
}
