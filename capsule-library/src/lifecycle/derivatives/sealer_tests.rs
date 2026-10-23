use super::*;

/// A draw that hands back a fixed sequence, so a collision can be *forced* rather than
/// waited for — a real 7-byte collision is a 1-in-2^56 event.
struct ScriptedDraw {
    prefixes: RefCell<Vec<[u8; NONCE_PREFIX_LEN]>>,
}

impl ScriptedDraw {
    fn next(&self) -> [u8; NONCE_PREFIX_LEN] {
        let mut queue = self.prefixes.borrow_mut();
        if queue.len() == 1 {
            queue[0]
        } else {
            queue.remove(0)
        }
    }
}

fn sealer<'a>(
    amk: &'a Amk,
    used: HashSet<[u8; NONCE_PREFIX_LEN]>,
    draw: &'a dyn Fn() -> [u8; NONCE_PREFIX_LEN],
) -> AlbumSealer<'a> {
    AlbumSealer {
        amk,
        asset_id: Uuid::from_u128(0xDEF),
        used: RefCell::new(used),
        draw,
    }
}

/// **The normative refusal.** A draw that keeps returning a prefix already used for this
/// `file_id` is refused rather than accepted, and the refusal is a `Sign`-class fault so
/// decision 22 propagates it instead of silently writing one derivative fewer.
///
/// A prefix is folded into the file-key salt, so reusing one reuses the **key**: two blobs
/// under one keystream, which is exactly what the encryption doc's "defense in depth on top
/// of the CSPRNG draw" exists to prevent.
#[test]
fn a_prefix_already_used_for_this_file_id_is_refused() {
    let amk = Amk::from_bytes([0x11; 32]);
    let original = [1, 2, 3, 4, 5, 6, 7];
    let mut used = HashSet::new();
    used.insert(original);

    // The RNG is forced to keep offering the original's prefix.
    let scripted = ScriptedDraw {
        prefixes: RefCell::new(vec![original]),
    };
    let draw = || scripted.next();
    let error = sealer(&amk, used, &draw)
        .seal(b"derivative plaintext")
        .expect_err("a reused prefix is refused");
    assert!(
        matches!(error, MediaError::Sign { .. }),
        "an exhausted draw is a workspace fault, not a missing thumbnail: {error:?}"
    );
}

/// A collision is **redrawn**, not fatal: the first candidate is taken, the second is used.
#[test]
fn a_collision_is_redrawn_and_the_next_candidate_is_accepted() {
    let amk = Amk::from_bytes([0x22; 32]);
    let taken = [9, 9, 9, 9, 9, 9, 9];
    let fresh = [8, 7, 6, 5, 4, 3, 2];
    let mut used = HashSet::new();
    used.insert(taken);

    let scripted = ScriptedDraw {
        prefixes: RefCell::new(vec![taken, fresh]),
    };
    let draw = || scripted.next();
    let sealed = sealer(&amk, used, &draw)
        .seal(b"derivative plaintext")
        .expect("the redraw succeeds");
    assert_eq!(
        sealed.nonce_prefix, fresh,
        "the colliding candidate is skipped and the next one is used"
    );
}

/// Each sealed prefix **joins** the set, so two derivatives of one asset cannot collide with
/// each other either — not only with what was already on disk.
#[test]
fn a_freshly_sealed_prefix_is_spoken_for_by_the_next_seal() {
    let amk = Amk::from_bytes([0x33; 32]);
    let first = [1, 1, 1, 1, 1, 1, 1];
    let second = [2, 2, 2, 2, 2, 2, 2];

    // The draw offers `first`, then `first` again (a collision with what was just sealed),
    // then `second`.
    let scripted = ScriptedDraw {
        prefixes: RefCell::new(vec![first, first, second]),
    };
    let draw = || scripted.next();
    let sealer = sealer(&amk, HashSet::new(), &draw);

    assert_eq!(sealer.seal(b"one").expect("first seal").nonce_prefix, first);
    assert_eq!(
        sealer.seal(b"two").expect("second seal").nonce_prefix,
        second,
        "the prefix the first seal used is refused for the second"
    );
}

/// The bundle reader hands the sealer every prefix already spent on this `file_id`.
#[test]
fn existing_derivatives_reports_every_persisted_prefix() {
    use capsule_core::crypto::keys::HybridSigningKey;
    use capsule_core::crypto::provenance::{DERIVATIVE_MANIFEST_VERSION, DerivativeCore};

    let dir = tempfile::tempdir().expect("scratch");
    let asset_id = Uuid::from_u128(0xFEED);
    let device = HybridSigningKey::from_seed_bytes(&[31; 32], &[32; 32]);
    let write = HybridSigningKey::from_seed_bytes(&[33; 32], &[34; 32]);

    let manifest = |role, prefix: [u8; NONCE_PREFIX_LEN]| {
        DerivativeCore {
            version: DERIVATIVE_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: Some(PROTOCOL_VERSION.into()),
            amk_version: Some(AmkVersion(1)),
            source_asset_id: asset_id,
            role,
            format: "image/jxl".into(),
            ciphertext_hash: hash::hash_bytes(b"bytes"),
            nonce_prefix: prefix,
            generated_by_device: Uuid::from_u128(0xD1),
            generated_by_client: "capsule-core/test".into(),
            model_id: None,
            model_version: None,
            generated_at: "2026-09-02T00:00:00Z".into(),
            prior_provenance_hash: None,
        }
        .sign(&device, &write)
        .expect("signing")
    };
    let manifests = vec![
        manifest(DerivativeRole::Thumbnail, [1, 1, 1, 1, 1, 1, 1]),
        manifest(DerivativeRole::Preview, [2, 2, 2, 2, 2, 2, 2]),
    ];
    fs::write(
        dir.path()
            .join(format!("{}.derivatives.cbor", asset_id.simple())),
        cbor::to_canonical_vec(&manifests).unwrap(),
    )
    .unwrap();

    let existing = existing_derivatives(dir.path(), asset_id);
    assert!(existing.used_prefixes.contains(&[1, 1, 1, 1, 1, 1, 1]));
    assert!(existing.used_prefixes.contains(&[2, 2, 2, 2, 2, 2, 2]));
    assert_eq!(existing.used_prefixes.len(), 2);
    assert_eq!(existing.heads.len(), 2, "and both roles have a chain head");
}
