use capsule_core::client_build::ClientVersion;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_library::lifecycle::Workspace;

use super::as_capsule_cli;

/// A workspace built the way the CLI builds one (`as_capsule_cli`, the same wrapper the
/// `import`/`demo` paths use) stamps every manifest it authors with
/// `capsule-cli/{semver}+{commit}` — proving the CLI reports itself, not the bare
/// `capsule-core` default. Uses a fast Argon2 cost so the test never pays the production KDF.
#[test]
fn cli_workspace_stamps_capsule_cli_client_version() {
    let dir = std::env::temp_dir().join(format!("capsule-cli-scratch-{}", nanoid::nanoid!()));
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let src = dir.join("photo.jpg");
    std::fs::write(&src, b"\xFF\xD8\xFF cli wiring bytes").unwrap();

    let mut ws = as_capsule_cli(
        Workspace::create_with_params(
            &lib,
            b"pw",
            Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .unwrap(),
    );
    let album = ws.default_album_id();
    ws.create_album_with_id(album, "Imports").unwrap();
    let id = ws.import_asset(album, &src).unwrap();

    let cv = ws
        .asset(&id)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .manifest
        .core
        .client_version
        .clone();
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        cv.starts_with("capsule-cli/"),
        "the CLI must author capsule-cli/..., got {cv}"
    );
    let parsed = ClientVersion::parse(&cv).expect("CLI client_version must parse the grammar");
    assert_eq!(parsed.client_id, "capsule-cli");
    assert_eq!(parsed.semver, env!("CARGO_PKG_VERSION"));
}
