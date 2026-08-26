use std::{env, fs, path::Path, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    link_runtime(
        "xkbcommon",
        "libxkbcommon.so",
        &[
            "/usr/lib64/libxkbcommon.so.0",
            "/lib64/libxkbcommon.so.0",
            "/usr/lib/x86_64-linux-gnu/libxkbcommon.so.0",
            "/lib/x86_64-linux-gnu/libxkbcommon.so.0",
        ],
    );
    link_runtime(
        "pixman-1",
        "libpixman-1.so",
        &[
            "/usr/lib64/libpixman-1.so.0",
            "/lib64/libpixman-1.so.0",
            "/usr/lib/x86_64-linux-gnu/libpixman-1.so.0",
            "/lib/x86_64-linux-gnu/libpixman-1.so.0",
        ],
    );
}

fn link_runtime(package: &str, link_name: &str, candidates: &[&str]) {
    if Command::new("pkg-config")
        .args(["--exists", package])
        .status()
        .is_ok_and(|status| status.success())
    {
        return;
    }
    let Some(runtime_library) = candidates.iter().map(Path::new).find(|path| path.exists()) else {
        return;
    };

    let link_dir = Path::new(&env::var_os("OUT_DIR").unwrap()).join("native-link");
    fs::create_dir_all(&link_dir).unwrap();
    let link = link_dir.join(link_name);
    if !link.exists() {
        std::os::unix::fs::symlink(runtime_library, &link).unwrap();
    }
    println!("cargo:rustc-link-search=native={}", link_dir.display());
}
