use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const SHA_ENV: &str = "XJPCODE_BUILD_GIT_SHA";
const DIRTY_ENV: &str = "XJPCODE_BUILD_GIT_DIRTY";

fn main() {
    println!("cargo:rerun-if-env-changed={SHA_ENV}");
    println!("cargo:rerun-if-env-changed={DIRTY_ENV}");

    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    emit_source_rerun_inputs(&root);
    emit_git_rerun_inputs(&root);

    let git_available =
        git_stdout(&root, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true");
    let git_sha = env::var(SHA_ENV)
        .ok()
        .or_else(|| git_stdout(&root, &["rev-parse", "HEAD"]))
        .unwrap_or_else(|| {
            panic!(
                "xjpcode builds require a full Git source SHA; build from a Git checkout or set {SHA_ENV}"
            )
        });
    assert_full_sha(&git_sha);

    let git_dirty = env::var(DIRTY_ENV)
        .ok()
        .map(|value| parse_dirty_override(&value))
        .unwrap_or_else(|| {
            if git_available {
                git_is_dirty(&root)
            } else {
                panic!(
                    "a non-Git xjpcode build must set {DIRTY_ENV}=false or true alongside {SHA_ENV}"
                )
            }
        });
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let profile = env::var("PROFILE").expect("PROFILE");
    let target = env::var("TARGET").expect("TARGET");
    let source_state = if git_dirty { "dirty" } else { "clean" };
    let identity = format!("{version} (git {git_sha}, {source_state}, {profile}, {target})");

    println!("cargo:rustc-env=XJPCODE_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=XJPCODE_GIT_DIRTY={git_dirty}");
    println!("cargo:rustc-env=XJPCODE_BUILD_PROFILE={profile}");
    println!("cargo:rustc-env=XJPCODE_BUILD_TARGET={target}");
    println!("cargo:rustc-env=XJPCODE_BUILD_IDENTITY={identity}");
}

fn git_stdout(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn git_is_dirty(root: &Path) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .unwrap_or_else(|error| panic!("failed to inspect Git source state: {error}"));
    if !output.status.success() {
        panic!("git status failed while deriving xjpcode build provenance");
    }
    !output.stdout.is_empty()
}

fn parse_dirty_override(value: &str) -> bool {
    match value {
        "true" | "1" => true,
        "false" | "0" => false,
        _ => panic!("{DIRTY_ENV} must be true, false, 1, or 0"),
    }
}

fn assert_full_sha(value: &str) {
    assert!(
        value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{SHA_ENV} must be a full 40-character hexadecimal Git SHA"
    );
}

fn emit_source_rerun_inputs(root: &Path) {
    for path in ["src", "Cargo.toml", "Cargo.lock", "build.rs"] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
}

fn emit_git_rerun_inputs(root: &Path) {
    let Some(git_dir) = git_stdout(root, &["rev-parse", "--git-dir"]) else {
        return;
    };
    let git_dir = PathBuf::from(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        root.join(git_dir)
    };
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!("cargo:rerun-if-changed={}", git_dir.join("index").display());

    if let Some(symbolic_head) = git_stdout(root, &["symbolic-ref", "-q", "HEAD"]) {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(symbolic_head).display()
        );
    }
}
