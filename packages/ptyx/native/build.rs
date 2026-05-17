use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=PTYX_DART_SDK");
    println!("cargo:rerun-if-env-changed=DART_SDK");
    println!("cargo:rerun-if-env-changed=PATH");
    println!("cargo:rerun-if-env-changed=PATHEXT");

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
