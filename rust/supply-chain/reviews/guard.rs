//! Dependency-policy regressions, runnable before third-party dependencies are vetted.
//! From repository root: rustc --edition=2024 --test rust/supply-chain/reviews/guard.rs
//! -o /tmp/cama-review-guard-tests && /tmp/cama-review-guard-tests

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

struct Fixture {
    root: PathBuf,
    script: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "cama-review-guard-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create isolated fixture");
        let script = std::env::current_dir()
            .unwrap()
            .join(".github/scripts/check-reviewed-dependencies");
        assert!(script.is_file(), "run guard tests from repository root");
        let this = Self { root, script };
        let host = Command::new("rustc").arg("-vV").output().unwrap();
        let host = String::from_utf8(host.stdout).unwrap();
        let host = host.lines().find_map(|l| l.strip_prefix("host: ")).unwrap();
        let dependencies = [
            ("toml_parser", "1.1.3", vec![]),
            ("toml_edit", "0.25.14", vec![]),
            ("crossbeam-utils", "0.8.23", vec!["dashmap"]),
            ("rsa", "0.9.10", vec!["steam-vent", "steam-vent-crypto"]),
        ];
        let consumers = [
            ("dashmap", "6.2.1", "crossbeam-utils"),
            ("steam-vent", "0.5.0", "rsa"),
            ("steam-vent-crypto", "0.2.1", "rsa"),
        ];
        let mut packages = Vec::new();
        let mut nodes = Vec::new();
        let mut rules = Vec::new();
        for (name, version, parents) in dependencies {
            packages.push(format!(
                "{{\"id\":{n},\"name\":{n},\"version\":{v},\"manifest_path\":\"/registry/Cargo.toml\",\"source\":\"registry+fixture\"}}",
                n = quote(name), v = quote(version)
            ));
            nodes.push(format!(
                "{{\"id\":{},\"features\":[],\"deps\":[]}}",
                quote(name)
            ));
            let parents = parents.iter().map(|name| {
                let version = consumers.iter().find(|c| c.0 == *name).unwrap().1;
                format!("{{\"name\":{},\"version\":{},\"kinds\":[{{\"kind\":\"normal\",\"target\":null}}]}}",quote(name),quote(version))
            }).collect::<Vec<_>>().join(",");
            rules.push(format!(
                "{{\"name\":{},\"version\":{},\"features\":[],\"parents\":[{}]}}",
                quote(name),
                quote(version),
                parents
            ));
        }
        let mut roots = Vec::new();
        let mut checksums = Vec::new();
        for (name, version, dependency) in consumers {
            let relative = format!("rust/vendor/{name}");
            let manifest = format!("{relative}/Cargo.toml");
            fs::create_dir_all(this.root.join(&relative)).unwrap();
            this.write(&manifest, "reviewed fixture\n");
            let digest = Command::new("sha256sum")
                .arg(this.root.join(&manifest))
                .output()
                .unwrap();
            assert!(digest.status.success());
            let digest = String::from_utf8(digest.stdout).unwrap();
            let digest = digest.split_whitespace().next().unwrap();
            roots.push(quote(&relative));
            checksums.push(format!("{}:{}", quote(&manifest), quote(digest)));
            packages.push(format!(
                "{{\"id\":{n},\"name\":{n},\"version\":{v},\"manifest_path\":{p}}}",
                n = quote(name),
                v = quote(version),
                p = quote(this.root.join(&manifest).to_str().unwrap())
            ));
            nodes.push(format!("{{\"id\":{},\"features\":[],\"deps\":[{{\"name\":{},\"pkg\":{},\"dep_kinds\":[{{\"kind\":null,\"target\":null}}]}}]}}",quote(name),quote(dependency),quote(dependency)));
        }
        this.write(
            "metadata.json",
            &format!(
                "{{\"packages\":[{}],\"resolve\":{{\"nodes\":[{}]}}}}",
                packages.join(","),
                nodes.join(",")
            ),
        );
        this.write(
            "scope.json",
            &format!(
                "{{\"schema\":1,\"target\":{},\"cargo_configs\":{{}},\"packages\":[{}]}}",
                quote(host),
                rules.join(",")
            ),
        );
        this.write(
            "inventory.json",
            &format!(
                "{{\"schema\":1,\"roots\":[{}],\"files\":{{{}}}}}",
                roots.join(","),
                checksums.join(",")
            ),
        );
        this
    }

    fn write(&self, path: &str, content: &str) {
        fs::write(self.root.join(path), content).unwrap();
    }

    fn replace(&self, path: &str, from: &str, to: &str) {
        let text = fs::read_to_string(self.root.join(path)).unwrap();
        assert!(text.contains(from), "fixture replacement did not match");
        self.write(path, &text.replace(from, to));
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("bash");
        cmd.arg(&self.script)
            .args(["--root", self.root.to_str().unwrap()])
            .args([
                "--metadata",
                self.root.join("metadata.json").to_str().unwrap(),
            ])
            .args(["--scope", self.root.join("scope.json").to_str().unwrap()])
            .args([
                "--inventory",
                self.root.join("inventory.json").to_str().unwrap(),
            ])
            .env_remove("CARGO_BUILD_TARGET");
        cmd
    }

    fn run(&self) -> Output {
        self.command().output().unwrap()
    }

    fn rejects(&self, message: &str) {
        let result = self.run();
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "unexpected guard success");
        assert!(
            stderr.contains(message),
            "expected {message:?}, got {stderr}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn unchanged_review_passes() {
    let result = Fixture::new().run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn newly_enabled_feature_requires_review() {
    let f = Fixture::new();
    f.replace(
        "metadata.json",
        "\"id\":\"toml_parser\",\"features\":[]",
        "\"id\":\"toml_parser\",\"features\":[\"extra\"]",
    );
    f.rejects("versions, features, or consumers changed");
}

#[test]
fn unsafe_and_unbounded_are_rejected_even_if_manifest_allows_them() {
    for (package, version, feature) in [
        ("toml_parser", "1.1.3", "unsafe"),
        ("toml_edit", "0.25.14", "unbounded"),
    ] {
        let f = Fixture::new();
        f.replace(
            "metadata.json",
            &format!("\"id\":{},\"features\":[]", quote(package)),
            &format!(
                "\"id\":{},\"features\":[{}]",
                quote(package),
                quote(feature)
            ),
        );
        f.replace(
            "scope.json",
            &format!(
                "\"name\":{},\"version\":{},\"features\":[]",
                quote(package),
                quote(version)
            ),
            &format!(
                "\"name\":{},\"version\":{},\"features\":[{}]",
                quote(package),
                quote(version),
                quote(feature)
            ),
        );
        f.rejects("prohibited feature or consumer");
    }
}

#[test]
fn parent_edge_kind_changes_require_review() {
    let f = Fixture::new();
    f.replace("metadata.json", "\"kind\":null", "\"kind\":\"build\"");
    f.rejects("versions, features, or consumers changed");
}

#[test]
fn new_version_or_missing_dependency_requires_review() {
    for replacement in ["1.1.4", "missing-package"] {
        let f = Fixture::new();
        if replacement == "missing-package" {
            f.replace("metadata.json", "\"toml_parser\"", "\"missing-package\"");
        } else {
            f.replace("metadata.json", "\"1.1.3\"", "\"1.1.4\"");
        }
        f.rejects("versions, features, or consumers changed");
    }
}

#[test]
fn consumer_cannot_switch_to_registry_source() {
    let f = Fixture::new();
    let path = f.root.join("rust/vendor/dashmap/Cargo.toml");
    f.replace(
        "metadata.json",
        path.to_str().unwrap(),
        "/registry/dashmap/Cargo.toml",
    );
    f.rejects("not the reviewed vendored package");
}

#[test]
fn modified_vendor_source_fails_checksum() {
    let f = Fixture::new();
    f.write("rust/vendor/dashmap/Cargo.toml", "unreviewed change\n");
    f.rejects("source checksum changed");
}

#[test]
fn extra_or_removed_vendor_file_fails_inventory() {
    let f = Fixture::new();
    f.write("rust/vendor/dashmap/extra.rs", "unreviewed");
    f.rejects("file inventory changed");
    fs::remove_file(f.root.join("rust/vendor/dashmap/extra.rs")).unwrap();
    fs::remove_file(f.root.join("rust/vendor/dashmap/Cargo.toml")).unwrap();
    f.rejects("file inventory changed");
}

#[test]
fn symlink_cannot_escape_inventory() {
    let f = Fixture::new();
    std::os::unix::fs::symlink(
        Path::new("/etc/passwd"),
        f.root.join("rust/vendor/dashmap/alias"),
    )
    .unwrap();
    f.rejects("symlink or special file");
}

#[test]
fn unsafe_inventory_path_is_rejected() {
    let f = Fixture::new();
    f.replace(
        "inventory.json",
        "rust/vendor/dashmap/Cargo.toml",
        "rust/vendor/dashmap/../Cargo.toml",
    );
    f.rejects("invalid vendor inventory");
}

#[test]
fn target_override_is_rejected() {
    let f = Fixture::new();
    let result = f
        .command()
        .env("CARGO_BUILD_TARGET", "x86_64-pc-windows-msvc")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("CARGO_BUILD_TARGET"));
}

#[test]
fn missing_required_scope_rule_is_rejected() {
    let f = Fixture::new();
    f.replace("scope.json", "\"toml_parser\"", "\"other_parser\"");
    f.rejects("invalid dependency-scope manifest");
}

#[test]
fn unlisted_vendor_crate_is_rejected() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("rust/vendor/new-crate")).unwrap();
    f.rejects("vendor root inventory changed");
}

#[test]
fn new_crossbeam_or_rsa_consumer_is_rejected_even_if_manifest_allows_it() {
    for parent in ["dashmap", "steam-vent"] {
        let f = Fixture::new();
        for path in ["metadata.json", "scope.json"] {
            f.replace(path, &quote(parent), "\"unreviewed-consumer\"");
        }
        f.rejects("prohibited feature or consumer");
    }
}

#[test]
fn external_local_dependency_is_rejected() {
    let f = Fixture::new();
    f.replace(
        "metadata.json",
        "\"source\":\"registry+fixture\"",
        "\"source\":null",
    );
    f.rejects("local dependency is outside");
}

#[test]
fn unlisted_cargo_target_configuration_is_rejected() {
    let f = Fixture::new();
    fs::create_dir(f.root.join(".cargo")).unwrap();
    f.write(
        ".cargo/config.toml",
        "[build]\ntarget='x86_64-pc-windows-msvc'\n",
    );
    f.rejects("Cargo config inventory changed");
}

#[test]
fn changed_reviewed_cargo_configuration_is_rejected() {
    let f = Fixture::new();
    fs::create_dir(f.root.join(".cargo")).unwrap();
    f.write(".cargo/config.toml", "[build]\njobs=2\n");
    let digest = Command::new("sha256sum")
        .arg(f.root.join(".cargo/config.toml"))
        .output()
        .unwrap();
    assert!(digest.status.success());
    let digest = String::from_utf8(digest.stdout).unwrap();
    let digest = digest.split_whitespace().next().unwrap();
    f.replace(
        "scope.json",
        "\"cargo_configs\":{}",
        &format!(
            "\"cargo_configs\":{{\".cargo/config.toml\":{}}}",
            quote(digest)
        ),
    );
    let initial = f.run();
    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );
    f.write(
        ".cargo/config.toml",
        "[build]\ntarget='x86_64-pc-windows-msvc'\n",
    );
    f.rejects("Cargo configuration checksum changed");
}
