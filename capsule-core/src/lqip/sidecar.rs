//! The bridge between an [`Lqip`] payload and the signed sidecar's `lqip` record.
//!
//! Gated on `native` for one reason only: [`crate::sidecar`] is itself `native`-gated. The
//! *encoder* in the parent module is unconditional and is what `capsule-wasm` links, so the two
//! surfaces never diverge — this file only assembles and reads the three fields the sidecar
//! carries.
//!
//! `lqip` is a **signed** field: the sidecar signature covers it, so its encoding is
//! signature-visible rather than a private detail. See [`LQIP_FORMAT_V1`] for why moving from
//! ThumbHash to chromahash keeps version `1` and does not move `sidecar_schema`.

use super::{LQIP_FORMAT_V1, Lqip, RgbaImage, render};
use crate::sidecar::sidecar_v1::Lqip as SidecarLqip;

impl Lqip {
    /// Build the encrypted-sidecar record: the chromahash payload, the [`LQIP_FORMAT_V1`] tag,
    /// and the `dominant_color` fill a reader paints when it cannot decode the payload.
    ///
    /// Infallible — `average_color` is a DC read that cannot fail, and the payload is this
    /// type's invariant.
    pub fn to_sidecar(&self) -> SidecarLqip {
        SidecarLqip {
            chromahash: self.as_bytes().to_vec(),
            format_version: LQIP_FORMAT_V1,
            dominant_color: self.dominant_color(),
        }
    }
}

/// Render a stored sidecar record to a displayable placeholder, band-limited to the box being
/// painted.
///
/// Delegates to [`render`], so an unrecognized `format_version` or an undecodable payload
/// yields the 1x1 solid `dominant_color` fill rather than a misrender.
pub fn render_sidecar_lqip(lqip: &SidecarLqip, max_width: u32, max_height: u32) -> RgbaImage {
    render(
        lqip.format_version,
        &lqip.chromahash,
        lqip.dominant_color,
        max_width,
        max_height,
    )
}

#[cfg(test)]
mod tests {
    use super::{LQIP_FORMAT_V1, Lqip, SidecarLqip, render_sidecar_lqip};
    use crate::lqip::{Gamut, RgbaImage};

    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let mut v = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&[
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x + y) * 255 / (w + h)) as u8,
                    255,
                ]);
            }
        }
        v
    }

    fn hash() -> Lqip {
        Lqip::encode(64, 48, &gradient(64, 48), Gamut::Srgb).expect("valid frame")
    }

    /// The record carries the 32-byte chromahash payload, the version tag, and the fallback
    /// colour — and nothing else (the source gamut is not among them).
    #[test]
    fn to_sidecar_carries_payload_version_and_fallback() {
        let lqip = hash();
        let record = lqip.to_sidecar();
        assert_eq!(record.chromahash, lqip.as_bytes());
        assert_eq!(record.chromahash.len(), 32);
        assert_eq!(record.format_version, LQIP_FORMAT_V1);
        assert_eq!(record.format_version, 1, "format version does not move");
        assert_eq!(record.dominant_color, lqip.dominant_color());
    }

    /// The `S-B14` acceptance case: a **signed** sidecar carrying a real chromahash payload
    /// signs, survives a canonical round trip byte-identically, and still verifies — with the
    /// schema version unchanged at `SIDECAR_SCHEMA_V1`.
    #[test]
    fn signed_sidecar_round_trips_with_a_chromahash_payload() {
        use std::collections::BTreeMap;

        use uuid::Uuid;

        use crate::crypto::hash::Hash32;
        use crate::crypto::keys::HybridSigningKey;
        use crate::crypto::primitives::CRYPTO_SUITE_ID;
        use crate::metadata::crdt::{Lww, OrSet};
        use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let lqip = hash();

        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: Uuid::from_u128(0xB14),
            hash: Hash32([0xAB; 32]),
            capture_timestamp: "2026-08-29T10:00:00Z".into(),
            import_timestamp: "2026-08-29T11:00:00Z".into(),
            content_type: "image/jpeg".into(),
            dimensions: None,
            lqip: None,
            tags_user: OrSet::new(),
            tags_ai: OrSet::new(),
            caption: Lww::new(),
            rating: Lww::new(),
            stack_membership: Lww::new(),
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: Uuid::from_u128(0xD1),
            session_id: Uuid::from_u128(0x5E),
            gps: None,
            provenance_chain_hash: None,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.lqip = Some(lqip.to_sidecar());
        sidecar.sign(&ik);
        assert!(sidecar.verify(&ik.verifying_key()));

        let bytes = sidecar.to_canonical_vec();
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1)
            .expect("canonical round trip");
        assert_eq!(back.lqip, sidecar.lqip);
        assert_eq!(
            back.lqip.as_ref().map(|l| l.chromahash.as_slice()),
            Some(lqip.as_bytes())
        );
        assert!(back.verify(&ik.verifying_key()));

        // The payload is covered by the signature: swapping it invalidates the sidecar.
        let mut tampered = back.clone();
        if let Some(l) = tampered.lqip.as_mut() {
            l.chromahash[0] ^= 0x01;
        }
        assert!(!tampered.verify(&ik.verifying_key()));
    }

    /// The record a reader actually holds decodes through the same path the encoder produced.
    #[test]
    fn render_sidecar_lqip_decodes_a_recognized_record() {
        let lqip = hash();
        let image = render_sidecar_lqip(&lqip.to_sidecar(), 16, 16);
        assert_eq!(image, lqip.decode_capped(16, 16));
        assert!(image.width > 1 && image.height > 1);
    }

    /// An unknown future version, and a stale/corrupt payload under a known one, both paint the
    /// stored `dominant_color` instead of misrendering.
    #[test]
    fn render_sidecar_lqip_falls_back_to_dominant_color() {
        let future = SidecarLqip {
            chromahash: vec![0xDE, 0xAD, 0xBE, 0xEF],
            format_version: LQIP_FORMAT_V1 + 999,
            dominant_color: [12, 34, 56],
        };
        assert_eq!(
            render_sidecar_lqip(&future, 32, 32),
            RgbaImage::solid([12, 34, 56])
        );

        let corrupt = SidecarLqip {
            chromahash: vec![0xDE, 0xAD, 0xBE, 0xEF],
            format_version: LQIP_FORMAT_V1,
            dominant_color: [65, 66, 67],
        };
        assert_eq!(
            render_sidecar_lqip(&corrupt, 32, 32),
            RgbaImage::solid([65, 66, 67])
        );
    }
}
