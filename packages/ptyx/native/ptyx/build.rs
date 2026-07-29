use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(ptyx_no_embedded_broker)");
    println!("cargo:rerun-if-env-changed=PTYX_BROKER_BINARY");

    if env::var("CARGO_CFG_UNIX").is_err() {
        return;
    }

    let target = env::var("TARGET").unwrap_or_else(|_| fail("Cargo did not provide TARGET"));
    let Some(path) = env::var_os("PTYX_BROKER_BINARY")
        .map(PathBuf::from)
        .or_else(|| packaged_broker(&target))
        .or_else(|| workspace_broker(&target))
    else {
        println!("cargo:rustc-cfg=ptyx_no_embedded_broker");
        println!(
            "cargo:warning=target-matched broker is not embedded; \
             RuntimeBuilder::broker_path or PTYX_BROKER is required"
        );
        return;
    };
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

fn packaged_broker(target: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR")?)
        .join("broker-assets")
        .join(target)
        .join("ptyx-broker");
    println!("cargo:rerun-if-changed={}", path.display());
    path.is_file().then_some(path)
}

fn workspace_broker(target: &str) -> Option<PathBuf> {
    if env::var("HOST").ok().as_deref() != Some(target) {
        return None;
    }
    env::current_dir()
        .ok()
        .map(|root| root.join("../target/release/ptyx-broker"))
        .filter(|candidate| candidate.is_file())
}

fn require_file(path: &Path, label: &str) {
    if !path.is_file() {
        fail(format!("{label} not found at {}", path.display()));
    }
}

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("error: {}", message.as_ref());
    std::process::exit(1);
}
