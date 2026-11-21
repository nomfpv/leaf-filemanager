/// "b241ace" (plus "+" when the tree had uncommitted changes) and a time, shown next to the
/// version so every copy says which code it is. The time is the commit's, so the stamp only
/// changes with a commit or a checkout; LEAF_STAMP_BUILD=1 stamps the build time instead.
///
/// Git runs with --no-optional-locks: a plain `git status` rewrites .git/index, and anything
/// watching it (as this script used to) then rebuilt rf-app on every build.
fn build_info() {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("--no-optional-locks")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let hash = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let secs = if std::env::var_os("LEAF_STAMP_BUILD").is_some() {
        now()
    } else {
        git(&["log", "-1", "--format=%ct"]).and_then(|s| s.parse().ok()).unwrap_or_else(now)
    };
    println!("cargo:rustc-env=LEAF_COMMIT={hash}{}", if dirty { "+" } else { "" });
    println!("cargo:rustc-env=LEAF_BUILT={secs}");
    println!("cargo:rerun-if-env-changed=LEAF_STAMP_BUILD");
    // a new commit or a checkout rebuilds the stamp: HEAD, the branch it points at, packed refs
    // (refs live in the common dir when this is a worktree)
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={dir}/HEAD");
    }
    // only files that exist: Cargo counts a missing one as changed on every build
    if let Some(common) = git(&["rev-parse", "--git-common-dir"]) {
        let branch = git(&["symbolic-ref", "-q", "HEAD"]);
        for f in std::iter::once("packed-refs".to_string()).chain(branch) {
            let path = format!("{common}/{f}");
            if std::path::Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
}

fn main() {
    build_info();
    println!("cargo:rerun-if-changed=rustyfiles.rc");
    println!("cargo:rerun-if-changed=directlook.rc");
    println!("cargo:rerun-if-changed=assets/rustyfiles.ico");
    // each exe gets its own name (Windows shows FileDescription in Open with / Task Manager)
    // The .rc files say 0.1.0 as a placeholder: the version resource (Task Manager, Properties →
    // Details, Apps & features) gets the crate's real version, written into a copy in OUT_DIR.
    #[cfg(windows)]
    {
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let ver = std::env::var("CARGO_PKG_VERSION").unwrap();
        let nums: Vec<u32> = ver.split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect();
        let commas = format!("{},{},{},0", nums.first().unwrap_or(&0), nums.get(1).unwrap_or(&0), nums.get(2).unwrap_or(&0));
        for (rc, bin) in [("rustyfiles.rc", "rustyfiles"), ("directlook.rc", "directlook")] {
            let text = std::fs::read_to_string(dir.join(rc)).expect("read .rc")
                .replace("0,1,0,0", &commas)
                .replace("\"0.1.0\"", &format!("\"{ver}\""))
                .replace("\"assets/", &format!("\"{}/assets/", dir.display().to_string().replace('\\', "/")));
            let copy = out.join(rc);
            std::fs::write(&copy, text).expect("write .rc");
            embed_resource::compile_for(&copy, [bin], embed_resource::NONE).manifest_optional().expect("embed resources");
        }
    }
}
