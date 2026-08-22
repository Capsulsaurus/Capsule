package com.justin13888.capsule.core.designsystem.theme

import androidx.compose.ui.graphics.Color

/**
 * The tone stops every [CapsuleRamp] carries, ascending.
 *
 * These are the Material tone positions the colour roles actually reference. Storing a
 * sparse ramp rather than all 101 tones keeps the generated table reviewable, and a
 * missing stop fails loudly at first use instead of silently rounding to a neighbour.
 */
private val ToneStops =
    intArrayOf(0, 4, 6, 10, 12, 17, 20, 22, 24, 30, 40, 50, 60, 70, 80, 87, 90, 92, 94, 95, 96, 98, 100)

/**
 * One hue's tonal ramp, indexed by Material tone.
 *
 * Every ramp below was generated in OKLCH — fixed chroma and hue, lightness driven by
 * the CIE L* of the requested tone, then gamut-mapped by reducing chroma until the
 * colour is representable in sRGB. That is what makes contrast predictable: a role
 * pair like tone 10 on tone 90 has the same perceptual separation in every ramp, so
 * the semantic colours below inherit the contrast guarantees of the Material roles
 * without anyone hand-checking 250 swatches.
 */
@JvmInline
internal value class CapsuleRamp(
    private val argb: LongArray,
) {
    /** The colour at [tone]. [tone] must be one of the generated stops. */
    fun tone(tone: Int): Color {
        val index = ToneStops.binarySearch(tone)
        require(index >= 0) { "tone $tone is not a generated stop" }
        return Color(argb[index])
    }
}

/**
 * The surface family; near-achromatic so photographs set the colour of a screen. Generated at
 * OKLCH chroma 0.006, hue 250.
 */
internal val NeutralRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000000,
            0xFF0C0E11,
            0xFF111416,
            0xFF191C1E,
            0xFF1D2022,
            0xFF282A2D,
            0xFF2E3133,
            0xFF323538,
            0xFF37393C,
            0xFF44474A,
            0xFF5C5F62,
            0xFF74777B,
            0xFF8E9194,
            0xFFA8ACAF,
            0xFFC4C7CA,
            0xFFD7DADE,
            0xFFE0E3E6,
            0xFFE5E9EC,
            0xFFEBEEF2,
            0xFFEEF1F5,
            0xFFF1F4F8,
            0xFFF6FAFD,
            0xFFFFFFFF,
        ),
    )

/**
 * Outlines and dividers; just enough cast to separate from the surfaces. Generated at OKLCH
 * chroma 0.016, hue 250.
 */
internal val NeutralVariantRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000000,
            0xFF090F15,
            0xFF0E141A,
            0xFF161C23,
            0xFF1A2027,
            0xFF242B32,
            0xFF2A3138,
            0xFF2F363D,
            0xFF333A41,
            0xFF40484F,
            0xFF585F67,
            0xFF707880,
            0xFF8A929A,
            0xFFA4ACB5,
            0xFFBFC8D1,
            0xFFD2DBE5,
            0xFFDBE4ED,
            0xFFE0E9F3,
            0xFFE6EFF9,
            0xFFE9F2FB,
            0xFFECF5FE,
            0xFFF5FAFF,
            0xFFFFFFFF,
        ),
    )

/**
 * The Capsule brand seed — the accent reserved for selection and focus. Generated at OKLCH
 * chroma 0.105, hue 240.
 */
internal val PrimaryRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000002,
            0xFF00101D,
            0xFF001525,
            0xFF001E31,
            0xFF002237,
            0xFF002D47,
            0xFF003450,
            0xFF003957,
            0xFF003D5E,
            0xFF004C72,
            0xFF0F6593,
            0xFF327EAE,
            0xFF4E99CA,
            0xFF69B4E7,
            0xFF8BCFFF,
            0xFFB6E0FF,
            0xFFC8E7FF,
            0xFFD3ECFF,
            0xFFDEF1FF,
            0xFFE4F3FF,
            0xFFE9F6FF,
            0xFFF4FAFF,
            0xFFFFFFFF,
        ),
    )

/**
 * Supporting accent for containers that must recede behind primary. Generated at OKLCH chroma
 * 0.038, hue 240.
 */
internal val SecondaryRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000000,
            0xFF00101D,
            0xFF021522,
            0xFF091E2B,
            0xFF0D2230,
            0xFF172C3B,
            0xFF1E3342,
            0xFF223746,
            0xFF273C4B,
            0xFF344A59,
            0xFF4B6272,
            0xFF637B8C,
            0xFF7C94A6,
            0xFF96AFC1,
            0xFFB1CADD,
            0xFFC4DEF1,
            0xFFCDE7FA,
            0xFFD3ECFF,
            0xFFDEF1FF,
            0xFFE4F3FF,
            0xFFE9F6FF,
            0xFFF4FAFF,
            0xFFFFFFFF,
        ),
    )

/**
 * The rarely-used third accent; kept muted so it never competes with content. Generated at
 * OKLCH chroma 0.055, hue 305.
 */
internal val TertiaryRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000001,
            0xFF140621,
            0xFF190B27,
            0xFF221430,
            0xFF261834,
            0xFF302240,
            0xFF372947,
            0xFF3B2D4B,
            0xFF403250,
            0xFF4E3F5F,
            0xFF665778,
            0xFF7E6F92,
            0xFF9889AC,
            0xFFB3A3C8,
            0xFFCFBEE4,
            0xFFE2D2F8,
            0xFFEADAFF,
            0xFFEEE2FF,
            0xFFF2E9FF,
            0xFFF5EDFF,
            0xFFF7F0FF,
            0xFFFBF8FF,
            0xFFFFFFFF,
        ),
    )

/** Destructive confirmation and hard failures. Generated at OKLCH chroma 0.16, hue 28. */
internal val ErrorRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000000,
            0xFF250000,
            0xFF2E0000,
            0xFF3C0001,
            0xFF430001,
            0xFF560002,
            0xFF610003,
            0xFF690003,
            0xFF710004,
            0xFF890307,
            0xFFA62B25,
            0xFFC3483E,
            0xFFE26457,
            0xFFFF8072,
            0xFFFFAFA4,
            0xFFFFCCC5,
            0xFFFFD8D2,
            0xFFFFE0DB,
            0xFFFFE8E4,
            0xFFFFECE9,
            0xFFFFF0ED,
            0xFFFFF7F6,
            0xFFFFFFFF,
        ),
    )

/** Work the server has not acknowledged yet. Generated at OKLCH chroma 0.115, hue 85. */
internal val SyncPendingRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000000,
            0xFF150D00,
            0xFF1B1200,
            0xFF251900,
            0xFF2A1D00,
            0xFF372700,
            0xFF3F2D00,
            0xFF443100,
            0xFF4A3600,
            0xFF5B4200,
            0xFF785900,
            0xFF967106,
            0xFFB18B30,
            0xFFCCA54E,
            0xFFE9C16A,
            0xFFFDD47E,
            0xFFFFDE9B,
            0xFFFFE5B0,
            0xFFFFECC5,
            0xFFFFEFCF,
            0xFFFFF2D9,
            0xFFFFF9EC,
            0xFFFFFFFF,
        ),
    )

/**
 * A sync that gave up; distinct from `error` so a banner can show both. Generated at OKLCH
 * chroma 0.15, hue 22.
 */
internal val SyncFailedRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000000,
            0xFF240002,
            0xFF2D0004,
            0xFF3C0006,
            0xFF430008,
            0xFF55000D,
            0xFF610010,
            0xFF690012,
            0xFF700014,
            0xFF850E1E,
            0xFFA23036,
            0xFFBF4B4D,
            0xFFDD6666,
            0xFFFC8180,
            0xFFFFAEAB,
            0xFFFFCCC9,
            0xFFFFD8D5,
            0xFFFFE0DE,
            0xFFFFE8E6,
            0xFFFFECEA,
            0xFFFFEFEE,
            0xFFFFF7F7,
            0xFFFFFFFF,
        ),
    )

/**
 * Content held back by moderation — deliberately unlike any success or failure hue. Generated
 * at OKLCH chroma 0.12, hue 320.
 */
internal val QuarantineRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000002,
            0xFF1C0023,
            0xFF24002C,
            0xFF30003A,
            0xFF360040,
            0xFF420B4D,
            0xFF491355,
            0xFF4E195A,
            0xFF531E5F,
            0xFF622D6E,
            0xFF7C4588,
            0xFF965EA3,
            0xFFB177BE,
            0xFFCD92DA,
            0xFFE9ADF7,
            0xFFF5C8FF,
            0xFFF7D5FF,
            0xFFF9DDFF,
            0xFFFAE6FF,
            0xFFFBEAFF,
            0xFFFCEEFF,
            0xFFFDF7FF,
            0xFFFFFFFF,
        ),
    )

/**
 * A capability running below its contract (proxy-only originals, stale index). Generated at
 * OKLCH chroma 0.045, hue 70.
 */
internal val DegradedRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000000,
            0xFF180B00,
            0xFF1F0F00,
            0xFF291702,
            0xFF2D1B05,
            0xFF38260F,
            0xFF3F2C16,
            0xFF44311A,
            0xFF48351E,
            0xFF56432C,
            0xFF6F5A42,
            0xFF89735A,
            0xFFA38C73,
            0xFFBEA78D,
            0xFFDAC2A8,
            0xFFEED6BB,
            0xFFF7DEC3,
            0xFFFCE4C8,
            0xFFFFEAD3,
            0xFFFFEEDB,
            0xFFFFF1E2,
            0xFFFFF8F1,
            0xFFFFFFFF,
        ),
    )

/** A completed, durable, verified state. Generated at OKLCH chroma 0.105, hue 150. */
internal val SuccessRamp =
    CapsuleRamp(
        longArrayOf(
            0xFF000000,
            0xFF001304,
            0xFF001906,
            0xFF00230A,
            0xFF00280D,
            0xFF003413,
            0xFF003C17,
            0xFF004119,
            0xFF00461C,
            0xFF075626,
            0xFF296E3D,
            0xFF448855,
            0xFF5FA36E,
            0xFF79BE88,
            0xFF94DAA2,
            0xFFA7EEB5,
            0xFFB0F7BE,
            0xFFB5FDC3,
            0xFFC5FFD0,
            0xFFD0FFD8,
            0xFFDAFFE0,
            0xFFEDFFF0,
            0xFFFFFFFF,
        ),
    )
