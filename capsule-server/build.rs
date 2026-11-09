//! Build-time generation of the egress pull worker's client: the server's **own copy** of the
//! generated calls a peer pull makes (#1116).
//!
//! A federated pull speaks exactly the primitives a client speaks (design/federation.md, "No new
//! data protocol"), so the calls it makes are the ones `capsule-sdk` generates from the same
//! committed `openapi.json`. The SDK cannot be linked here: it pulls in `capsule-library` and
//! `capsule-core`'s `native` feature, and the key-free server links neither outside its
//! dev-dependencies. So `spargen` lowers the same document a second time, narrowed to the three
//! operations a pull presents a capability to — `GET /v1/sync`, `GET /v1/blob/{hash}` and
//! `POST /v1/federation/capabilities/refresh` — and everything that parses or serializes on the
//! pull path is generated, exactly as it is in the SDK.
//!
//! The narrowing is computed from the document rather than listed: every other path is omitted,
//! so an operation the contract adds later is not silently compiled into the server's egress.

use std::path::PathBuf;

/// The operations the pull worker calls, as `(method, path)` in the document.
const KEPT: &[(&str, &str)] = &[
    ("get", "/v1/sync"),
    ("get", "/v1/blob/{hash}"),
    ("post", "/v1/federation/capabilities/refresh"),
];

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let spec = manifest_dir.join("openapi.json");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"))
        .join("peer_client.rs");
    println!("cargo:rerun-if-changed={}", spec.display());
    println!("cargo:rerun-if-changed=build.rs");

    let document: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&spec).expect("the committed openapi.json is readable"),
    )
    .expect("the committed openapi.json is JSON");
    let paths = document
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .expect("the document declares paths");

    let mut rules = Vec::new();
    for (path, item) in paths {
        let kept: Vec<&str> = KEPT
            .iter()
            .filter(|(_, kept_path)| kept_path == path)
            .map(|(method, _)| *method)
            .collect();
        if kept.is_empty() {
            rules.push(spargen::OmitRule::path(path.clone()));
            continue;
        }
        let methods = item.as_object().expect("a path item is an object");
        for method in methods.keys() {
            if kept.contains(&method.as_str()) {
                continue;
            }
            if let Some(method) = omit_method(method) {
                rules.push(spargen::OmitRule::operation(method, path.clone()));
            }
        }
    }
    for (method, path) in KEPT {
        assert!(
            paths
                .get(*path)
                .and_then(|item| item.get(*method))
                .is_some(),
            "the pull worker calls {method} {path}, and the committed document no longer declares it"
        );
    }

    let spec = spec
        .into_os_string()
        .into_string()
        .expect("UTF-8 spec path");
    let out = out
        .into_os_string()
        .into_string()
        .expect("UTF-8 OUT_DIR path");
    // `uuid` and `time` off: the three operations carry neither format, and leaving them on would
    // add two crates to the server's runtime tree for nothing.
    let narrowed = rules.into_iter().fold(
        spargen::Spec::new(spec.clone()).uuid(false).time(false),
        spargen::Spec::omit_rule,
    );
    let report = spargen::generate(&narrowed.build(out));
    assert!(
        matches!(
            report.outcome(),
            spargen::Outcome::Generated | spargen::Outcome::Cached
        ),
        "spargen could not generate the pull worker's client from {spec}: {report:#?}"
    );
}

/// A path item's method key as an omit rule's method, or `None` for a key that is not one
/// (`parameters`, `summary`, …).
fn omit_method(key: &str) -> Option<spargen::OmitMethod> {
    Some(match key {
        "get" => spargen::OmitMethod::Get,
        "put" => spargen::OmitMethod::Put,
        "post" => spargen::OmitMethod::Post,
        "delete" => spargen::OmitMethod::Delete,
        "options" => spargen::OmitMethod::Options,
        "head" => spargen::OmitMethod::Head,
        "patch" => spargen::OmitMethod::Patch,
        "trace" => spargen::OmitMethod::Trace,
        "query" => spargen::OmitMethod::Query,
        _ => return None,
    })
}
