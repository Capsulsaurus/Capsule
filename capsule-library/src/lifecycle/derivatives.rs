//! The one `lifecycle` file that reaches [`capsule_core::media`]: decode a still once at import and
//! derive everything that needs pixels.
//!
//! # Why this is not feature-gated
//!
//! `capsule-library` depends on `capsule-core` with its default features, and `native` (the
//! default) *implies* `media`, so a build that compiles this file always has the codec stack. The
//! builds that drop `media` (`capsule-wasm`, and `capsule-server` as a normal dependency, both
//! `default-features = false`) do not link the library at all. A `#[cfg(feature = "media")]` here
//! would therefore guard nothing while making every signature read as optional; if the
//! implication is ever removed, this file fails to compile, which is the right way for that
//! decision to surface.
//!
//! # Never fails the import
//!
//! Capsule is a backup tool. Every path below degrades to "the original is imported signed,
//! encrypted and `verify_asset`-accepting, without a placeholder or a thumbnail" and records
//! **why** in the returned [`DerivativeStatus`]. The only errors that propagate are the ones
//! that mean the *workspace* is broken — a missing album, a signer that refused — not the ones
//! that mean the pixels were unreadable.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use capsule_core::cbor;
use capsule_core::crypto::encryption::{
    AssetEncryption, NONCE_PREFIX_LEN, encrypt_asset_rekey_with_prefix,
};
use capsule_core::crypto::hash::{self, Hash32};
use capsule_core::crypto::keys::{Amk, AmkVersion};
use capsule_core::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use capsule_core::crypto::provenance::{DerivativeManifest, DerivativeRole};
use capsule_core::crypto::rng;
use capsule_core::lqip::Lqip;
use capsule_core::media::{
    DecodedImage, DerivativeContext, DerivativeSealer, DerivativeTier, GeneratedDerivative,
    MediaError, RawshiftDecoder, SealedDerivative, StillFormat, decode_guarded,
    generate_still_derivatives, guarded,
};
use capsule_core::sidecar::sidecar_v1::{Dimensions, Lqip as SidecarLqip};
use uuid::Uuid;

use super::{AssetState, DerivativeStatus, LifecycleError, Result, Workspace, media_dir};
use crate::exif::ExifExtract;

/// The file under import, as [`prepare_still`](Workspace::prepare_still) needs to see it.
///
/// A parameter object rather than four positional arguments: these four are one thing — the
/// bytes on the way in and what the scanner already learned about them — and they are always
/// passed together. The alternative was silencing `clippy::too_many_arguments`, which would have
/// hidden that the signature had grown two *kinds* of input (the file, and the crypto identity
/// it commits under) without saying so.
pub(super) struct StillSource<'a> {
    /// The file's bytes — or only its head, when the import did not hold the file: one whose
    /// head names no still this build decodes (the decoder's gate reads no further before
    /// refusing, so the status is the same), or a decodable still too large to hold.
    pub(super) plaintext: &'a [u8],
    /// The file's full size. Larger than `plaintext` exactly when only the head was passed.
    pub(super) size: u64,
    /// Its lowercase extension without the dot, `""` when it has none.
    pub(super) ext: &'a str,
    /// Where it came from — for logs only; the bytes above are authoritative. An adopted guest
    /// drop has no path, so it passes a `drop:{id}` label instead.
    pub(super) src: &'a Path,
    /// What `capsule_core::exif` read off it, the fallback for dimensions. An adopted drop
    /// passes the empty extract: its bytes arrive in memory and nothing has scanned them.
    pub(super) exif: &'a ExifExtract,
}

/// Everything one still yields in a single decode pass: the sidecar fields, the signed
/// derivatives to persist after the durable commit, and the reason for anything missing.
pub(super) struct PreparedStill {
    /// The format detection identified, if the bytes are a still Capsule models. Drives the
    /// sidecar's `content_type` from the *header* rather than from the file name.
    pub(super) format: Option<StillFormat>,
    /// Pixel dimensions when the still decoded, EXIF dimensions otherwise, `None` if neither.
    ///
    /// Decoded pixels win over EXIF because they are post-orientation: a quarter-turned JPEG's
    /// EXIF `PixelXDimension` is its *stored* width, which is transposed relative to what a
    /// viewer shows.
    pub(super) dimensions: Option<Dimensions>,
    /// The sidecar LQIP, present only when the still decoded.
    pub(super) lqip: Option<SidecarLqip>,
    /// Signed thumbnail derivatives, to be written after the asset's own files.
    pub(super) derivatives: Vec<GeneratedDerivative>,
    /// How many `(tier, format)` pairs the tier table commits to and this build cannot encode.
    pub(super) deferred_formats: usize,
    /// Whether derivatives were generated, and if not, why.
    pub(super) status: DerivativeStatus,
}

impl PreparedStill {
    /// The outcome for bytes that yielded no pixels: EXIF dimensions only, no LQIP, no
    /// derivatives, and `status` carrying which of the reasons it was.
    fn undecoded(
        format: Option<StillFormat>,
        exif_dimensions: Option<Dimensions>,
        status: DerivativeStatus,
    ) -> Self {
        Self {
            format,
            dimensions: exif_dimensions,
            lqip: None,
            derivatives: Vec::new(),
            deferred_formats: 0,
            status,
        }
    }
}

/// Map a decode failure onto the status the run summary counts, logging the distinction that
/// makes the two reasons useful.
fn classify(error: &MediaError, src: &Path, format: Option<StillFormat>) -> DerivativeStatus {
    match error {
        MediaError::UnsupportedFormat { format, op } => {
            tracing::warn!(
                path = %src.display(),
                %format,
                op = op.as_str(),
                supported = ?capsule_core::media::SUPPORTED_STILL_FORMATS,
                "derivatives: no codec for this format in this build; the original is imported \
                 signed and encrypted, but without a thumbnail or LQIP until the codec lands. \
                 Derivatives are backfillable from the stored original"
            );
            DerivativeStatus::DeferredNoCodec
        }
        MediaError::NotAStillImage => {
            tracing::debug!(
                path = %src.display(),
                "derivatives: not a still image Capsule models; nothing to decode"
            );
            DerivativeStatus::NotAKnownStill
        }
        // Everything else is a format we *do* support failing on these particular bytes — a
        // real problem worth investigating, not an expected gap.
        error => {
            tracing::warn!(
                path = %src.display(),
                ?format,
                %error,
                "derivatives: a supported format failed to decode; the original is imported \
                 signed and encrypted, but without a thumbnail or LQIP"
            );
            DerivativeStatus::DecodeFailed
        }
    }
}

/// Compute the sidecar LQIP from a decoded frame.
///
/// From the **full-resolution, orientation-applied** frame, never from the thumbnail:
/// chromahash consumes the whole frame and band-limits on the read side via `decode_capped`, so
/// pre-resizing would silently cap fidelity the format can carry ([`capsule_core::lqip`]).
fn lqip_from(decoded: &DecodedImage, src: &Path) -> Option<SidecarLqip> {
    // Guarded because `chromahash` is pre-1.0 too and its own `encode` panics on a zero
    // dimension or a length mismatch. `Lqip::encode` checks both, so this is belt and braces —
    // but it is what makes the module's "no codec can abort an import" claim true rather than
    // nearly true.
    let encoded = guarded("lqip", || {
        Lqip::encode(
            decoded.width(),
            decoded.height(),
            &decoded.image.rgba,
            decoded.gamut,
        )
        .map_err(|e| MediaError::Decode {
            format: decoded.format,
            detail: format!("LQIP encode: {e}"),
        })
    });
    match encoded {
        Ok(lqip) => Some(lqip.to_sidecar()),
        Err(error) => {
            // A decoded frame satisfies both of `encode`'s preconditions by construction, so
            // this is unreachable rather than expected — logged as such, and never fatal.
            tracing::warn!(
                path = %src.display(),
                %error,
                "derivatives: a decoded frame was rejected by the LQIP encoder; importing \
                 without a placeholder"
            );
            None
        }
    }
}

/// What an asset's existing derivative bundle constrains about the next generation.
///
/// One read, two facts, because both come off the same file and both are needed together.
pub(super) struct ExistingDerivatives {
    /// The current head of each role's chain. Empty when the asset has no bundle yet, which is
    /// every import: a create starts each role's chain. It is a **regeneration** — the `#437`
    /// backfill that adds a second format to an asset that already has one — that needs this,
    /// and it needs it to be right the first time, because a forked chain is not something a
    /// later run can repair.
    ///
    /// The link is SHA-256 over the manifest's canonical CBOR, signatures included: the same
    /// content-hash link the asset provenance chain uses.
    pub(super) heads: HashMap<DerivativeRole, Hash32>,
    /// Every `nonce_prefix` already used for this `file_id` by a derivative.
    ///
    /// The encryption doc makes the refusal normative: "the writer additionally refuses to emit
    /// a `nonce_prefix` it has already used for that `file_id` … the same rule governs
    /// derivative re-encryption". A prefix is folded into the file-key salt, so reusing one
    /// reuses the *key* as well as the nonce — the keystream separation the whole construction
    /// rests on.
    pub(super) used_prefixes: HashSet<[u8; NONCE_PREFIX_LEN]>,
}

/// Read `asset_id`'s persisted derivative bundle, if it has one.
///
/// A bundle that does not decode yields the empty answer *and a warning*: treating an
/// unreadable bundle as "no constraints" is the safe direction for the chain (a role restarts)
/// but the unsafe one for prefixes, so the warning says which risk is being taken.
pub(super) fn existing_derivatives(dir: &Path, asset_id: Uuid) -> ExistingDerivatives {
    let empty = ExistingDerivatives {
        heads: HashMap::new(),
        used_prefixes: HashSet::new(),
    };
    let path = dir.join(format!("{}.derivatives.cbor", asset_id.simple()));
    let Ok(bytes) = fs::read(&path) else {
        return empty;
    };
    let Ok(manifests) = cbor::from_slice::<Vec<DerivativeManifest>>(&bytes) else {
        tracing::warn!(
            path = %path.display(),
            "derivatives: undecodable bundle; every role's chain restarts and no previously used \
             nonce prefix can be excluded from the next draw"
        );
        return empty;
    };

    // Generation order is the chain order, so the last manifest of a role is that role's head.
    let mut heads = HashMap::new();
    let mut used_prefixes = HashSet::new();
    for manifest in &manifests {
        used_prefixes.insert(manifest.core.nonce_prefix);
        match cbor::to_canonical_vec(manifest) {
            Ok(canonical) => {
                heads.insert(manifest.core.role, hash::hash_bytes(&canonical));
            }
            Err(error) => tracing::warn!(
                %error,
                "derivatives: a persisted manifest did not re-serialise; its role's chain \
                 restarts rather than linking to something unverifiable"
            ),
        }
    }
    ExistingDerivatives {
        heads,
        used_prefixes,
    }
}

/// The album-key half of derivative generation: `media` produces the bytes, this encrypts them.
///
/// One `encrypt_asset_rekey_with_prefix` per derivative under the **source asset's** `file_id`
/// and the album's current AMK, so every derivative gets its own file key per the encryption
/// doc's per-file derivation. The ciphertext is deliberately dropped: the client keeps the
/// plaintext derivative on disk (the local gallery paints it) and re-derives the ciphertext at
/// push time from the recorded prefix, exactly as it already does for the original.
///
/// # The reuse refusal
///
/// A CSPRNG draw is not on its own what the design asks for. The encryption doc requires that
/// the writer *refuse* a `nonce_prefix` it has already used for that `file_id`, "defense in
/// depth on top of the CSPRNG draw", and says the same rule governs derivative re-encryption.
/// So [`used`](Self::used) starts as the original's prefix plus every prefix in the existing
/// bundle, each newly sealed prefix joins it, and a collision is redrawn.
///
/// A prefix is folded into the file-key salt, so a reused one reuses the **key** as well as the
/// nonce — two blobs under one keystream, which is the failure the whole construction exists to
/// prevent.
struct AlbumSealer<'a> {
    amk: &'a Amk,
    asset_id: Uuid,
    /// Prefixes already spoken for on this `file_id`. `RefCell` because [`DerivativeSealer`]
    /// takes `&self` — the seam is shared, and each seal has to see what the last one used.
    used: RefCell<HashSet<[u8; NONCE_PREFIX_LEN]>>,
    /// Where a candidate prefix comes from: the OS CSPRNG in production, forced in the test
    /// that proves the refusal fires.
    draw: &'a dyn Fn() -> [u8; NONCE_PREFIX_LEN],
}

/// How many times a collision is redrawn before the draw itself is called broken.
///
/// A 7-byte prefix collides by chance at about 1 in 2^56, so a run of eight is not bad luck —
/// it is a CSPRNG returning something it should not, which is a **workspace** fault and not
/// this asset's. Hence `MediaError::Sign`, which decision 22 routes to a propagated error
/// rather than to a missing thumbnail: an import that cannot draw a safe nonce must stop, not
/// quietly write one derivative fewer.
const MAX_PREFIX_DRAWS: usize = 8;

/// Test-only fault injection for the sealer.
///
/// The two failure paths decision 22 and decision 23 turn on — a codec refusing a frame the
/// decoder accepted, and a codec *panicking* on one — cannot be produced from real bytes on
/// demand, and testing them anywhere but through `import_asset_with` proves nothing about the
/// property that matters: that **the asset still commits**. So the fault is injected at the one
/// point inside the real import path where a codec failure originates.
///
/// `#[cfg(test)]`, so it does not exist in a release build at all — not a disabled branch, not a
/// dead field, absent. A thread-local rather than a parameter because threading an `Option<&dyn
/// DerivativeSealer>` through `prepare_still` would put a test seam in a production signature;
/// nextest runs each test in its own process, so there is nothing for it to leak into.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(super) enum SealerFault {
    /// A codec refuses the frame — an `Encode`-class error, which decision 22 degrades.
    Refuse,
    /// A pre-1.0 codec panics — which decision 23's guard has to catch.
    Panic,
}

#[cfg(test)]
thread_local! {
    static SEALER_FAULT: RefCell<Option<SealerFault>> = const { RefCell::new(None) };
}

/// Run `body` with `fault` injected into every seal, restoring the previous state after.
#[cfg(test)]
pub(super) fn with_sealer_fault<T>(fault: SealerFault, body: impl FnOnce() -> T) -> T {
    SEALER_FAULT.with(|slot| *slot.borrow_mut() = Some(fault));
    let out = body();
    SEALER_FAULT.with(|slot| *slot.borrow_mut() = None);
    out
}

impl DerivativeSealer for AlbumSealer<'_> {
    fn seal(&self, plaintext: &[u8]) -> std::result::Result<SealedDerivative, MediaError> {
        #[cfg(test)]
        if let Some(fault) = SEALER_FAULT.with(|slot| *slot.borrow()) {
            match fault {
                // Deliberately **not** `Sign`: this stands in for a codec refusing pixels, which
                // decision 22 degrades to `DecodeFailed` rather than propagating.
                SealerFault::Refuse => {
                    return Err(MediaError::Encode {
                        format: capsule_core::derivative_format::DerivativeFormat::Jxl,
                        detail: "injected codec refusal".into(),
                    });
                }
                SealerFault::Panic => panic!("injected codec panic on an accepted frame"),
            }
        }

        for attempt in 0..MAX_PREFIX_DRAWS {
            let prefix = (self.draw)();
            if self.used.borrow().contains(&prefix) {
                tracing::warn!(
                    asset_id = %self.asset_id,
                    attempt,
                    "derivatives: drew a nonce prefix already used for this file_id; redrawing"
                );
                continue;
            }
            let (enc, _ciphertext, _file_key) =
                encrypt_asset_rekey_with_prefix(self.amk, &self.asset_id, plaintext, prefix, None)
                    .map_err(|e| MediaError::Sign {
                        detail: format!("sealing the derivative: {e}"),
                    })?;
            self.used.borrow_mut().insert(enc.nonce_prefix);
            return Ok(SealedDerivative {
                ciphertext_hash: enc.ciphertext_hash,
                nonce_prefix: enc.nonce_prefix,
            });
        }
        Err(MediaError::Sign {
            detail: format!(
                "could not draw an unused nonce prefix for {} in {MAX_PREFIX_DRAWS} attempts",
                self.asset_id
            ),
        })
    }
}

impl Workspace {
    /// Decode the still once and derive: the `content_type`, pixel `dimensions`, the sidecar
    /// `lqip`, and the signed thumbnail derivatives. All are attached before the sidecar is
    /// sealed, per the pipeline's Execute step.
    ///
    /// **Never fails over unreadable pixels.** A still this build cannot decode still commits as
    /// a signed, encrypted original — it falls back to EXIF dimensions with no LQIP and no
    /// derivatives, and the returned [`DerivativeStatus`] says which reason applied so the
    /// caller can report the gap instead of it being invisible.
    #[tracing::instrument(
        level = "debug",
        skip_all,
        fields(asset_id = %asset_id, src = %source.src.display(), bytes = source.plaintext.len())
    )]
    pub(super) fn prepare_still(
        &self,
        source: &StillSource<'_>,
        asset_id: Uuid,
        album_id: Uuid,
        capture_utc: i64,
        amk: &Amk,
        original: &AssetEncryption,
    ) -> Result<PreparedStill> {
        let existing = existing_derivatives(
            &media_dir(&self.root, capture_utc).join("derivatives"),
            asset_id,
        );
        self.prepare_still_with(source, asset_id, album_id, amk, original, existing)
    }

    /// [`prepare_still`](Self::prepare_still) over a caller-supplied [`ExistingDerivatives`]
    /// rather than the bundle on disk.
    ///
    /// The one caller that needs it is the stale-epoch re-seal
    /// ([`reseal_create`](Self::reseal_create)): its derivatives start **fresh chains** at the
    /// album's current epoch, because the ones on disk were sealed under a key the album has
    /// moved past and were never published, so nothing may link to them. It still hands in every
    /// prefix the asset has spent, so the reuse refusal covers them.
    pub(super) fn prepare_still_with(
        &self,
        source: &StillSource<'_>,
        asset_id: Uuid,
        album_id: Uuid,
        amk: &Amk,
        original: &AssetEncryption,
        existing: ExistingDerivatives,
    ) -> Result<PreparedStill> {
        let StillSource {
            plaintext,
            size,
            ext,
            src,
            exif,
        } = *source;
        let exif_dimensions = exif
            .width
            .zip(exif.height)
            .map(|(width, height)| Dimensions { width, height });
        // Detected here as well as inside the decoder so the sidecar's `content_type` is
        // header-derived even for a format with no codec: a HEIC is `image/heic` in the sidecar
        // whether or not this build can read its pixels.
        let sniffed = StillFormat::detect(plaintext, ext);
        if (plaintext.len() as u64) < size && sniffed.is_some_and(StillFormat::is_decodable) {
            // The importer declined to hold a file this large for the decoder. Reported as a
            // failed decode, the bucket a still over the pixel budget lands in too: a
            // supported format that produced no derivative, worth someone looking at.
            tracing::warn!(
                asset_id = %asset_id,
                path = %src.display(),
                format = ?sniffed,
                size,
                "derivatives: a still too large to hold for decoding; the original is imported \
                 signed and encrypted, but without a thumbnail or LQIP"
            );
            return Ok(PreparedStill::undecoded(
                sniffed,
                exif_dimensions,
                DerivativeStatus::DecodeFailed,
            ));
        }

        let decoded = match decode_guarded(&RawshiftDecoder, plaintext, ext) {
            Ok(decoded) => decoded,
            Err(error) => {
                let status = classify(&error, src, sniffed);
                return Ok(PreparedStill::undecoded(sniffed, exif_dimensions, status));
            }
        };

        let dimensions = Some(Dimensions {
            width: decoded.width(),
            height: decoded.height(),
        });
        if let (Some(pixels), Some(exif_dims)) = (dimensions.as_ref(), exif_dimensions.as_ref())
            && pixels != exif_dims
        {
            // Not an error: EXIF dimensions are pre-orientation and are frequently stale after
            // an edit. Logged because a surprising sidecar dimension is otherwise unexplainable
            // after the fact.
            tracing::debug!(
                asset_id = %asset_id,
                pixel_width = pixels.width,
                pixel_height = pixels.height,
                exif_width = exif_dims.width,
                exif_height = exif_dims.height,
                orientation = decoded.orientation_applied,
                "derivatives: decoded dimensions differ from EXIF; the pixels are authoritative"
            );
        }
        let lqip = lqip_from(&decoded, src);

        let album = self.album(&album_id)?;
        let ExistingDerivatives {
            heads,
            mut used_prefixes,
        } = existing;
        // The original's prefix is spoken for too: it is a prefix used for this `file_id`.
        used_prefixes.insert(original.nonce_prefix);

        let ctx = DerivativeContext {
            source_asset_id: asset_id,
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            amk_version: AmkVersion(album.current_epoch),
            generated_by_device: self.account.device.device_id,
            generated_by_client: self.client_version.clone(),
            generated_at: super::now_rfc3339(),
            device_signer: self.device_signer.as_ref(),
            write_tier_signer: album.write_tier_signer()?,
            sealer: &AlbumSealer {
                amk,
                asset_id,
                // Seeded with the original's own prefix and every prefix the existing bundle
                // already spent on this `file_id`.
                used: RefCell::new(used_prefixes),
                draw: &rng::random_array::<NONCE_PREFIX_LEN>,
            },
            // Empty on a create; a regeneration continues each role's chain from here.
            prior_heads: &heads,
            // The `original` sentinel references the original blob rather than encrypting
            // anything, so it signs what the original's own manifest signs.
            original: SealedDerivative {
                ciphertext_hash: original.ciphertext_hash,
                nonce_prefix: original.nonce_prefix,
            },
        };
        // Guarded, and **not** propagated on failure. A codec refusing a frame the decoder just
        // produced is a real defect, but it is this asset's derivative that is broken, not the
        // workspace: the signed original, its dimensions and its placeholder are all still
        // right, and failing the import would trade a missing thumbnail for a missing backup.
        // Reported as `DecodeFailed` — the "a supported path produced no derivative and somebody
        // should look at it" bucket — so the run summary counts it instead of staying silent.
        let generated = guarded("derivatives", || {
            generate_still_derivatives(&decoded, DerivativeTier::GENERATED, &ctx)
        });
        let derivatives = match generated {
            Ok(derivatives) => derivatives,
            // **A signing fault is the workspace's, not this asset's.** A hardware signer that
            // refuses, or a missing epoch write-tier key, is the same fault that would stop the
            // asset's own manifest being authored — degrading it to "no thumbnail" would hide a
            // broken workspace behind a cosmetic gap. It propagates.
            Err(error @ MediaError::Sign { .. }) => {
                tracing::error!(
                    asset_id = %asset_id,
                    path = %src.display(),
                    %error,
                    "derivatives: the workspace could not author a signed derivative record"
                );
                return Err(LifecycleError::Io(format!("derivative signing: {error}")));
            }
            // Everything else is about *pixels*: a codec refused a frame, a resize was rejected,
            // a third-party encoder panicked. A refusal on *some* pairs never reaches here —
            // generation records it as a deferral and ships the rest, so a failed preview AVIF
            // does not cost the thumbnail JXL — this arm is the asset with no derivative at all.
            // The signed original, its dimensions and its
            // placeholder are all still right, and failing the import would trade a missing
            // thumbnail for a missing backup — which is the whole of the no-codec reasoning
            // and this module's stated contract. Reported as `DecodeFailed`, the "a supported path
            // produced no derivative and somebody should look at it" bucket, so the run summary
            // counts it instead of staying silent.
            Err(error) => {
                tracing::warn!(
                    asset_id = %asset_id,
                    path = %src.display(),
                    format = %decoded.format,
                    width = decoded.width(),
                    height = decoded.height(),
                    %error,
                    "derivatives: the still decoded but no derivative could be produced from it; \
                     the original, its dimensions and its placeholder are committed regardless"
                );
                return Ok(PreparedStill {
                    format: Some(decoded.format),
                    dimensions,
                    lqip,
                    derivatives: Vec::new(),
                    deferred_formats: 0,
                    status: DerivativeStatus::DecodeFailed,
                });
            }
        };

        Ok(PreparedStill {
            format: Some(decoded.format),
            dimensions,
            lqip,
            deferred_formats: derivatives.deferred.len(),
            derivatives: derivatives.generated,
            status: DerivativeStatus::Decoded,
        })
    }

    /// Write the generated derivative bytes plus their signed manifest bundle under the asset's
    /// media directory: `derivatives/{uuid}.{role}.{ext}` and `{uuid}.derivatives.cbor`.
    ///
    /// The layout is the one the upload bundle reader already looks for: since `F5` it composes
    /// a derivative's exact path from the manifest's `(role, format)` pair rather than scanning
    /// for a `{uuid}.{role}.` prefix, so two formats of one role cannot be mistaken for each
    /// other.
    ///
    /// Called **after** the asset's own files are durable: a derivative is regenerable and must
    /// never be able to fail an import that has already committed. A write error is therefore
    /// logged and swallowed rather than propagated.
    pub(super) fn persist_derivatives(
        &self,
        asset: &AssetState,
        derivatives: &[GeneratedDerivative],
    ) {
        if derivatives.is_empty() {
            return;
        }
        let dir = media_dir(&self.root, asset.capture_utc).join("derivatives");
        if let Err(error) = fs::create_dir_all(&dir) {
            tracing::warn!(
                asset_id = %asset.asset_id,
                dir = %dir.display(),
                %error,
                "derivatives: could not create the derivative directory; the asset is committed \
                 and its derivatives are regenerable"
            );
            return;
        }
        let stem = asset.asset_id.simple();

        let mut manifests = Vec::with_capacity(derivatives.len());
        for derivative in derivatives {
            // The `original` sentinel has no bytes of its own: its manifest *references* the
            // original, whose content address it signs. Writing a byte-for-byte copy under a
            // thumbnail's name would duplicate a file two directories up and re-expose the
            // original's EXIF — GPS included — as a derivative, where a re-encoded thumbnail is
            // metadata-free by construction. Its manifest still goes into the bundle: that
            // signed marker is the difference between "the original *is* the thumbnail" and
            // "the thumbnail is missing, rebuild it".
            if let Some(format_ext) = derivative.format.extension() {
                let path = dir.join(format!(
                    "{stem}.{}.{format_ext}",
                    derivative.tier.role_name()
                ));
                if let Err(error) = fs::write(&path, &derivative.bytes) {
                    tracing::warn!(
                        asset_id = %asset.asset_id,
                        path = %path.display(),
                        %error,
                        "derivatives: could not write a derivative; skipping it"
                    );
                    continue;
                }
            } else {
                debug_assert!(
                    derivative.bytes.is_empty(),
                    "only the byte-free `original` sentinel has no extension"
                );
            }
            manifests.push(derivative.manifest.clone());
        }

        if manifests.is_empty() {
            return;
        }
        match cbor::to_canonical_vec(&manifests) {
            Ok(bundle) => {
                let path = dir.join(format!("{stem}.derivatives.cbor"));
                if let Err(error) = fs::write(&path, bundle) {
                    tracing::warn!(
                        asset_id = %asset.asset_id,
                        path = %path.display(),
                        %error,
                        "derivatives: could not write the manifest bundle; the bytes on disk are \
                         unusable without it and will be regenerated"
                    );
                    return;
                }
                tracing::debug!(
                    asset_id = %asset.asset_id,
                    count = manifests.len(),
                    dir = %dir.display(),
                    "derivatives: persisted with their signed manifest bundle"
                );
            }
            Err(error) => tracing::warn!(
                asset_id = %asset.asset_id,
                %error,
                "derivatives: the manifest bundle did not serialise; skipping persistence"
            ),
        }
    }
}

#[cfg(test)]
mod head_tests;

#[cfg(test)]
mod tests;

// ── The nonce-prefix reuse refusal (encryption.md, "Re-keying on Rewrite") ───

#[cfg(test)]
mod sealer_tests;
