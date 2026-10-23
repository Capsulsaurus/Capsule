//! Windows TPM 2.0 [`HardwareSigner`](capsule_core::crypto::keys::HardwareSigner) over **TBS** (TPM Base Services).
//!
//! The `tpm` reference adapter (`tpm`-feature-gated, so undocumented in a default build) drives a
//! TPM through `tss-esapi`'s high-level ESAPI and links the system `libtss2` — the Linux path.
//! Windows exposes the TPM through `tbs.dll` instead: a *raw command channel*.
//! [`Tbsip_Submit_Command`] takes a marshalled TPM 2.0 command byte-stream and returns the raw
//! response, with no ESAPI in between. This adapter therefore marshals the same key lifecycle the
//! reference performs — `CreatePrimary` → `Create` → `Load` → `EvictControl`, then `ReadPublic` /
//! `Hash` + `Sign` — directly to the wire and submits each through TBS.
//!
//! # P-256, composed
//!
//! Shipping TPMs expose **ECDSA over NIST P-256**, so — exactly like Secure Enclave and StrongBox
//! and the `tpm` reference — the classical half is P-256, and this signer plugs
//! into [`P256HybridSigningKey`](capsule_core::crypto::keys::P256HybridSigningKey) unchanged:
//! [`enroll`](capsule_core::crypto::keys::HardwareSigner::enroll) returns the bare `x‖y` public point (64 bytes — the
//! form `p256::parse_p256_public` normalizes), and [`sign_classical`] returns a **DER-encoded**
//! ECDSA signature (the composition's contract; the TPM emits raw `r‖s`, which this adapter
//! re-encodes). The ML-DSA-65 half stays software-sealed.
//!
//! # Non-exportability
//!
//! The signing key is created with `fixedTPM | fixedParent | sensitiveDataOrigin`, so its private
//! portion is generated inside the TPM and can never be duplicated out.
//! [`assert_non_exportable`](capsule_core::crypto::keys::HardwareSigner::assert_non_exportable) re-reads the public
//! area and confirms `fixedTPM`/`fixedParent` — the TBS analogue of the reference's check.
//!
//! # Testing
//!
//! The wire codec (command marshalling + response parsing) is pure and host-runnable: the mock
//! tests below build every command and round-trip synthetic responses on any host. The real-TPM
//! smoke (`windows_tpm_signer_composes_through_p256_hybrid`) is `#[cfg(windows)]` and gated on
//! [`Tbsi_Is_Tpm_Present`], mirroring the reference smoke in `README-tpm.md` and the Secure
//! Enclave availability gate.
//!
//! [`Tbsip_Submit_Command`]: https://learn.microsoft.com/windows/win32/api/tbs/nf-tbs-tbsip_submit_command
//! [`Tbsi_Is_Tpm_Present`]: https://learn.microsoft.com/windows/win32/api/tbs/nf-tbs-tbsi_is_tpm_present
//! [`sign_classical`]: capsule_core::crypto::keys::HardwareSigner::sign_classical

#[cfg(windows)]
pub use backend::TbsTpmSigner;

/// The pure TPM 2.0 wire codec — command marshalling and response parsing. No I/O, so it compiles
/// and runs on any host (the mock tests exercise it) and is the whole verifiable surface off
/// Windows. Compiled on Windows (the backend consumes it) and under `cfg(test)`.
#[cfg(any(windows, test))]
mod wire {
    use capsule_core::crypto::keys::HardwareSignerError;
    use p256::ecdsa::Signature;
    use sha2::{Digest as _, Sha256};

    // ── TPM 2.0 structure tags (`TPM_ST`) ──────────────────────────────────────────────────────
    const TPM_ST_NO_SESSIONS: u16 = 0x8001;
    const TPM_ST_SESSIONS: u16 = 0x8002;
    /// Ticket-structure tag — the backend reads the `TPMT_TK_HASHCHECK` verbatim from the `Hash`
    /// response and never constructs one, so only the synthetic-response tests reference this.
    #[cfg(test)]
    const TPM_ST_HASHCHECK: u16 = 0x8024;

    // ── Command codes (`TPM_CC`) ───────────────────────────────────────────────────────────────
    const TPM_CC_EVICT_CONTROL: u32 = 0x0000_0120;
    const TPM_CC_CREATE_PRIMARY: u32 = 0x0000_0131;
    const TPM_CC_CREATE: u32 = 0x0000_0153;
    const TPM_CC_LOAD: u32 = 0x0000_0157;
    const TPM_CC_SIGN: u32 = 0x0000_015D;
    const TPM_CC_FLUSH_CONTEXT: u32 = 0x0000_0165;
    const TPM_CC_READ_PUBLIC: u32 = 0x0000_0173;
    const TPM_CC_HASH: u32 = 0x0000_017D;

    // ── Permanent handles ──────────────────────────────────────────────────────────────────────
    const TPM_RH_OWNER: u32 = 0x4000_0001;
    const TPM_RS_PW: u32 = 0x4000_0009;

    // ── Algorithm identifiers (`TPM_ALG`) + curve ──────────────────────────────────────────────
    const TPM_ALG_AES: u16 = 0x0006;
    const TPM_ALG_SHA256: u16 = 0x000B;
    const TPM_ALG_NULL: u16 = 0x0010;
    const TPM_ALG_ECDSA: u16 = 0x0018;
    const TPM_ALG_ECC: u16 = 0x0023;
    const TPM_ALG_CFB: u16 = 0x0043;
    const TPM_ECC_NIST_P256: u16 = 0x0003;

    // ── Object attributes (`TPMA_OBJECT`) ──────────────────────────────────────────────────────
    const TPMA_FIXED_TPM: u32 = 1 << 1;
    const TPMA_FIXED_PARENT: u32 = 1 << 4;
    const TPMA_SENSITIVE_DATA_ORIGIN: u32 = 1 << 5;
    const TPMA_USER_WITH_AUTH: u32 = 1 << 6;
    const TPMA_RESTRICTED: u32 = 1 << 16;
    const TPMA_DECRYPT: u32 = 1 << 17;
    const TPMA_SIGN: u32 = 1 << 18;

    /// Base of the owner persistent-handle range (`0x8100_0000`), mirroring [`super::super::tpm`].
    const PERSISTENT_BASE: u32 = 0x8100_0000;

    fn err(msg: impl Into<String>) -> HardwareSignerError {
        HardwareSignerError::Backend(msg.into())
    }

    // ── Little marshalling helpers ─────────────────────────────────────────────────────────────
    fn push_u16(buf: &mut Vec<u8>, v: u16) {
        buf.extend_from_slice(&v.to_be_bytes());
    }
    fn push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_be_bytes());
    }
    /// A `TPM2B` (a `u16` big-endian length prefix followed by the bytes).
    fn push_tpm2b(buf: &mut Vec<u8>, data: &[u8]) {
        push_u16(buf, data.len() as u16);
        buf.extend_from_slice(data);
    }

    /// The empty-password authorization area: one `TPMS_AUTH_COMMAND` for the password session
    /// (`TPM_RS_PW`) with empty nonce, `continueSession`, and empty HMAC/password — the owner
    /// hierarchy's common empty-auth case the reference also assumes.
    fn password_auth_area() -> Vec<u8> {
        let mut a = Vec::new();
        push_u32(&mut a, TPM_RS_PW);
        push_tpm2b(&mut a, &[]); // nonce
        a.push(0x01); // sessionAttributes: continueSession
        push_tpm2b(&mut a, &[]); // hmac / password
        a
    }

    /// Frame a command: `tag ‖ commandSize ‖ commandCode ‖ handles ‖ [authSize ‖ auth] ‖ params`.
    fn frame(cc: u32, handles: &[u32], auth: Option<&[u8]>, params: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        for &h in handles {
            push_u32(&mut body, h);
        }
        if let Some(a) = auth {
            push_u32(&mut body, a.len() as u32);
            body.extend_from_slice(a);
        }
        body.extend_from_slice(params);

        let tag = if auth.is_some() {
            TPM_ST_SESSIONS
        } else {
            TPM_ST_NO_SESSIONS
        };
        let mut out = Vec::with_capacity(10 + body.len());
        push_u16(&mut out, tag);
        push_u32(&mut out, (10 + body.len()) as u32);
        push_u32(&mut out, cc);
        out.extend_from_slice(&body);
        out
    }

    // ── Public-area templates (`TPMT_PUBLIC`) ──────────────────────────────────────────────────

    /// The restricted ECC-P256 storage-parent template (owner hierarchy), matching the reference's
    /// `primary_template`.
    pub(super) fn ecc_storage_template() -> Vec<u8> {
        let attrs = TPMA_FIXED_TPM
            | TPMA_FIXED_PARENT
            | TPMA_SENSITIVE_DATA_ORIGIN
            | TPMA_USER_WITH_AUTH
            | TPMA_RESTRICTED
            | TPMA_DECRYPT;
        let mut t = Vec::new();
        push_u16(&mut t, TPM_ALG_ECC);
        push_u16(&mut t, TPM_ALG_SHA256);
        push_u32(&mut t, attrs);
        push_tpm2b(&mut t, &[]); // authPolicy
        // TPMS_ECC_PARMS: symmetric AES-128-CFB, scheme NULL, curve P-256, kdf NULL.
        push_u16(&mut t, TPM_ALG_AES);
        push_u16(&mut t, 128);
        push_u16(&mut t, TPM_ALG_CFB);
        push_u16(&mut t, TPM_ALG_NULL);
        push_u16(&mut t, TPM_ECC_NIST_P256);
        push_u16(&mut t, TPM_ALG_NULL);
        // unique TPMS_ECC_POINT: empty x, empty y.
        push_tpm2b(&mut t, &[]);
        push_tpm2b(&mut t, &[]);
        t
    }

    /// The unrestricted ECDSA-P256 device signing-key template, matching the reference's
    /// `signing_key_template` (`fixedTPM | fixedParent | sensitiveDataOrigin | userWithAuth |
    /// sign`).
    pub(super) fn ecc_signing_template() -> Vec<u8> {
        let attrs = TPMA_FIXED_TPM
            | TPMA_FIXED_PARENT
            | TPMA_SENSITIVE_DATA_ORIGIN
            | TPMA_USER_WITH_AUTH
            | TPMA_SIGN;
        let mut t = Vec::new();
        push_u16(&mut t, TPM_ALG_ECC);
        push_u16(&mut t, TPM_ALG_SHA256);
        push_u32(&mut t, attrs);
        push_tpm2b(&mut t, &[]); // authPolicy
        // TPMS_ECC_PARMS: symmetric NULL, scheme ECDSA/SHA-256, curve P-256, kdf NULL.
        push_u16(&mut t, TPM_ALG_NULL);
        push_u16(&mut t, TPM_ALG_ECDSA);
        push_u16(&mut t, TPM_ALG_SHA256);
        push_u16(&mut t, TPM_ECC_NIST_P256);
        push_u16(&mut t, TPM_ALG_NULL);
        push_tpm2b(&mut t, &[]);
        push_tpm2b(&mut t, &[]);
        t
    }

    // ── Command builders ───────────────────────────────────────────────────────────────────────

    /// An empty `TPM2B_SENSITIVE_CREATE` (empty `userAuth`, empty `data`).
    fn empty_sensitive_create() -> Vec<u8> {
        let mut inner = Vec::new();
        push_tpm2b(&mut inner, &[]);
        push_tpm2b(&mut inner, &[]);
        let mut sens = Vec::new();
        push_tpm2b(&mut sens, &inner);
        sens
    }

    pub(super) fn build_create_primary() -> Vec<u8> {
        let mut params = empty_sensitive_create();
        push_tpm2b(&mut params, &ecc_storage_template()); // inPublic
        push_tpm2b(&mut params, &[]); // outsideInfo
        push_u32(&mut params, 0); // creationPCR: TPML_PCR_SELECTION count = 0
        frame(
            TPM_CC_CREATE_PRIMARY,
            &[TPM_RH_OWNER],
            Some(&password_auth_area()),
            &params,
        )
    }

    pub(super) fn build_create(parent: u32) -> Vec<u8> {
        let mut params = empty_sensitive_create();
        push_tpm2b(&mut params, &ecc_signing_template()); // inPublic
        push_tpm2b(&mut params, &[]); // outsideInfo
        push_u32(&mut params, 0); // creationPCR
        frame(
            TPM_CC_CREATE,
            &[parent],
            Some(&password_auth_area()),
            &params,
        )
    }

    /// `in_private` / `in_public` are the `TPM2B_PRIVATE` / `TPM2B_PUBLIC` blobs (length prefix
    /// included) copied verbatim from the `Create` response.
    pub(super) fn build_load(parent: u32, in_private: &[u8], in_public: &[u8]) -> Vec<u8> {
        let mut params = Vec::new();
        params.extend_from_slice(in_private);
        params.extend_from_slice(in_public);
        frame(TPM_CC_LOAD, &[parent], Some(&password_auth_area()), &params)
    }

    pub(super) fn build_evict_control(object: u32, persistent: u32) -> Vec<u8> {
        let mut params = Vec::new();
        push_u32(&mut params, persistent); // persistentHandle
        frame(
            TPM_CC_EVICT_CONTROL,
            &[TPM_RH_OWNER, object],
            Some(&password_auth_area()),
            &params,
        )
    }

    pub(super) fn build_flush_context(handle: u32) -> Vec<u8> {
        let mut params = Vec::new();
        push_u32(&mut params, handle);
        frame(TPM_CC_FLUSH_CONTEXT, &[], None, &params)
    }

    pub(super) fn build_read_public(object: u32) -> Vec<u8> {
        frame(TPM_CC_READ_PUBLIC, &[object], None, &[])
    }

    /// Hash `data` inside the TPM under the owner hierarchy so `Sign` gets its validation ticket.
    /// `MaxBuffer` caps a single hash at 1024 bytes; longer inputs need a hash sequence — the same
    /// caveat the reference documents.
    pub(super) fn build_hash(data: &[u8]) -> Vec<u8> {
        let mut params = Vec::new();
        push_tpm2b(&mut params, data); // TPM2B_MAX_BUFFER
        push_u16(&mut params, TPM_ALG_SHA256);
        push_u32(&mut params, TPM_RH_OWNER); // hierarchy
        frame(TPM_CC_HASH, &[], None, &params)
    }

    /// Sign `digest` with the key at `key_handle`, using the key's own ECDSA scheme (`inScheme`
    /// NULL). `ticket` is the `TPMT_TK_HASHCHECK` from the `Hash` response.
    pub(super) fn build_sign(key_handle: u32, digest: &[u8], ticket: &[u8]) -> Vec<u8> {
        let mut params = Vec::new();
        push_tpm2b(&mut params, digest); // TPM2B_DIGEST
        push_u16(&mut params, TPM_ALG_NULL); // inScheme TPMT_SIG_SCHEME = NULL
        params.extend_from_slice(ticket); // validation TPMT_TK_HASHCHECK
        frame(
            TPM_CC_SIGN,
            &[key_handle],
            Some(&password_auth_area()),
            &params,
        )
    }

    /// Map a key alias to a stable persistent handle in the owner range (mirrors the reference).
    pub(super) fn persistent_handle(alias: &str) -> u32 {
        let digest = Sha256::digest(alias.as_bytes());
        let offset = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) & 0x000F_FFFF;
        PERSISTENT_BASE + offset
    }

    // ── Response parsing ───────────────────────────────────────────────────────────────────────

    struct Reader<'a> {
        buf: &'a [u8],
        pos: usize,
    }

    impl<'a> Reader<'a> {
        fn new(buf: &'a [u8]) -> Self {
            Self { buf, pos: 0 }
        }
        fn need(&self, n: usize) -> Result<(), HardwareSignerError> {
            if self.pos.saturating_add(n) > self.buf.len() {
                Err(err("truncated TPM response"))
            } else {
                Ok(())
            }
        }
        fn u16(&mut self) -> Result<u16, HardwareSignerError> {
            self.need(2)?;
            let v = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
            self.pos += 2;
            Ok(v)
        }
        fn u32(&mut self) -> Result<u32, HardwareSignerError> {
            self.need(4)?;
            let v = u32::from_be_bytes([
                self.buf[self.pos],
                self.buf[self.pos + 1],
                self.buf[self.pos + 2],
                self.buf[self.pos + 3],
            ]);
            self.pos += 4;
            Ok(v)
        }
        fn take(&mut self, n: usize) -> Result<&'a [u8], HardwareSignerError> {
            self.need(n)?;
            let s = &self.buf[self.pos..self.pos + n];
            self.pos += n;
            Ok(s)
        }
        /// A `TPM2B`: read the `u16` length prefix, return the bytes after it.
        fn tpm2b(&mut self) -> Result<&'a [u8], HardwareSignerError> {
            let n = self.u16()? as usize;
            self.take(n)
        }
        /// A `TPM2B` returned *with* its length prefix (for verbatim re-marshalling into `Load`).
        fn tpm2b_prefixed(&mut self) -> Result<&'a [u8], HardwareSignerError> {
            let start = self.pos;
            let n = self.u16()? as usize;
            self.take(n)?;
            Ok(&self.buf[start..self.pos])
        }
        fn rest(&self) -> &'a [u8] {
            &self.buf[self.pos..]
        }
    }

    /// Validate a response header and return `(tag, body)` where `body` is everything after the
    /// 10-byte header. A non-zero `responseCode` maps to a backend error.
    pub(super) fn check_response(resp: &[u8]) -> Result<(u16, &[u8]), HardwareSignerError> {
        if resp.len() < 10 {
            return Err(err("short TPM response"));
        }
        let tag = u16::from_be_bytes([resp[0], resp[1]]);
        let size = u32::from_be_bytes([resp[2], resp[3], resp[4], resp[5]]) as usize;
        let code = u32::from_be_bytes([resp[6], resp[7], resp[8], resp[9]]);
        if code != 0 {
            return Err(err(format!("TPM error 0x{code:08x}")));
        }
        let end = size.clamp(10, resp.len());
        Ok((tag, &resp[10..end]))
    }

    /// The parameter area of a response: skips `num_handles` response handles and, for a
    /// `TPM_ST_SESSIONS` response, the `parameterSize` field and any trailing auth area.
    fn response_params(resp: &[u8], num_handles: usize) -> Result<&[u8], HardwareSignerError> {
        let (tag, body) = check_response(resp)?;
        let mut r = Reader::new(body);
        for _ in 0..num_handles {
            r.u32()?;
        }
        if tag == TPM_ST_SESSIONS {
            let psize = r.u32()? as usize;
            return r.take(psize);
        }
        Ok(r.rest())
    }

    /// The first response handle (the object handle of `CreatePrimary` / `Load`).
    pub(super) fn response_first_handle(resp: &[u8]) -> Result<u32, HardwareSignerError> {
        let (_tag, body) = check_response(resp)?;
        Reader::new(body).u32()
    }

    /// `(outPrivate, outPublic)` from a `Create` response, each with its length prefix so `Load`
    /// can re-marshal them verbatim.
    pub(super) fn parse_create_response(
        resp: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), HardwareSignerError> {
        let params = response_params(resp, 0)?;
        let mut r = Reader::new(params);
        let out_private = r.tpm2b_prefixed()?.to_vec();
        let out_public = r.tpm2b_prefixed()?.to_vec();
        Ok((out_private, out_public))
    }

    /// `(digest, ticket)` from a `Hash` response — the ticket is the raw `TPMT_TK_HASHCHECK`
    /// (`tag ‖ hierarchy ‖ digest`) fed straight back into `Sign`.
    pub(super) fn parse_hash_response(
        resp: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), HardwareSignerError> {
        let params = response_params(resp, 0)?;
        let mut r = Reader::new(params);
        let digest = r.tpm2b()?.to_vec();
        let start = r.pos;
        r.u16()?; // TPMT_TK_HASHCHECK.tag
        r.u32()?; // .hierarchy
        r.tpm2b()?; // .digest
        let ticket = params[start..r.pos].to_vec();
        Ok((digest, ticket))
    }

    /// A parsed ECC public area.
    pub(super) struct EccPublic {
        /// `TPMA_OBJECT` — checked for `fixedTPM`/`fixedParent` by the non-exportability assertion.
        pub(super) attributes: u32,
        /// The `x‖y` public point, each coordinate left-padded to 32 bytes (64 bytes total — the
        /// bare form [`capsule_core::crypto::keys::parse_p256_public`] normalizes).
        pub(super) point: [u8; 64],
    }

    /// Parse a `TPMT_PUBLIC` (ECC), extracting the object attributes and the `x‖y` point. Walks the
    /// selector-tagged `TPMS_ECC_PARMS` (symmetric, scheme, kdf) to reach the unique point.
    fn parse_ecc_public(tpmt: &[u8]) -> Result<EccPublic, HardwareSignerError> {
        let mut r = Reader::new(tpmt);
        if r.u16()? != TPM_ALG_ECC {
            return Err(err("not an ECC key"));
        }
        let _name_alg = r.u16()?;
        let attributes = r.u32()?;
        let _auth_policy = r.tpm2b()?;
        // symmetric TPMT_SYM_DEF_OBJECT: algorithm, then keyBits+mode unless NULL.
        if r.u16()? != TPM_ALG_NULL {
            r.u16()?;
            r.u16()?;
        }
        // scheme TPMT_ECC_SCHEME: algorithm, then hashAlg unless NULL.
        if r.u16()? != TPM_ALG_NULL {
            r.u16()?;
        }
        let _curve = r.u16()?;
        // kdf TPMT_KDF_SCHEME: algorithm, then hashAlg unless NULL.
        if r.u16()? != TPM_ALG_NULL {
            r.u16()?;
        }
        let x = r.tpm2b()?;
        let y = r.tpm2b()?;
        let mut point = [0u8; 64];
        point[..32].copy_from_slice(&left_pad_32(x)?);
        point[32..].copy_from_slice(&left_pad_32(y)?);
        Ok(EccPublic { attributes, point })
    }

    /// Parse the ECC public area from a `ReadPublic` response.
    pub(super) fn parse_read_public(resp: &[u8]) -> Result<EccPublic, HardwareSignerError> {
        let params = response_params(resp, 0)?;
        let mut r = Reader::new(params);
        let tpmt = r.tpm2b()?; // TPM2B_PUBLIC → TPMT_PUBLIC bytes
        parse_ecc_public(tpmt)
    }

    /// `true` iff the public area asserts `fixedTPM` and `fixedParent` (non-exportable).
    pub(super) fn is_non_exportable(attributes: u32) -> bool {
        attributes & TPMA_FIXED_TPM != 0 && attributes & TPMA_FIXED_PARENT != 0
    }

    fn left_pad_32(bytes: &[u8]) -> Result<[u8; 32], HardwareSignerError> {
        if bytes.len() > 32 {
            return Err(err("P-256 scalar longer than 32 bytes"));
        }
        let mut out = [0u8; 32];
        out[32 - bytes.len()..].copy_from_slice(bytes);
        Ok(out)
    }

    /// Re-encode the TPM's raw `TPMS_SIGNATURE_ECDSA` (`r‖s`) as DER — the form
    /// [`P256HybridSigningKey`](capsule_core::crypto::keys::P256HybridSigningKey) consumes.
    fn ecdsa_der_from_signature(sig_area: &[u8]) -> Result<Vec<u8>, HardwareSignerError> {
        let mut r = Reader::new(sig_area);
        if r.u16()? != TPM_ALG_ECDSA {
            return Err(err("unexpected TPM signature algorithm"));
        }
        let _hash = r.u16()?;
        let sig_r = left_pad_32(r.tpm2b()?)?;
        let sig_s = left_pad_32(r.tpm2b()?)?;
        let mut rs = [0u8; 64];
        rs[..32].copy_from_slice(&sig_r);
        rs[32..].copy_from_slice(&sig_s);
        let sig = Signature::from_slice(&rs).map_err(|_| err("invalid ECDSA signature"))?;
        Ok(sig.to_der().as_bytes().to_vec())
    }

    /// The DER-encoded ECDSA signature from a `Sign` response.
    pub(super) fn ecdsa_der_from_sign_response(
        resp: &[u8],
    ) -> Result<Vec<u8>, HardwareSignerError> {
        let params = response_params(resp, 0)?;
        ecdsa_der_from_signature(params)
    }

    #[cfg(test)]
    mod tests;
}

/// The Windows TBS transport + the [`HardwareSigner`] built on it.
#[cfg(windows)]
mod backend {
    use std::ffi::c_void;
    use std::sync::Mutex;

    use capsule_core::crypto::keys::{HardwareSigner, HardwareSignerError};
    use windows_sys::Win32::System::TpmBaseServices::{
        TBS_COMMAND_LOCALITY_ZERO, TBS_COMMAND_PRIORITY_NORMAL, TBS_CONTEXT_PARAMS,
        TBS_CONTEXT_PARAMS2, TBS_CONTEXT_VERSION_TWO, TBS_SUCCESS, Tbsi_Context_Create,
        Tbsi_Is_Tpm_Present, Tbsip_Context_Close, Tbsip_Submit_Command,
    };

    use super::wire;

    /// The maximum TPM command/response buffer (`TPM2_MAX_COMMAND_SIZE`).
    const MAX_RESPONSE: usize = 4096;

    /// A live TBS context handle. Access is serialized by the owning [`Mutex`], so the raw handle
    /// is safe to move across threads.
    struct TbsContext(*mut c_void);

    // SAFETY: the handle is only ever touched while holding the `TbsTpmSigner` mutex, so no two
    // threads use it concurrently; TBS itself permits use from any thread.
    unsafe impl Send for TbsContext {}

    impl Drop for TbsContext {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` is a live context from `Tbsi_Context_Create`, closed exactly once.
                unsafe {
                    Tbsip_Context_Close(self.0);
                }
            }
        }
    }

    /// A Windows TPM 2.0 [`HardwareSigner`] driven over TBS. The classical device signing key is an
    /// ECDSA-P256 key that lives in the TPM under a per-alias persistent handle and never leaves it.
    pub struct TbsTpmSigner {
        ctx: Mutex<TbsContext>,
    }

    impl TbsTpmSigner {
        /// Open a TPM 2.0 TBS context.
        pub fn open() -> Result<Self, HardwareSignerError> {
            // TBS_CONTEXT_PARAMS2, version TWO, requesting the TPM 2.0 interface (includeTpm20).
            // SAFETY: a zeroed all-integer/union POD is a valid initial value.
            let mut params: TBS_CONTEXT_PARAMS2 = unsafe { core::mem::zeroed() };
            params.version = TBS_CONTEXT_VERSION_TWO;
            params.Anonymous.asUINT32 = 0x0000_0004; // includeTpm20

            let mut handle: *mut c_void = core::ptr::null_mut();
            // SAFETY: `params` outlives the call; `handle` is a live out-pointer. The
            // TBS_CONTEXT_PARAMS2 pointer is reinterpreted as the versioned TBS_CONTEXT_PARAMS the
            // API dispatches on `version`.
            let rc = unsafe {
                Tbsi_Context_Create(
                    core::ptr::addr_of!(params).cast::<TBS_CONTEXT_PARAMS>(),
                    &mut handle,
                )
            };
            if rc != TBS_SUCCESS {
                return Err(HardwareSignerError::Backend(format!(
                    "TBS context create failed: 0x{rc:08x}"
                )));
            }
            Ok(Self {
                ctx: Mutex::new(TbsContext(handle)),
            })
        }

        /// Open a context only if a TPM is present; otherwise `None`. Drives the smoke-test gate,
        /// mirroring the Secure Enclave availability check.
        pub fn open_if_present() -> Option<Self> {
            // SAFETY: no arguments; queries TPM presence.
            if unsafe { Tbsi_Is_Tpm_Present() } == 0 {
                return None;
            }
            Self::open().ok()
        }

        /// Submit one marshalled TPM 2.0 command and return the raw response bytes.
        fn submit(&self, command: &[u8]) -> Result<Vec<u8>, HardwareSignerError> {
            let ctx = self.ctx.lock().expect("tbs ctx lock");
            let mut response = vec![0u8; MAX_RESPONSE];
            let mut response_len = response.len() as u32;
            // SAFETY: `ctx.0` is a live context; `command` and `response` are valid buffers whose
            // lengths are passed alongside them.
            let rc = unsafe {
                Tbsip_Submit_Command(
                    ctx.0.cast_const(),
                    TBS_COMMAND_LOCALITY_ZERO,
                    TBS_COMMAND_PRIORITY_NORMAL,
                    command.as_ptr(),
                    command.len() as u32,
                    response.as_mut_ptr(),
                    &mut response_len,
                )
            };
            if rc != TBS_SUCCESS {
                return Err(HardwareSignerError::Backend(format!(
                    "TBS submit failed: 0x{rc:08x}"
                )));
            }
            response.truncate(response_len as usize);
            Ok(response)
        }

        /// Load (creating + persisting on first use) the signing key for `alias`, returning its
        /// persistent handle. Idempotent across process runs: an existing persistent key is adopted.
        fn ensure_key(&self, alias: &str) -> Result<u32, HardwareSignerError> {
            let persistent = wire::persistent_handle(alias);
            if let Ok(resp) = self.submit(&wire::build_read_public(persistent)) {
                if wire::check_response(&resp).is_ok() {
                    return Ok(persistent);
                }
            }
            // Create a fresh restricted primary, create + load the signing key under it, then evict
            // the loaded key to the persistent handle — the reference's lifecycle, marshalled raw.
            let primary_resp = self.submit(&wire::build_create_primary())?;
            let primary = wire::response_first_handle(&primary_resp)?;
            let create_resp = self.submit(&wire::build_create(primary))?;
            let (out_private, out_public) = wire::parse_create_response(&create_resp)?;
            let load_resp = self.submit(&wire::build_load(primary, &out_private, &out_public))?;
            let transient = wire::response_first_handle(&load_resp)?;
            self.submit(&wire::build_evict_control(transient, persistent))?;
            // Best-effort flush of the transient objects.
            let _ = self.submit(&wire::build_flush_context(transient));
            let _ = self.submit(&wire::build_flush_context(primary));
            Ok(persistent)
        }

        fn read_point(&self, handle: u32) -> Result<Vec<u8>, HardwareSignerError> {
            let resp = self.submit(&wire::build_read_public(handle))?;
            Ok(wire::parse_read_public(&resp)?.point.to_vec())
        }
    }

    impl HardwareSigner for TbsTpmSigner {
        fn enroll(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
            let handle = self.ensure_key(&key_alias)?;
            self.read_point(handle)
        }

        fn classical_public_key(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
            let handle = self.ensure_key(&key_alias)?;
            self.read_point(handle)
        }

        fn sign_classical(
            &self,
            key_alias: String,
            msg: Vec<u8>,
        ) -> Result<Vec<u8>, HardwareSignerError> {
            let handle = self.ensure_key(&key_alias)?;
            // Hash inside the TPM to obtain the validation ticket Sign requires.
            let hash_resp = self.submit(&wire::build_hash(&msg))?;
            let (digest, ticket) = wire::parse_hash_response(&hash_resp)?;
            let sign_resp = self.submit(&wire::build_sign(handle, &digest, &ticket))?;
            // The P-256 composition expects DER-encoded ECDSA; the TPM returns raw r‖s.
            wire::ecdsa_der_from_sign_response(&sign_resp)
        }

        fn assert_non_exportable(&self, key_alias: String) -> Result<(), HardwareSignerError> {
            let handle = self.ensure_key(&key_alias)?;
            let resp = self.submit(&wire::build_read_public(handle))?;
            if wire::is_non_exportable(wire::parse_read_public(&resp)?.attributes) {
                Ok(())
            } else {
                Err(HardwareSignerError::Exportable)
            }
        }
    }

    #[cfg(test)]
    mod smoke;
}
