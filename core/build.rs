//! Build script — compiles C hot path library + gRPC protobuf

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Declare custom cfg flags
    println!("cargo::rustc-check-cfg=cfg(has_c_fast_path)");

    // ── gRPC proto compilation ──────────────────────────────────
    let proto_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .join("proto")
        .join("mev.proto");

    if proto_path.exists() {
        println!("cargo:rerun-if-changed={}", proto_path.display());

        let protoc = protoc_bin_vendored::protoc_bin_path()
            .expect("Failed to locate vendored protoc");
        env::set_var("PROTOC", protoc);

        tonic_build::configure()
            .build_server(true)
            .build_client(true)
            .out_dir("src/grpc")
            .compile(&[&proto_path], &[proto_path.parent().unwrap()])
            .expect("Failed to compile proto");
    }

    // ── C hot path compilation (optional — gracefully skip on failure) ──
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let fast_dir = PathBuf::from(&manifest_dir).parent().unwrap().join("fast");
    
    println!("cargo:rerun-if-changed=../fast/src/");
    println!("cargo:rerun-if-changed=../fast/include/");

    if !fast_dir.join("src").exists() {
        println!("cargo:warning=fast/src not found — skipping C hot path");
        return;
    }

    match compile_c_library(&fast_dir) {
        Ok(()) => {
            println!("cargo:rustc-link-search=native={}/lib", fast_dir.display());
            println!("cargo:rustc-link-lib=static=mev_fast");
            println!("cargo:rustc-link-lib=static=mev_fast_cpp");
            println!("cargo:rustc-cfg=has_c_fast_path");
            if !cfg!(target_os = "windows") {
                println!("cargo:rustc-link-lib=pthread");
            }
        }
        Err(e) => {
            println!("cargo:warning=C hot path compilation failed: {} — Rust-only mode", e);
        }
    }
}

fn compile_c_library(fast_dir: &PathBuf) -> Result<(), String> {
    if cfg!(target_os = "windows") {
        compile_with_cc(fast_dir)
    } else {
        compile_with_make(fast_dir)
    }
}

fn compile_with_make(fast_dir: &PathBuf) -> Result<(), String> {
    let _ = Command::new("make")
        .current_dir(fast_dir)
        .arg("clean")
        .status();
    
    let status = Command::new("make")
        .current_dir(fast_dir)
        .status()
        .map_err(|e| format!("make: {}", e))?;
    
    if !status.success() {
        return Err("make failed".to_string());
    }
    Ok(())
}

fn compile_with_cc(fast_dir: &PathBuf) -> Result<(), String> {
    let src_dir = fast_dir.join("src");
    let include_dir = fast_dir.join("include");

    let all_sources: Vec<PathBuf> = std::fs::read_dir(&src_dir)
        .map_err(|e| format!("read_dir: {}", e))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .map(|e| e == "c" || e == "cpp" || e == "cxx")
                .unwrap_or(false)
        })
        .collect();

    let c_sources: Vec<&PathBuf>   = all_sources.iter().filter(|p| p.extension().map(|e| e == "c").unwrap_or(false)).collect();
    let cpp_sources: Vec<&PathBuf> = all_sources.iter().filter(|p| p.extension().map(|e| e == "cpp" || e == "cxx").unwrap_or(false)).collect();

    // ── C files ──────────────────────────────────────────────────────────────
    if !c_sources.is_empty() {
        let mut build = cc::Build::new();
        build
            .opt_level(3)
            .include(&include_dir)
            .warnings(true)
            .extra_warnings(true);

        if cfg!(target_env = "msvc") {
            build.flag("/arch:AVX2");
            build.flag("/O2");
            build.flag("/std:c17");
            build.flag("/experimental:c11atomics");
        } else {
            build.flag("-mavx2");
            build.flag("-msse4.2");
            build.flag("-mfma");
            build.flag("-mbmi2");
            build.flag("-ffast-math");
            build.flag("-funroll-loops");
            build.flag("-flto");
            build.flag("-march=native");
        }

        for source in &c_sources {
            println!("cargo:warning=Compiling C: {:?}", source);
            build.file(source);
        }

        build.try_compile("mev_fast").map_err(|e| format!("cc (C): {}", e))?;
    }

    // ── C++ files ─────────────────────────────────────────────────────────────
    if !cpp_sources.is_empty() {
        let mut build = cc::Build::new();
        build
            .cpp(true)
            .opt_level(3)
            .include(&include_dir)
            .warnings(true)
            .extra_warnings(true);

        if cfg!(target_env = "msvc") {
            build.flag("/arch:AVX2");
            build.flag("/O2");
            build.flag("/std:c++20");
            build.flag("/EHsc");
        } else {
            build.flag("-std=c++20");
            build.flag("-mavx2");
            build.flag("-msse4.2");
            build.flag("-mfma");
            build.flag("-mbmi2");
            build.flag("-ffast-math");
            build.flag("-funroll-loops");
            build.flag("-flto");
            build.flag("-march=native");
            build.flag("-fno-exceptions");
            build.flag("-fno-rtti");
        }

        for source in &cpp_sources {
            println!("cargo:warning=Compiling C++: {:?}", source);
            build.file(source);
        }

        build.try_compile("mev_fast_cpp").map_err(|e| format!("cc (C++): {}", e))?;
    }

    // If we had no sources at all, the C-only path would have returned nothing
    // but that's fine — the caller checks if the library exists.
    // Emit a combined archive name for the link search (we link both sub-archives).
    if c_sources.is_empty() && cpp_sources.is_empty() {
        return Err("no source files found in fast/src".to_string());
    }

    Ok(())
}
