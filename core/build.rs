use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // Compile and link FFI implementation sources
    cxx_build::bridge("src/lib.rs")
        .std("c++17")
        .compile("emulator_core");

    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=src/emulator.rs");

    // Output headers if invoked from CMake with CXXBRIDGE_OUTPUT_DIR
    if let Ok(output_dir) = env::var("CXXBRIDGE_OUTPUT_DIR") {
        let output_path = PathBuf::from(output_dir);

        // 1. Generate rust/cxx.h
        let cxx_h = output_path.join("rust").join("cxx.h");
        if let Some(parent) = cxx_h.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&cxx_h, cxx_gen::HEADER).unwrap();

        // 2. Generate core/src/lib.rs.h
        let bridge_h = output_path.join("core").join("src").join("lib.rs.h");
        if let Some(parent) = bridge_h.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        let rust_source = fs::read_to_string("src/lib.rs").unwrap();
        let token_stream = rust_source.parse::<proc_macro2::TokenStream>().unwrap();
        let gen = cxx_gen::generate_header_and_cc(
            token_stream,
            &cxx_gen::Opt::default()
        ).unwrap();
        fs::write(&bridge_h, gen.header).unwrap();
    }
}
