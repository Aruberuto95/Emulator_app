use std::path::Path;
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").unwrap() != "x86_64" {
        return;
    }
    let rdp = Path::new("../parallel-rdp");
    let mut build = cxx_build::bridge("src/bridge.rs");
    build.file("cpp/renderer.cpp").include("..").std("c++17");
    for dir in [
        "parallel-rdp",
        "volk",
        "vulkan",
        "vulkan-headers/include",
        "util",
    ] {
        build.include(rdp.join(dir));
    }
    // This list is the upstream standalone manifest, pinned with the source.
    let manifest = std::fs::read_to_string(rdp.join("config.mk")).unwrap();
    for line in manifest.lines() {
        if let Some((_, tail)) = line.split_once("$(PARALLEL_RDP_IMPLEMENTATION)/") {
            let file = tail.trim().trim_end_matches('\\').trim();
            if file.ends_with(".cpp") {
                build.file(rdp.join(file));
            }
        }
    }
    for entry in std::fs::read_dir(rdp.join("parallel-rdp")).unwrap() {
        let file = entry.unwrap().path();
        if file.extension().is_some_and(|e| e == "cpp") {
            build.file(file);
        }
    }
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        build
            .define("VK_USE_PLATFORM_WIN32_KHR", None)
            .flag_if_supported("/EHsc");
        println!("cargo:rustc-link-lib=winmm");
    }
    build.warnings(false).compile("n64_rdp");
    let mut volk = cc::Build::new();
    volk.file(rdp.join("volk/volk.c"))
        .include(rdp.join("vulkan-headers/include"));
    // VolkDeviceTable has platform-conditional members. Both languages must use
    // the same defines, otherwise every subsequent function pointer is shifted.
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        volk.define("VK_USE_PLATFORM_WIN32_KHR", None);
    }
    volk.warnings(false).compile("n64_volk");
    println!("cargo:rerun-if-changed=src/bridge.rs");
    println!("cargo:rerun-if-changed=cpp");
    println!("cargo:rerun-if-changed=../parallel-rdp");
}
