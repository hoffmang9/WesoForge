use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=BBR_CHIAVDF_DIR");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("crate is in crates/*");

    let chiavdf_dir = env::var("BBR_CHIAVDF_DIR")
        .map(PathBuf::from)
        // Default to the `chiavdf/` git submodule.
        .unwrap_or_else(|_| {
            let submodule = repo_root.join("chiavdf");
            if submodule
                .join("src")
                .join("c_bindings")
                .join("fast_wrapper.cpp")
                .exists()
            {
                return submodule;
            }

            panic!(
                "chiavdf repo not found at {}. Run `git submodule update --init --recursive` \
or set BBR_CHIAVDF_DIR to a chiavdf checkout.",
                submodule.display()
            );
        });
    let chiavdf_src = chiavdf_dir.join("src");

    println!(
        "cargo:rerun-if-changed={}",
        chiavdf_src.join("Makefile.vdf-client").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        chiavdf_src.join("vdf.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        chiavdf_src.join("callback.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        chiavdf_src
            .join("c_bindings")
            .join("fast_wrapper.cpp")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        chiavdf_src
            .join("c_bindings")
            .join("fast_wrapper.h")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        chiavdf_src.join("compile_asm.cpp").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        chiavdf_src.join("refcode").join("lzcnt.c").display()
    );

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        build_windows_fallback(&manifest_dir, &chiavdf_dir, &chiavdf_src);
        return;
    }

    let status = Command::new("make")
        .arg("-C")
        .arg(&chiavdf_src)
        .arg("-f")
        .arg("Makefile.vdf-client")
        // Let `make` use its incremental rebuild logic.
        .arg("fastlib")
        .arg("PIC=1")
        .arg("LTO=")
        .status()
        .expect("failed to run make to build chiavdf fast library");

    if !status.success() {
        panic!("chiavdf fast library build failed (exit code: {status})");
    }

    println!("cargo:rustc-link-search=native={}", chiavdf_src.display());
    println!("cargo:rustc-link-lib=static=chiavdf_fastc");

    // chiavdf depends on GMP and pthread.
    println!("cargo:rustc-link-lib=gmpxx");
    println!("cargo:rustc-link-lib=gmp");
    println!("cargo:rustc-link-lib=pthread");

    // We link C++ objects, so we need the C++ standard library.
    // Keep it simple: this project currently targets Linux.
    if target_os == "macos" {
        println!("cargo:rustc-link-lib=c++");
    } else if target_os != "windows" {
        println!("cargo:rustc-link-lib=stdc++");
    }

    // chiavdf's generated assembly isn't PIE/PIC-safe. Rust builds PIE binaries by default
    // on many Linux distros, so disable PIE for any binary that links this crate.
    if target_os == "linux" {
        println!("cargo:rustc-link-arg=-no-pie");
    }
}

fn build_windows_fallback(manifest_dir: &PathBuf, chiavdf_dir: &PathBuf, chiavdf_src: &PathBuf) {
    let fallback_cpp = manifest_dir.join("native").join("chiavdf_fast_fallback.cpp");
    println!("cargo:rerun-if-changed={}", fallback_cpp.display());

    // The chiavdf repository expects the MPIR (GMP-compatible) Windows bundle to
    // live at `chiavdf/mpir_gc_x64`.
    let mpir_dir = chiavdf_dir.join("mpir_gc_x64");
    let mpir_lib = mpir_dir.join("mpir.lib");
    if !mpir_lib.exists() {
        panic!(
            "mpir.lib not found at {}. Ensure chiavdf/mpir_gc_x64 is present (see chiavdf's pyproject.toml windows build instructions).",
            mpir_lib.display()
        );
    }

    // The chiavdf sources use GNU/Clang builtins (e.g. __builtin_clzll) even
    // on Windows. Compile this fallback with clang-cl for compatibility.
    //
    // Prefer an explicit path if the user configured one; otherwise fall back
    // to the default winget install location.
    let clang_cl = env::var("BBR_CLANG_CL").unwrap_or_else(|_| {
        let default = PathBuf::from(r"C:\Program Files\LLVM\bin\clang-cl.exe");
        if default.exists() {
            default.to_string_lossy().to_string()
        } else {
            "clang-cl".to_string()
        }
    });

    let mut build = cc::Build::new();
    build.cpp(true);
    build.compiler(clang_cl);
    build.flag("/std:c++17");
    build.flag("/EHsc");
    build.flag("/O2");
    build.define("_CRT_SECURE_NO_WARNINGS", None);
    build.include(chiavdf_src);
    build.include(&mpir_dir);
    build.file(fallback_cpp);
    build.file(chiavdf_src.join("refcode").join("lzcnt.c"));
    build.compile("chiavdf_fastc");

    // Link against MPIR (GMP-compatible) import library.
    println!("cargo:rustc-link-search=native={}", mpir_dir.display());
    println!("cargo:rustc-link-lib=mpir");
}
