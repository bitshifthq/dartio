use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=PTYX_DART_SDK");
    println!("cargo:rerun-if-env-changed=DART_SDK");

    let sdk = match resolve_configured_dart_sdk() {
        Some(sdk) => sdk,
        None => match resolve_dart_sdk() {
            Some(sdk) => sdk,
            None => missing_dart_sdk(),
        },
    };
    let include = sdk.join("include");
    require_file(&include.join("dart_api_dl.h"), "Dart DL header");
    require_file(&include.join("dart_api_dl.c"), "Dart DL source");

    let mut build = cc::Build::new();
    build.file(include.join("dart_api_dl.c"));
    build.include(include);
    build.warnings(false);
    add_apple_sdk_sysroot(&mut build);
    build.compile("ptyx_dart_api_dl");
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
    let dart = which("dart")?;
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

fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
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
