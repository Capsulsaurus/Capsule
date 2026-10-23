use std::sync::Arc;

use capsule_core::crypto::keys::{P256HybridSigningKey, Signer as _};

use super::*;

/// Real-TPM smoke, mirroring the tss-esapi reference round trip (`README-tpm.md`) and the
/// Secure Enclave / StrongBox composition smoke: enroll a TPM-held P-256
/// key, compose it through `P256HybridSigningKey`, sign, verify, and assert
/// non-exportability. Gated on a present TPM, so it no-ops on machines without one.
#[test]
fn windows_tpm_signer_composes_through_p256_hybrid() {
    let Some(signer) = TbsTpmSigner::open_if_present() else {
        return; // no TPM on this host — the CI lane with a TPM exercises the real path
    };
    let signer = Arc::new(signer);
    let alias = "capsule-smoke-dsk".to_string();

    // Standalone HardwareSigner round trip.
    let point = signer.enroll(alias.clone()).expect("enroll");
    assert_eq!(point.len(), 64, "bare x‖y P-256 point");
    signer
        .assert_non_exportable(alias.clone())
        .expect("fixedTPM|fixedParent");

    // Composed through the P-256 hybrid DSK exactly like the SE/StrongBox adapters.
    let hybrid = P256HybridSigningKey::enroll(signer.clone(), alias, &[3u8; 32])
        .expect("compose P-256 hybrid");
    let msg = b"asset manifest bytes";
    let sig = hybrid.sign(msg).expect("sign");
    assert!(
        hybrid.verifying_key().verify(msg, &sig),
        "the TPM-composed hybrid signature verifies against the published key"
    );
    assert!(!hybrid.verifying_key().verify(b"tampered", &sig));
}
