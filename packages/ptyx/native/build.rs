use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/dart_bridge.c");
    println!("cargo:rerun-if-env-changed=PTYX_DART_SDK");
    println!("cargo:rerun-if-env-changed=DART_SDK");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=PATHEXT");
    println!("cargo:rerun-if-env-changed=PTYX_BROKER_BINARY");

    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("linux" | "macos" | "windows") => {}
        Ok(target) => fail(format!(
            "ptyx has no native backend for target operating system {target}"
        )),
        Err(_) => fail("Cargo did not provide CARGO_CFG_TARGET_OS"),
    }

    if env::var("CARGO_CFG_UNIX").is_ok() {
        configure_broker();
    }
    if env::var_os("CARGO_FEATURE_DART_ADAPTER").is_none() {
        return;
    }

    let Some(sdk) = resolve_configured_dart_sdk().or_else(resolve_dart_sdk) else {
        missing_dart_sdk();
    };
    let include = sdk.join("include");
    let header = include.join("dart_api_dl.h");
    let source = include.join("dart_api_dl.c");
    require_file(&header, "Dart DL header");
    require_file(&source, "Dart DL source");

    let mut build = cc::Build::new();
    build.file(source);
    build.file("src/dart_bridge.c");
    build.include(include);
    build.warnings(false);
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        build.flag("/std:c11");
    }
    add_apple_sdk_sysroot(&mut build);
    build.compile("ptyx_dart_api_dl");
}

fn configure_broker() {
    let path = env::var_os("PTYX_BROKER_BINARY")
        .map(PathBuf::from)
        .or_else(|| {
            env::current_dir()
                .ok()
                .map(|root| root.join("target/release/ptyx-broker"))
                .filter(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| {
            fail(
                "PTYX_BROKER_BINARY must identify the target broker executable; \
                 build native/broker before building the library",
            )
        });
    require_file(&path, "ptyx broker executable");
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| fail(format!("failed to read broker {}: {error}", path.display())));
    let identity = bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    println!("cargo:rerun-if-changed={}", path.display());
    println!(
        "cargo:rustc-env=PTYX_BROKER_BINARY={}",
        path.canonicalize().unwrap_or(path).display()
    );
    println!("cargo:rustc-env=PTYX_BROKER_ID={identity:016x}");
}

fn add_apple_sdk_sysroot(build: &mut cc::Build) {
    let Ok(target) = env::var("TARGET") else {
        return;
    };
    let sdk = if target.contains("apple-darwin") {
        "macosx"
    } else if target.contains("apple-ios-sim") {
        "iphonesimulator"
    } else if target.contains("apple-ios") {
        "iphoneos"
    } else {
        return;
    };

    let Ok(output) = Command::new("xcrun")
        .args(["--sdk", sdk, "--show-sdk-path"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return;
    }
    build.flag("-isysroot");
    build.flag(&path);
}

fn resolve_dart_sdk() -> Option<PathBuf> {
    let dart = dart_on_path()?;
    let dart = fs::canonicalize(&dart).unwrap_or(dart);
    normalize_dart_sdk(dart.parent()?.parent()?.to_path_buf())
}

fn resolve_configured_dart_sdk() -> Option<PathBuf> {
    let path = env::var_os("PTYX_DART_SDK").or_else(|| env::var_os("DART_SDK"))?;
    let path = PathBuf::from(path);
    Some(normalize_dart_sdk(path.clone()).unwrap_or_else(|| invalid_dart_sdk(&path)))
}

fn normalize_dart_sdk(path: PathBuf) -> Option<PathBuf> {
    let candidates = [
        path.clone(),
        path.join("bin").join("cache").join("dart-sdk"),
        path.join("cache").join("dart-sdk"),
    ];
    candidates
        .into_iter()
        .find(|candidate| candidate.join("include").join("dart_api_dl.h").exists())
}

fn dart_on_path() -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let names = dart_executable_names();
    for dir in env::split_paths(&path) {
        for name in &names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn dart_executable_names() -> Vec<OsString> {
    let mut names = vec![OsString::from("dart")];
    if !cfg!(windows) {
        return names;
    }

    let pathext = env::var_os("PATHEXT").unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
    for ext in pathext
        .to_string_lossy()
        .split(';')
        .filter(|ext| !ext.is_empty())
    {
        let mut name = OsString::from("dart");
        name.push(ext);
        names.push(name);
    }
    names
}

fn require_file(path: &Path, label: &str) {
    if !path.is_file() {
        fail(format!("{label} not found at {}", path.display()));
    }
}

fn invalid_dart_sdk(path: &Path) -> ! {
    fail(format!(
        "Dart SDK at {} does not contain include/dart_api_dl.h",
        path.display()
    ));
}

fn missing_dart_sdk() -> ! {
    fail(
        "Dart SDK include/dart_api_dl.h was not found. Set PTYX_DART_SDK or DART_SDK to a Dart SDK root, or make dart available on PATH.",
    );
}

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("error: {}", message.as_ref());
    std::process::exit(1);
}
