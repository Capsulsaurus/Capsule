use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};

use super::*;

// ── Synthetic-response builders (mirror what a TPM returns over TBS) ────────────────────

fn resp_header(tag: u16, code: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_u16(&mut out, tag);
    push_u32(&mut out, (10 + body.len()) as u32);
    push_u32(&mut out, code);
    out.extend_from_slice(body);
    out
}

fn synth_read_public(point: &[u8; 64], attributes: u32) -> Vec<u8> {
    // TPMT_PUBLIC for a P-256 signing key with the given attributes + point.
    let mut tpmt = Vec::new();
    push_u16(&mut tpmt, TPM_ALG_ECC);
    push_u16(&mut tpmt, TPM_ALG_SHA256);
    push_u32(&mut tpmt, attributes);
    push_tpm2b(&mut tpmt, &[]); // authPolicy
    push_u16(&mut tpmt, TPM_ALG_NULL); // symmetric
    push_u16(&mut tpmt, TPM_ALG_ECDSA); // scheme
    push_u16(&mut tpmt, TPM_ALG_SHA256);
    push_u16(&mut tpmt, TPM_ECC_NIST_P256); // curve
    push_u16(&mut tpmt, TPM_ALG_NULL); // kdf
    push_tpm2b(&mut tpmt, &point[..32]); // x
    push_tpm2b(&mut tpmt, &point[32..]); // y

    let mut body = Vec::new();
    push_tpm2b(&mut body, &tpmt); // TPM2B_PUBLIC
    push_tpm2b(&mut body, &[]); // name
    push_tpm2b(&mut body, &[]); // qualifiedName
    resp_header(TPM_ST_NO_SESSIONS, 0, &body)
}

fn synth_sign(sig: &Signature) -> Vec<u8> {
    let bytes = sig.to_bytes(); // 64-byte r‖s
    let mut sig_area = Vec::new();
    push_u16(&mut sig_area, TPM_ALG_ECDSA);
    push_u16(&mut sig_area, TPM_ALG_SHA256);
    push_tpm2b(&mut sig_area, &bytes[..32]); // r
    push_tpm2b(&mut sig_area, &bytes[32..]); // s

    let mut body = Vec::new();
    push_u32(&mut body, sig_area.len() as u32); // parameterSize
    body.extend_from_slice(&sig_area);
    resp_header(TPM_ST_SESSIONS, 0, &body)
}

fn synth_hash(digest: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    push_tpm2b(&mut body, digest); // outHash
    // TPMT_TK_HASHCHECK: tag, hierarchy, digest.
    push_u16(&mut body, TPM_ST_HASHCHECK);
    push_u32(&mut body, TPM_RH_OWNER);
    push_tpm2b(&mut body, b"ticket-bytes");
    resp_header(TPM_ST_NO_SESSIONS, 0, &body)
}

// ── Command framing ────────────────────────────────────────────────────────────────────

#[test]
fn read_public_command_is_framed_without_sessions() {
    let cmd = build_read_public(0x8100_0042);
    assert_eq!(&cmd[..2], &[0x80, 0x01], "TPM_ST_NO_SESSIONS");
    assert_eq!(
        u32::from_be_bytes([cmd[2], cmd[3], cmd[4], cmd[5]]) as usize,
        cmd.len(),
        "commandSize equals the buffer length"
    );
    assert_eq!(&cmd[6..10], &[0x00, 0x00, 0x01, 0x73], "TPM_CC_ReadPublic");
    assert_eq!(&cmd[10..14], &0x8100_0042u32.to_be_bytes(), "object handle");
}

#[test]
fn sign_command_uses_sessions_and_the_key_scheme() {
    let cmd = build_sign(0x8100_0001, &[0xAB; 32], b"ticket");
    assert_eq!(&cmd[..2], &[0x80, 0x02], "TPM_ST_SESSIONS (auth present)");
    assert_eq!(&cmd[6..10], &[0x00, 0x00, 0x01, 0x5D], "TPM_CC_Sign");
    assert_eq!(&cmd[10..14], &0x8100_0001u32.to_be_bytes(), "key handle");
    // TPM_RS_PW appears in the authorization area.
    assert!(
        cmd.windows(4).any(|w| w == 0x4000_0009u32.to_be_bytes()),
        "password session handle present"
    );
}

#[test]
fn creation_and_lifecycle_commands_build() {
    // Exercise every builder so the wire codec has no dead paths and the framing is sane.
    for cmd in [
        build_create_primary(),
        build_create(0x8000_0000),
        build_load(0x8000_0000, b"\x00\x02pv", b"\x00\x02pb"),
        build_evict_control(0x8000_0001, 0x8100_0000),
        build_flush_context(0x8000_0001),
        build_hash(b"payload"),
    ] {
        assert!(cmd.len() >= 10);
        assert_eq!(
            u32::from_be_bytes([cmd[2], cmd[3], cmd[4], cmd[5]]) as usize,
            cmd.len()
        );
    }
}

#[test]
fn signing_template_declares_ecdsa_p256_and_hardware_binding() {
    let t = ecc_signing_template();
    assert_eq!(&t[..2], &TPM_ALG_ECC.to_be_bytes());
    let attrs = u32::from_be_bytes([t[4], t[5], t[6], t[7]]);
    assert!(attrs & TPMA_SIGN != 0, "sign key");
    assert!(
        attrs & TPMA_FIXED_TPM != 0 && attrs & TPMA_FIXED_PARENT != 0,
        "non-exportable (fixedTPM|fixedParent)"
    );
    assert!(attrs & TPMA_SENSITIVE_DATA_ORIGIN != 0, "TPM-generated");
    assert!(attrs & TPMA_RESTRICTED == 0, "unrestricted signing key");
    // The scheme is ECDSA over SHA-256 and the curve is NIST P-256.
    assert!(t.windows(2).any(|w| w == TPM_ALG_ECDSA.to_be_bytes()));
    assert!(t.windows(2).any(|w| w == TPM_ECC_NIST_P256.to_be_bytes()));
    // The storage parent is a restricted decrypt key.
    let s = ecc_storage_template();
    let s_attrs = u32::from_be_bytes([s[4], s[5], s[6], s[7]]);
    assert!(s_attrs & TPMA_RESTRICTED != 0 && s_attrs & TPMA_DECRYPT != 0);
}

// ── Response parsing ───────────────────────────────────────────────────────────────────

#[test]
fn read_public_round_trips_the_point_and_attributes() {
    let point = {
        let sk = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let enc = sk.verifying_key().to_encoded_point(false);
        let mut p = [0u8; 64];
        p[..32].copy_from_slice(enc.x().unwrap());
        p[32..].copy_from_slice(enc.y().unwrap());
        p
    };
    let attrs = (1 << 1) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 18); // fixed*, origin, sign
    let parsed = parse_read_public(&synth_read_public(&point, attrs)).unwrap();
    assert_eq!(parsed.point, point, "x‖y survives the round trip");
    assert!(
        is_non_exportable(parsed.attributes),
        "fixedTPM|fixedParent set"
    );

    // An exportable key (fixedTPM|fixedParent clear) is rejected.
    let exportable = parse_read_public(&synth_read_public(&point, 1 << 18)).unwrap();
    assert!(!is_non_exportable(exportable.attributes));

    // The 64-byte bare point is a valid SEC1 P-256 public key (`0x04‖x‖y`) — exactly what
    // the P-256 composition normalizes.
    let mut sec1 = vec![0x04];
    sec1.extend_from_slice(&parsed.point);
    assert!(VerifyingKey::from_sec1_bytes(&sec1).is_ok());
}

#[test]
fn sign_response_reencodes_raw_rs_to_verifiable_der() {
    // The load-bearing conversion: a TPM returns raw r‖s; the adapter must hand
    // P256HybridSigningKey a DER signature that verifies against the P-256 public key.
    let sk = SigningKey::from_slice(&[9u8; 32]).unwrap();
    let vk: &VerifyingKey = sk.verifying_key();
    let msg = b"asset manifest bytes";
    let sig: Signature = sk.sign(msg);

    let der = ecdsa_der_from_sign_response(&synth_sign(&sig)).unwrap();
    let recovered = Signature::from_der(&der).expect("valid DER");
    assert!(
        vk.verify(msg, &recovered).is_ok(),
        "the re-encoded DER signature verifies against the P-256 key"
    );
}

#[test]
fn hash_response_yields_digest_and_ticket_for_sign() {
    let digest = [0xEE; 32];
    let (got_digest, ticket) = parse_hash_response(&synth_hash(&digest)).unwrap();
    assert_eq!(got_digest, digest);
    // The ticket carries its TPMT_TK_HASHCHECK tag and is fed straight into Sign.
    assert_eq!(&ticket[..2], &TPM_ST_HASHCHECK.to_be_bytes());
    let sign_cmd = build_sign(0x8100_0001, &got_digest, &ticket);
    assert!(
        sign_cmd.windows(digest.len()).any(|w| w == digest),
        "the digest is embedded in the Sign command"
    );
}

#[test]
fn create_response_blobs_re_marshal_into_load() {
    let mut body = Vec::new();
    push_u32(&mut body, 0); // parameterSize placeholder; fixed below
    let mut params = Vec::new();
    push_tpm2b(&mut params, b"private-blob"); // outPrivate
    push_tpm2b(&mut params, b"public-blob"); // outPublic
    push_tpm2b(&mut params, b"creation"); // trailing creationData (ignored)
    body = {
        let mut b = Vec::new();
        push_u32(&mut b, params.len() as u32);
        b.extend_from_slice(&params);
        b
    };
    let resp = resp_header(TPM_ST_SESSIONS, 0, &body);

    let (out_private, out_public) = parse_create_response(&resp).unwrap();
    assert_eq!(&out_private, b"\x00\x0Cprivate-blob"); // length-prefixed
    assert_eq!(&out_public, b"\x00\x0Bpublic-blob");
    let load = build_load(0x8000_0000, &out_private, &out_public);
    assert!(load.windows(out_private.len()).any(|w| w == out_private));
    assert!(load.windows(out_public.len()).any(|w| w == out_public));
}

#[test]
fn nonzero_response_code_is_surfaced_as_backend_error() {
    let resp = resp_header(TPM_ST_NO_SESSIONS, 0x0000_0101, &[]); // TPM_RC_FAILURE-ish
    assert!(check_response(&resp).is_err());
    assert!(parse_read_public(&resp).is_err());
    assert!(
        response_first_handle(&[0x80, 0x01]).is_err(),
        "short response"
    );
}

#[test]
fn persistent_handles_are_stable_and_alias_specific() {
    let a = persistent_handle("device-dsk");
    assert_eq!(
        a,
        persistent_handle("device-dsk"),
        "deterministic per alias"
    );
    assert_ne!(a, persistent_handle("other"), "distinct aliases differ");
    assert!(
        (0x8100_0000..=0x817F_FFFF).contains(&a),
        "in the owner persistent range"
    );
}
