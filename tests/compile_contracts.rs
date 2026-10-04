//! Compile-fail checks use the just-built library, without nested Cargo builds.

use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn callback_lifetimes_and_owner_thread_confinement_are_enforced() {
    let executable = std::env::current_exe().unwrap();
    let dependencies = executable.parent().unwrap();
    let library = fs::read_dir(dependencies)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            (name.starts_with("solapp-") || name.starts_with("libsolapp-"))
                && name.ends_with(".rlib")
        })
        .max_by_key(|entry| entry.metadata().unwrap().modified().unwrap())
        .expect("Cargo-built solapp library")
        .path();
    let output = dependencies.parent().unwrap().join("compile-contracts");
    fs::create_dir_all(&output).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compile_fail");
    for (fixture, expected) in [
        ("owner_send", "cannot be sent between threads safely"),
        ("context_send", "cannot be sent between threads safely"),
        ("message_send", "cannot be sent between threads safely"),
        ("context_escape", "lifetime may not live long enough"),
        ("application_lifetime", "lifetime may not live long enough"),
        ("native_view_send", "cannot be sent between threads safely"),
        (
            "native_view_sync",
            "cannot be shared between threads safely",
        ),
    ] {
        let result = Command::new("rustc")
            .arg("--edition=2024")
            .arg("--emit=metadata")
            .arg("--extern")
            .arg(format!("solapp={}", library.display()))
            .arg("-L")
            .arg(format!("dependency={}", dependencies.display()))
            .arg("--out-dir")
            .arg(&output)
            .arg(fixtures.join(format!("{fixture}.rs")))
            .output()
            .expect("Rust compiler available from the initialized environment");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{fixture} unexpectedly compiled");
        assert!(
            stderr.contains(expected),
            "{fixture} failed for an unexpected reason: {stderr}"
        );
    }
}
