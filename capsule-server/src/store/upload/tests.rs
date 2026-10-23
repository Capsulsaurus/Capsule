use super::BlobRole;

#[test]
fn every_role_reads_back_from_its_own_token() {
    // The adapters store the token and parse it back through `from_token`, so a role missing
    // from `ALL` is a role every stored record of it fails to decode.
    for role in BlobRole::ALL {
        assert_eq!(BlobRole::from_token(role.as_str()), Some(role));
    }
    assert_eq!(BlobRole::DerivativeManifest.as_str(), "derivative_manifest");
    assert_eq!(BlobRole::from_token("derivativemanifest"), None);
}
