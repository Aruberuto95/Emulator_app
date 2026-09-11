fn main() {
    // Cargo owns generation of both the implementation and matching headers.
    // CMake stages target/cxxbridge after every Cargo invocation, even when
    // this build script is cached or the CMake build directory was recreated.
    cxx_build::bridge("src/lib.rs")
        .std("c++17")
        .compile("emulator_core");
    println!("cargo:rerun-if-changed=src/lib.rs");
}
