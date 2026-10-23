//! Windows: embed the app icon and version info (VERSIONINFO) into `wordcraft.exe`, so it
//! shows in Explorer, the taskbar, the Start menu and Alt-Tab.
//!
//! A missing resource compiler is a warning, so a cross-compile from macOS or Linux still links,
//! unless `WORDCRAFT_REQUIRE_WINRES=1` turns it into an error (for release builds).
//!
//! macOS: link clang's runtime for the speech model (`link_clang_runtime`). Elsewhere this does
//! nothing.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/wordcraft.ico");
    println!("cargo:rerun-if-env-changed=WORDCRAFT_REQUIRE_WINRES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        link_clang_runtime();
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/wordcraft.ico")
        .set("ProductName", "WordCraft")
        .set("FileDescription", "WordCraft word processor")
        .set("CompanyName", "Learning Machines LLC")
        .set("LegalCopyright", "Copyright (c) the WordCraft authors. MIT OR Apache-2.0.")
        .set("OriginalFilename", "wordcraft.exe")
        .set("InternalName", "wordcraft");
    if let Err(e) = res.compile() {
        if std::env::var_os("WORDCRAFT_REQUIRE_WINRES").is_some() {
            eprintln!("embedding Windows resources failed: {e}");
            std::process::exit(1);
        }
        println!("cargo:warning=wordcraft.exe built without icon/version resources: {e}");
    }
}

/// macOS: link clang's runtime library for `__isPlatformVersionAtLeast`. Dictate's speech model
/// (whisper.cpp's Metal code) checks the system version with `@available`, which clang turns into
/// calls to it; clang links its runtime itself, but Rust's link step doesn't, so a release build
/// for an older system (`MACOSX_DEPLOYMENT_TARGET=11.0`, the packaging's) failed to link.
fn link_clang_runtime() {
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    let Ok(out) = std::process::Command::new("xcrun").args(["clang", "--print-resource-dir"]).output() else {
        println!("cargo:warning=xcrun clang not found; the link may miss __isPlatformVersionAtLeast");
        return;
    };
    let dir = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).join("lib/darwin");
    if dir.join("libclang_rt.osx.a").is_file() {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    } else {
        println!("cargo:warning=no libclang_rt.osx.a in {}; the link may miss __isPlatformVersionAtLeast", dir.display());
    }
}
