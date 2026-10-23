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
                 Derivatives are backfillable from the stored original (S-B13)"
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
            // thumbnail for a missing backup — which is the whole of `S-B13`'s reasoning and
            // this module's stated contract. Reported as `DecodeFailed`, the "a supported path
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
mod tests {
    use capsule_core::crypto::hash;
    use capsule_core::crypto::provenance::DerivativeManifest;
    use capsule_core::derivative_format::{DerivativeFormat, verify_still_format};
    use capsule_core::media::Decoder as _;
    use capsule_core::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};
    use rawshift_image::core::metadata::{ImageInfo, ImageMetadata};
    use rawshift_image::core::{BitDepth, MetadataEmbedOptions};
    use rawshift_image::formats::encode_rgb_image_to_vec;
    use rawshift_image::formats::export::{
        CommonEncodeOptions, EncodeOptions, JpegEncEncodeConfig, ZunePngEncodeConfig,
    };
    use tempfile::TempDir;

    use super::super::{DerivativeStatus, SignedImportOptions, Workspace, fast_workspace};
    use super::*;

    /// Delivery formats this build encodes: JXL, plus AVIF under `capsule-core`'s
    /// `media-linked-codecs`.
    ///
    /// Read from what core generates rather than from a `cfg!` here: the codec feature is
    /// `capsule-core`'s, and a workspace build can turn it on through another dependent (the CLI)
    /// without turning on this crate's pass-through of the same name. The preview tier is
    /// generated exactly when the linked codecs are.
    fn delivery_formats_encoded() -> usize {
        if DerivativeTier::GENERATED.contains(&DerivativeTier::Preview) {
            2
        } else {
            1
        }
    }

    /// Manifests a still larger than the thumbnail cap yields: every generated tier times every
    /// encoded delivery format.
    fn encoded_pairs_per_large_still() -> usize {
        DerivativeTier::GENERATED.len() * delivery_formats_encoded()
    }

    /// A deterministic RGB gradient, `width` x `height`, as interleaved RGB `u16`.
    fn frame(width: u32, height: u32) -> rawshift_image::core::image::RgbImage {
        let (w, h) = (width as usize, height as usize);
        let mut data = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                data.push(((x * 255 / w) as u16) * 257);
                data.push(((y * 255 / h) as u16) * 257);
                data.push((((x + y) * 255 / (w + h)) as u16) * 257);
            }
        }
        rawshift_image::core::image::RgbImage::with_color_space(
            width,
            height,
            data,
            rawshift_image::core::ColorSpace::Srgb,
        )
    }

    fn common(metadata: MetadataEmbedOptions) -> CommonEncodeOptions {
        CommonEncodeOptions {
            metadata,
            bit_depth: BitDepth::Eight,
        }
    }

    /// A JPEG carrying an EXIF orientation tag, so the decode path has a transform to apply and
    /// the sidecar's dimensions have to disagree with the stored ones.
    pub(super) fn jpeg(width: u32, height: u32, orientation: Option<u16>) -> Vec<u8> {
        let metadata = ImageMetadata {
            image: ImageInfo {
                orientation,
                ..ImageInfo::default()
            },
            ..ImageMetadata::default()
        };
        let embed = MetadataEmbedOptions {
            embed_exif: orientation.is_some(),
            embed_icc: false,
            embed_xmp: false,
        };
        encode_rgb_image_to_vec(
            &frame(width, height),
            &metadata,
            &EncodeOptions::JpegJpegEnc(JpegEncEncodeConfig {
                common: common(embed),
                quality: 90,
            }),
        )
        .expect("the fixture JPEG encodes")
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        encode_rgb_image_to_vec(
            &frame(width, height),
            &ImageMetadata::default(),
            &EncodeOptions::PngZune(ZunePngEncodeConfig {
                common: common(MetadataEmbedOptions::none()),
            }),
        )
        .expect("the fixture PNG encodes")
    }

    /// A workspace with fast Argon2 params and its default album created.
    pub(super) fn workspace(dir: &Path) -> (Workspace, Uuid) {
        let mut ws = fast_workspace(dir);
        let album = ws.default_album_id();
        ws.create_album_with_id(album, "Imports").unwrap();
        (ws, album)
    }

    /// Write `bytes` into `dir` under `name` and import it, returning the receipt.
    fn import(
        ws: &mut Workspace,
        album: Uuid,
        dir: &Path,
        name: &str,
        bytes: &[u8],
    ) -> super::super::SignedImport {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        ws.import_asset_with(album, &path, &SignedImportOptions::default())
            .expect("the import commits")
    }

    /// Read back the signed sidecar an import wrote, from the library directory alone.
    fn sidecar_of(root: &Path, asset_id: Uuid) -> SidecarV1 {
        let mut stack = vec![root.join("media")];
        let name = format!("{}.cbor", asset_id.simple());
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if entry.file_name() == std::ffi::OsString::from(&name) {
                    let bytes = fs::read(&path).unwrap();
                    return SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1)
                        .expect("the sidecar decodes");
                }
            }
        }
        panic!("no sidecar for {asset_id}");
    }

    /// The derivatives directory for the bucket holding `asset_id`'s files.
    fn derivatives_dir(root: &Path, asset_id: Uuid) -> std::path::PathBuf {
        let mut stack = vec![root.join("media")];
        let name = format!("{}.cbor", asset_id.simple());
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if entry.file_name() == std::ffi::OsString::from(&name) {
                    return dir.join("derivatives");
                }
            }
        }
        panic!("no bucket for {asset_id}");
    }

    /// **The acceptance case, and the first production caller of `capsule_core::lqip`.**
    ///
    /// A decodable still imports with real pixel dimensions and a 32-byte chromahash placeholder
    /// inside the *signed* sidecar, and the sidecar still verifies — the placeholder is
    /// signature-covered, so producing it is a signature-visible change and has to be checked as
    /// one.
    #[test]
    fn a_decodable_still_imports_with_pixel_dimensions_and_an_lqip() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let receipt = import(
            &mut ws,
            album,
            src.path(),
            "photo.jpg",
            &jpeg(320, 240, None),
        );
        assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);
        assert_eq!(
            receipt.deferred_formats, 2,
            "two (tier, format) pairs have no encoder: the thumbnail's AVIF and WebP without \
             `media-linked-codecs`, or WebP for both the thumbnail and the preview with it"
        );

        let sidecar = sidecar_of(lib.path(), receipt.asset_id);
        let dimensions = sidecar
            .dimensions
            .as_ref()
            .expect("dimensions from decoded pixels");
        assert_eq!((dimensions.width, dimensions.height), (320, 240));
        assert_eq!(
            sidecar.content_type, "image/jpeg",
            "the content type is header-derived"
        );

        let lqip = sidecar.lqip.as_ref().expect("the LQIP producer ran");
        assert_eq!(lqip.chromahash.len(), 32, "DEFAULT_TIER is 32 bytes");
        assert_eq!(lqip.format_version, capsule_core::lqip::LQIP_FORMAT_V1);
        assert!(
            Lqip::from_bytes(&lqip.chromahash).is_ok(),
            "the stored payload is a structurally valid chromahash"
        );

        assert!(
            sidecar.verify(&ws.user_ik_public()),
            "the sidecar signature covers the placeholder it now carries"
        );
    }

    /// **Issue #588, through the real import.** With `media-avif-decode` an AVIF original is a
    /// decodable still: it imports `Decoded`, with pixel dimensions, a placeholder and a JXL
    /// thumbnail, where without the feature it would be `DeferredNoCodec`. The source AVIF comes
    /// from ravif (`media-linked-codecs`) because the suite commits no binary fixtures.
    #[cfg(all(feature = "media-avif-decode", feature = "media-linked-codecs"))]
    #[test]
    fn an_avif_original_imports_with_dimensions_an_lqip_and_derivatives() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let avif = encode_rgb_image_to_vec(
            &frame(512, 384),
            &ImageMetadata::default(),
            &EncodeOptions::AvifRavif(rawshift_image::formats::export::RavifEncodeConfig {
                common: common(MetadataEmbedOptions::none()),
                quality: 90,
                speed: 6,
            }),
        )
        .expect("the fixture AVIF encodes");
        let receipt = import(&mut ws, album, src.path(), "photo.avif", &avif);
        assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);

        let sidecar = sidecar_of(lib.path(), receipt.asset_id);
        assert_eq!(sidecar.content_type, "image/avif");
        let dimensions = sidecar.dimensions.as_ref().expect("pixel dimensions");
        assert_eq!((dimensions.width, dimensions.height), (512, 384));
        assert!(
            sidecar.lqip.is_some(),
            "the placeholder came from the AVIF's pixels"
        );
        assert!(sidecar.verify(&ws.user_ik_public()));

        let stem = receipt.asset_id.simple().to_string();
        let thumb =
            derivatives_dir(lib.path(), receipt.asset_id).join(format!("{stem}.thumbnail.jxl"));
        let decoded = capsule_core::media::RawshiftDecoder
            .decode(&fs::read(&thumb).unwrap(), "jxl")
            .expect("the thumbnail decodes");
        assert_eq!((decoded.width(), decoded.height()), (256, 192));
    }

    /// The sidecar's dimensions are the **upright** ones. A quarter-turned JPEG's stored width
    /// is its EXIF `PixelXDimension`, transposed relative to what a viewer shows, so taking the
    /// decoded pixels rather than the tag is the whole point.
    #[test]
    fn a_rotated_still_records_upright_dimensions() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let receipt = import(
            &mut ws,
            album,
            src.path(),
            "portrait.jpg",
            &jpeg(320, 240, Some(6)),
        );
        assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);

        let sidecar = sidecar_of(lib.path(), receipt.asset_id);
        let dimensions = sidecar.dimensions.as_ref().expect("dimensions");
        assert_eq!(
            (dimensions.width, dimensions.height),
            (240, 320),
            "orientation 6 is a quarter-turn, so the sidecar records the transposed pair"
        );
    }

    /// Thumbnail bytes and a signed manifest bundle land on disk, at the layout the upload
    /// bundle reader already looks for, and the bundle re-verifies against the bytes beside it.
    #[test]
    fn an_import_persists_thumbnail_bytes_and_a_verifying_manifest_bundle() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let receipt = import(&mut ws, album, src.path(), "big.png", &png(512, 384));
        assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);

        let dir = derivatives_dir(lib.path(), receipt.asset_id);
        let stem = receipt.asset_id.simple().to_string();
        let thumb = dir.join(format!("{stem}.thumbnail.jxl"));
        let bundle_path = dir.join(format!("{stem}.derivatives.cbor"));
        assert!(thumb.is_file(), "thumbnail bytes at {}", thumb.display());
        assert!(bundle_path.is_file(), "a manifest bundle beside them");

        let bytes = fs::read(&thumb).unwrap();
        let manifests: Vec<DerivativeManifest> =
            cbor::from_slice(&fs::read(&bundle_path).unwrap()).expect("the bundle decodes");
        // One manifest per generated (tier, format) pair: the thumbnail JXL alone without
        // `media-linked-codecs`, thumbnail and preview in JXL and AVIF with it. The thumbnail
        // JXL is first either way.
        assert_eq!(manifests.len(), encoded_pairs_per_large_still());
        let core = &manifests[0].core;
        assert_ne!(
            core.ciphertext_hash,
            hash::hash_bytes(&bytes),
            "the manifest addresses the ciphertext, never the plaintext on disk"
        );
        assert_ne!(
            core.nonce_prefix, [0u8; 7],
            "and it records the prefix that ciphertext was produced under"
        );
        assert_eq!(core.source_asset_id, receipt.asset_id);
        assert_eq!(
            verify_still_format(&manifests[0]),
            Ok(Some(DerivativeFormat::Jxl)),
            "the persisted format is inside the closed set"
        );

        // The bytes really are a 256 px JXL.
        let decoded = capsule_core::media::RawshiftDecoder
            .decode(&bytes, "jxl")
            .expect("the persisted thumbnail decodes");
        assert_eq!((decoded.width(), decoded.height()), (256, 192));
    }

    /// A still already inside the tier cap gets the signed `original` sentinel: a manifest that
    /// says `original` and content-addresses the source, and **no derivative bytes on disk**.
    ///
    /// The absent bytes are the point, and they are what "the tier *references* the original"
    /// means. Writing a copy would put the original — EXIF and GPS intact — in
    /// `derivatives/{uuid}.thumbnail.{ext}` and therefore into the derivative blob of the upload
    /// bundle, which is the one place a re-encoded thumbnail is metadata-free by construction.
    /// The signed marker is still there, so this stays distinct from an absent derivative, which
    /// means "rebuild me".
    #[test]
    fn a_small_still_persists_the_original_sentinel_without_copying_it() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let original = png(128, 96);
        let receipt = import(&mut ws, album, src.path(), "small.png", &original);
        assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);
        // The sentinel satisfies the thumbnail tier, so nothing is deferred there. The preview
        // tier (generated under `media-linked-codecs`) has no cap, so it is a re-encode and
        // defers whatever this build cannot encode — WebP.
        let preview_deferred: u32 = if DerivativeTier::GENERATED.contains(&DerivativeTier::Preview)
        {
            (DerivativeFormat::STILL_DELIVERY_ORDER.len() - delivery_formats_encoded()) as u32
        } else {
            0
        };
        assert_eq!(
            receipt.deferred_formats, preview_deferred,
            "the sentinel satisfies the thumbnail tier, so nothing was deferred for it"
        );

        let dir = derivatives_dir(lib.path(), receipt.asset_id);
        let stem = receipt.asset_id.simple().to_string();

        let files: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.contains(".preview."))
            .collect();
        assert_eq!(
            files,
            vec![format!("{stem}.derivatives.cbor")],
            "the sentinel writes its manifest and no thumbnail bytes"
        );

        let manifests: Vec<DerivativeManifest> =
            cbor::from_slice(&fs::read(dir.join(format!("{stem}.derivatives.cbor"))).unwrap())
                .expect("the bundle decodes");
        let manifests: Vec<DerivativeManifest> = manifests
            .into_iter()
            .filter(|m| m.core.role == DerivativeRole::Thumbnail)
            .collect();
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].core.format, "original");
        // The sentinel references the original *blob*: it signs the original manifest's own
        // ciphertext address and nonce prefix, not the plaintext's.
        let state = ws.asset(&receipt.asset_id).expect("the asset is held");
        let original_core = &state
            .chain
            .records()
            .last()
            .expect("a create record")
            .manifest
            .core;
        assert_eq!(
            manifests[0].core.ciphertext_hash, original_core.ciphertext_hash,
            "the sentinel points at the blob a receiver already holds"
        );
        assert_eq!(
            manifests[0].core.nonce_prefix, original_core.nonce_prefix,
            "under the same key the original was encrypted with"
        );
        assert_ne!(
            manifests[0].core.ciphertext_hash,
            hash::hash_bytes(&original),
            "which is not the plaintext's address"
        );
        assert_eq!(
            verify_still_format(&manifests[0]),
            Ok(Some(DerivativeFormat::Original)),
            "the sentinel is inside the closed set"
        );
    }

    /// **Decision 22's degradation path, through the real import.** A codec that refuses a frame
    /// the decoder accepted costs this asset its thumbnail and **nothing else**: the original
    /// commits, signed and self-verifying, with real pixel dimensions and a real placeholder,
    /// and the run reports `DecodeFailed` so somebody can look at it.
    ///
    /// Reverting `prepare_still`'s match arm to a bare `?` must fail this test — that is what it
    /// is for. Before the review round the code did exactly that, and because the failure
    /// happened *before* `write_asset_files`, an encoder refusal lost the original from the
    /// backup outright.
    #[test]
    fn a_codec_refusal_costs_the_thumbnail_and_not_the_backup() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());
        let path = src.path().join("photo.png");
        fs::write(&path, png(512, 384)).unwrap();

        let receipt = super::with_sealer_fault(super::SealerFault::Refuse, || {
            ws.import_asset_with(album, &path, &SignedImportOptions::default())
                .expect("a codec refusal must never fail the import")
        });

        assert_eq!(
            receipt.derivatives,
            DerivativeStatus::DecodeFailed,
            "reported as a real problem rather than an expected gap"
        );
        assert_eq!(receipt.deferred_formats, 0);

        // The half that must not be lost: a signed, encrypted, self-verifying original.
        assert_eq!(
            ws.verify(&receipt.asset_id).unwrap(),
            capsule_core::crypto::verify_asset::VerifyOutcome::Accept
        );
        let sidecar = sidecar_of(lib.path(), receipt.asset_id);
        let dimensions = sidecar.dimensions.as_ref().expect("real pixel dimensions");
        assert_eq!((dimensions.width, dimensions.height), (512, 384));
        assert_eq!(
            sidecar.lqip.as_ref().map(|l| l.chromahash.len()),
            Some(32),
            "the placeholder came from the decode, which succeeded"
        );
        assert!(
            !derivatives_dir(lib.path(), receipt.asset_id).exists(),
            "and no derivative was written"
        );
    }

    /// **Decision 23's guard, through the real import.** A codec that *panics* on a frame the
    /// decoder accepted is caught, and the import still commits.
    ///
    /// Driven through `import_asset_with` rather than by calling `guarded` directly: a test that
    /// calls the guard itself stays green even if every production call site is deleted, which
    /// is precisely the hole this replaces. Deleting the guard around generation makes this test
    /// abort the process rather than fail — which is the failure mode it exists to prevent.
    #[test]
    fn a_codec_panic_is_caught_and_the_import_still_commits() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());
        let path = src.path().join("photo.png");
        fs::write(&path, png(512, 384)).unwrap();

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let receipt = super::with_sealer_fault(super::SealerFault::Panic, || {
            ws.import_asset_with(album, &path, &SignedImportOptions::default())
                .expect("a codec panic must never fail the import")
        });
        std::panic::set_hook(previous);

        assert_eq!(receipt.derivatives, DerivativeStatus::DecodeFailed);
        assert_eq!(
            ws.verify(&receipt.asset_id).unwrap(),
            capsule_core::crypto::verify_asset::VerifyOutcome::Accept,
            "one panicking photo does not cost the asset, let alone the rest of the run"
        );
        let sidecar = sidecar_of(lib.path(), receipt.asset_id);
        assert!(sidecar.lqip.is_some(), "the placeholder still landed");
    }

    /// A format with no codec here, and bytes that are no still at all: both import as signed,
    /// verifiable originals, with EXIF-or-nothing dimensions, no placeholder, no derivative
    /// files, and the reason recorded.
    #[test]
    fn an_undecodable_original_still_imports_with_the_reason_recorded() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        // A real HEIC header (ISO-BMFF `ftyp heic`) with no payload: recognised, no codec.
        let mut heic = vec![0, 0, 0, 0x20];
        heic.extend_from_slice(b"ftypheic");
        heic.extend_from_slice(&[0; 16]);
        let deferred = import(&mut ws, album, src.path(), "shot.heic", &heic);
        assert_eq!(deferred.derivatives, DerivativeStatus::DeferredNoCodec);
        assert_eq!(deferred.deferred_formats, 0, "nothing was attempted");

        let sidecar = sidecar_of(lib.path(), deferred.asset_id);
        assert!(sidecar.lqip.is_none(), "no pixels, no placeholder");
        assert!(
            sidecar.dimensions.is_none(),
            "no pixels and no EXIF dimensions"
        );
        assert_eq!(
            sidecar.content_type, "image/heic",
            "the header still names the format, codec or not"
        );
        assert!(
            !derivatives_dir(lib.path(), deferred.asset_id).exists(),
            "no derivative directory is created for an asset with no derivatives"
        );

        // Not a still at all: a video, whose derivatives are deferred until video codecs land
        // rather than inapplicable (issue #695).
        let video = import(
            &mut ws,
            album,
            src.path(),
            "clip.mp4",
            b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom",
        );
        assert_eq!(video.derivatives, DerivativeStatus::DeferredNoCodec);
        assert_eq!(
            sidecar_of(lib.path(), video.asset_id).content_type,
            "video/mp4",
            "the extension table still types a video"
        );

        // Both are signed, encrypted, self-verifying backups regardless.
        for id in [deferred.asset_id, video.asset_id] {
            assert_eq!(
                ws.verify(&id).unwrap(),
                capsule_core::crypto::verify_asset::VerifyOutcome::Accept
            );
        }
    }

    /// Seal `bytes` as a guest drop claiming `content_type`, stage it, and adopt it.
    fn adopt(
        ws: &mut Workspace,
        album: Uuid,
        bytes: &[u8],
        content_type: &str,
    ) -> (capsule_core::drop::AdoptedDrop, Vec<u8>) {
        use capsule_core::drop::{DropAdopter, LinkCaps, UploadLinkIssuer, seal_drop};

        let link = UploadLinkIssuer::create_link(ws, LinkCaps::default(), None).unwrap();
        let sealed = seal_drop(bytes, &link.drop_pubkey, content_type).unwrap();
        let ciphertext = sealed.ciphertext.clone();
        let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();
        let adopted = DropAdopter::adopt(ws, drop_id, album).expect("the adoption commits");
        (adopted, ciphertext)
    }

    /// **Issue #555's acceptance case.** An adopted drop runs the import's still pipeline: a
    /// decodable JPEG gets pixel dimensions, a chromahash LQIP and a header-derived
    /// `content_type` in its signed sidecar, and signed thumbnail derivatives on disk — under
    /// their own AMK-derived keys, beside an original whose key is the guest's wrapped `K` —
    /// and the asset still `verify`-accepts and pushes with those derivatives in its bundle.
    #[test]
    fn a_decodable_drop_adopts_with_dimensions_an_lqip_and_derivatives() {
        let lib = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        // The guest claims a generic type; the header is what the sidecar records.
        let (adopted, staged) = adopt(
            &mut ws,
            album,
            &jpeg(512, 384, None),
            "application/octet-stream",
        );
        let asset_id = adopted.asset_id();
        assert_eq!(adopted.derivatives, DerivativeStatus::Decoded);
        assert_eq!(
            adopted.manifest.core.key_mode,
            capsule_core::crypto::provenance::KeyMode::Wrapped
        );

        let sidecar = sidecar_of(lib.path(), asset_id);
        let dimensions = sidecar.dimensions.as_ref().expect("dimensions from pixels");
        assert_eq!((dimensions.width, dimensions.height), (512, 384));
        assert_eq!(sidecar.content_type, "image/jpeg", "header-derived");
        let lqip = sidecar.lqip.as_ref().expect("the LQIP producer ran");
        assert_eq!(lqip.chromahash.len(), 32);
        assert!(sidecar.verify(&ws.user_ik_public()));

        // At least one signed derivative manifest, sealed under a prefix other than the
        // guest's, beside thumbnail bytes on disk.
        let dir = derivatives_dir(lib.path(), asset_id);
        let stem = asset_id.simple().to_string();
        let manifests: Vec<DerivativeManifest> =
            cbor::from_slice(&fs::read(dir.join(format!("{stem}.derivatives.cbor"))).unwrap())
                .expect("the bundle decodes");
        assert_eq!(manifests.len(), encoded_pairs_per_large_still());
        assert_eq!(manifests[0].core.source_asset_id, asset_id);
        assert_eq!(
            verify_still_format(&manifests[0]),
            Ok(Some(DerivativeFormat::Jxl))
        );
        assert_ne!(
            manifests[0].core.nonce_prefix,
            adopted.manifest.core.nonce_prefix
        );
        assert!(dir.join(format!("{stem}.thumbnail.jxl")).is_file());

        // The asset still verifies, its index row carries the dimensions, and a push bundle
        // carries the guest's ciphertext byte for byte plus derivatives whose re-derived
        // ciphertext matches their signed address (the bundle reader drops any that do not).
        assert_eq!(
            ws.verify(&asset_id).unwrap(),
            capsule_core::crypto::verify_asset::VerifyOutcome::Accept
        );
        let row = ws
            .db()
            .find_by_uuid(&asset_id.to_string())
            .unwrap()
            .expect("an index row");
        assert_eq!((row.width, row.height), (Some(512), Some(384)));
        let bundle = ws.upload_bundle(&asset_id).unwrap();
        assert_eq!(bundle.original.to_vec().unwrap(), staged);
        assert_eq!(bundle.content_type, "image/jpeg");
        assert!(
            !bundle.derivatives.is_empty(),
            "the derived-key derivatives ship beside the wrapped-key original"
        );
    }

    /// An undecodable drop still adopts: no placeholder, no derivatives, the reason reported in
    /// [`AdoptedDrop::derivatives`](capsule_core::drop::AdoptedDrop::derivatives), and a
    /// `verify`-accepting asset regardless.
    #[test]
    fn an_undecodable_drop_still_adopts_with_the_reason_reported() {
        let lib = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let mut heic = vec![0, 0, 0, 0x20];
        heic.extend_from_slice(b"ftypheic");
        heic.extend_from_slice(&[0; 16]);
        let cases: [(&[u8], &str, DerivativeStatus, &str); 3] = [
            // Recognised by its header, no codec in this build.
            (
                &heic,
                "image/jpeg",
                DerivativeStatus::DeferredNoCodec,
                "image/heic",
            ),
            // A JPEG header over bytes that do not decode.
            (
                b"\xFF\xD8\xFF not really a photo",
                "image/jpeg",
                DerivativeStatus::DecodeFailed,
                "image/jpeg",
            ),
            // No still at all: the guest's claimed type is the fallback.
            (
                b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom",
                "video/mp4",
                DerivativeStatus::NotAKnownStill,
                "video/mp4",
            ),
        ];
        for (bytes, claimed, status, content_type) in cases {
            let (adopted, _) = adopt(&mut ws, album, bytes, claimed);
            let asset_id = adopted.asset_id();
            assert_eq!(adopted.derivatives, status, "{content_type}");
            assert_eq!(adopted.deferred_formats, 0, "nothing was attempted");
            let sidecar = sidecar_of(lib.path(), asset_id);
            assert_eq!(sidecar.content_type, content_type);
            assert!(sidecar.lqip.is_none());
            assert!(sidecar.dimensions.is_none(), "no pixels and no EXIF");
            assert!(
                !derivatives_dir(lib.path(), asset_id)
                    .join(format!("{}.derivatives.cbor", asset_id.simple()))
                    .exists(),
                "no derivative bundle for an asset with no derivatives"
            );
            assert_eq!(
                ws.verify(&asset_id).unwrap(),
                capsule_core::crypto::verify_asset::VerifyOutcome::Accept
            );
        }
    }
}

// ── The nonce-prefix reuse refusal (encryption.md, "Re-keying on Rewrite") ───

#[cfg(test)]
mod sealer_tests {
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
}
