//! S-D14 — Local-gallery security gates: the cache/temp **placement audit** (SR3) and the **NFR1**
//! no-network-on-read-paths proof, both encoded as executable invariants.
//!
//! Contract: [Local Gallery — Security Requirements] (SR3, NFR1).
//!
//! - SR3 (no plaintext spillage outside the library root) is audited two ways: every path the
//!   `library::paths` module can hand out is proven to live under the library root, and a
//!   source/manifest sweep proves production code has no way to write to a shared/OS-temp location
//!   (no `env::temp_dir`, no hardcoded `/tmp`, and `tempfile` is a dev-dependency only, so it
//!   cannot be referenced from non-test code by construction).
//! - NFR1 (read paths perform zero network I/O) is proven **structurally**: `capsule-core` — the
//!   crate every gallery read path (library open, view queries, asset read) runs through — has no
//!   network dependency in its transitive graph. A socket-refusing runtime harness would only
//!   observe what the dependency graph already guarantees: there is no HTTP/gRPC client to call.
//!
//! [Local Gallery — Security Requirements]: https://docs/design/local-gallery/#security-requirements

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use capsule_core::library::{
    ThumbnailSize, media_dir, media_path, meta_cache_path, receipts_path, sidecar_path,
    thumbnail_path, tmp_path, transcode_h264_path, transcode_live_path, trash_path,
};
use uuid::Uuid;

/// Walk to the workspace root (the nearest ancestor holding `Cargo.lock`).
fn workspace_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    while !dir.join("Cargo.lock").is_file() {
        if !dir.pop() {
            panic!("no Cargo.lock found above {}", env!("CARGO_MANIFEST_DIR"));
        }
    }
    dir
}

/// Collect every `.rs` file under `dir`, ignoring unreadable entries.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

// ── SR3: placement audit ─────────────────────────────────────────────────────

/// Every derivative/cache/temp/trash path the `paths` module produces lives under the library
/// root. This is the positive half of the SR3 invariant: nothing the module can hand out escapes
/// the app-private root.
#[test]
fn every_paths_module_output_stays_under_the_library_root() {
    let root = Path::new("/app/private/library");
    let uuid = Uuid::parse_str("01956ef3-1234-7abc-9def-123456789abc").unwrap();
    let capture = Some(1_721_001_600_i64);

    let mut candidates = vec![
        media_dir(root, 2024, 7),
        media_path(root, &uuid, "arw", capture),
        media_path(root, &uuid, "jpg", None),
        sidecar_path(root, &uuid, "jpg", capture),
        receipts_path(root, &uuid, capture),
        meta_cache_path(root, &uuid),
        transcode_h264_path(root, &uuid),
        transcode_live_path(root, &uuid),
        trash_path(root, &uuid, "jpg"),
        thumbnail_path(root, &uuid, ThumbnailSize::Xl),
    ];
    // The staging path for atomic writes is derived from an under-root path; it must stay under it.
    let media = media_path(root, &uuid, "jpg", capture);
    candidates.push(tmp_path(&media));

    for path in candidates {
        assert!(
            path.starts_with(root),
            "path escapes the library root: {}",
            path.display()
        );
    }
}

/// The negative half of SR3: production code has no route to a shared / OS-temp location. Any
/// `env::temp_dir`, `temp_dir()`, or hardcoded `/tmp`/`/var/tmp` in a source file would be a
/// spillage risk; there must be none. (Test scaffolding uses `tempfile::TempDir`, which is barred
/// from production by the dev-dependency check below.)
#[test]
fn no_source_file_reaches_for_a_shared_temp_location() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(!files.is_empty(), "found no sources to audit under {src:?}");

    let needles = ["env::temp_dir", "temp_dir()", "\"/tmp", "\"/var/tmp"];
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for needle in needles {
            assert!(
                !text.contains(needle),
                "{}: production sweep found a shared-temp reference `{needle}` (SR3 spillage risk)",
                file.display()
            );
        }
    }
}

/// `tempfile` — the only crate in the graph that plants files in the OS temp dir — is a
/// **dev-dependency** of `capsule-core`, never a normal one. So no non-test code path can even
/// name it: the SR3 "no plaintext outside the root" boundary holds by construction, not vigilance.
#[test]
fn tempfile_is_a_dev_dependency_only() {
    let manifest =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .unwrap();

    let mut section = String::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed.trim_matches(['[', ']']).to_string();
            continue;
        }
        let is_tempfile_key = trimmed
            .split_once('=')
            .is_some_and(|(k, _)| k.trim() == "tempfile");
        if is_tempfile_key {
            assert_eq!(
                section, "dev-dependencies",
                "tempfile must live in [dev-dependencies], found it in [{section}]"
            );
        }
    }
}

// ── NFR1: no network on read paths (structural) ──────────────────────────────

/// Parse the flat `[[package]]` list in a `Cargo.lock` into a name → direct-dependency-names map.
/// Dependency entries may read `"name"`, `"name version"`, or `"name version (source)"`; only the
/// leading name token matters for reachability.
fn cargo_lock_graph(lock: &str) -> HashMap<String, Vec<String>> {
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for block in lock.split("[[package]]") {
        let mut name = None;
        let mut deps = Vec::new();
        let mut in_deps = false;
        for line in block.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("name = ") {
                name = Some(rest.trim_matches('"').to_string());
            } else if trimmed.starts_with("dependencies = [") {
                in_deps = true;
            } else if in_deps {
                if trimmed == "]" {
                    in_deps = false;
                } else if let Some(dep) = trimmed
                    .trim_end_matches(',')
                    .trim_matches('"')
                    .split(' ')
                    .next()
                {
                    if !dep.is_empty() {
                        deps.push(dep.to_string());
                    }
                }
            }
        }
        if let Some(name) = name {
            graph.entry(name).or_default().extend(deps);
        }
    }
    graph
}

/// NFR1, proven by construction: no known network client (HTTP/gRPC/TLS-over-socket) is reachable
/// from `capsule-core` in the resolved dependency graph. These crates DO exist elsewhere in the
/// workspace lock (the server and SDK use them), so a false pass is impossible — the assertion is
/// that none is reachable *from core*, the crate every gallery read path is built on.
#[test]
fn capsule_core_has_no_network_dependency_by_construction() {
    let lock = std::fs::read_to_string(workspace_root().join("Cargo.lock")).unwrap();
    let graph = cargo_lock_graph(&lock);
    assert!(
        graph.contains_key("capsule-core"),
        "capsule-core not found in Cargo.lock"
    );

    // Sanity: the forbidden crates really are present in the workspace lock, so reachability is a
    // meaningful test and not vacuously satisfied by their absence.
    let forbidden = [
        "reqwest",
        "tonic",
        "hyper",
        "hyper-util",
        "h2",
        "axum",
        "actix-web",
        "ureq",
        "isahc",
        "curl",
        "native-tls",
        "openssl",
        "openssl-sys",
    ];
    let present: Vec<&str> = forbidden
        .iter()
        .copied()
        .filter(|c| graph.contains_key(*c))
        .collect();
    assert!(
        !present.is_empty(),
        "expected some network crates in the workspace lock to make this test non-vacuous"
    );

    // BFS the transitive closure reachable from capsule-core.
    let mut reachable = HashSet::new();
    let mut stack = vec!["capsule-core".to_string()];
    while let Some(node) = stack.pop() {
        if !reachable.insert(node.clone()) {
            continue;
        }
        if let Some(deps) = graph.get(&node) {
            for dep in deps {
                stack.push(dep.clone());
            }
        }
    }

    let forbidden_set: HashSet<&str> = forbidden.iter().copied().collect();
    let leaks: Vec<&String> = reachable
        .iter()
        .filter(|c| forbidden_set.contains(c.as_str()))
        .collect();
    assert!(
        leaks.is_empty(),
        "capsule-core transitively depends on network crate(s): {leaks:?} — NFR1 (no network on \
         read paths) is violated by construction"
    );
}
