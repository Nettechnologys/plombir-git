use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=PLOMBIR_GIT_VERSION");
    // A tag can be added without touching a crate source file. Cargo must
    // rerun this build script then, including when Git packs older refs.
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
    {
        if output.status.success() {
            let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            for path in ["HEAD", "refs/tags", "packed-refs"] {
                println!("cargo:rerun-if-changed={git_dir}/{path}");
            }
        }
    }

    let package_version = std::env::var("CARGO_PKG_VERSION").expect("Cargo package version");
    let release_tag = format!("v{package_version}");
    let version = match std::env::var("PLOMBIR_GIT_VERSION") {
        Ok(value) if !value.is_empty() => {
            assert_eq!(
                value, release_tag,
                "release tag must match the Cargo package version"
            );
            value
        }
        _ => {
            if let Ok(output) = Command::new("git")
                .args([
                    "describe", "--tags", "--match", "v[0-9]*", "--always", "--dirty",
                ])
                .output()
            {
                if output.status.success() {
                    let described = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                    if !described.is_empty() {
                        println!("cargo:rustc-env=PLOMBIR_GIT_VERSION={described}");
                        return;
                    }
                }
            }
            format!("{release_tag}-unreleased")
        }
    };
    println!("cargo:rustc-env=PLOMBIR_GIT_VERSION={version}");
}
