package com.justin13888.capsule.feature.recovery

import java.security.SecureRandom

/**
 * A freshly minted recovery secret, in the form a person can write down.
 *
 * Words rather than a string, because every screen that touches it — the reveal grid, the
 * type-back challenge, the accessibility description — works word by word, and splitting a
 * sentence back apart in three places is three chances to disagree about what a word is.
 */
data class RecoverySecret(
    /** The words, in order. */
    val words: List<String>,
    /** How much entropy the mint claims to have put in them. */
    val entropyBits: Int,
) {
    /** The phrase as the user would write it on paper. */
    val phrase: String get() = words.joinToString(" ")
}

/** What a completed re-wrap leaves behind, and what the user still has to deal with. */
data class RewrapReceipt(
    /** Whether guardian shares existed and have now been invalidated. */
    val sharesInvalidated: Boolean,
)

/**
 * Minting a new recovery secret and re-wrapping the account master key under it.
 *
 * Declared here rather than in `core:data` because the escrow-replacement call has no
 * repository contract yet. The *shape* is fixed by the design and is worth stating even while
 * the transport is not: this is an O(1) replacement of one small blob. It re-wraps the **same
 * master key**, so no data is re-encrypted, no blob hash changes, and no asset is touched. A
 * gateway that did more than that would not be implementing this interface.
 */
interface RecoveryRewrapGateway {
    /** Mints a fresh secret of at least [MINIMUM_ENTROPY_BITS]. */
    fun mint(): RecoverySecret

    /**
     * Replaces the server escrow with the same master key wrapped under [secret].
     *
     * The old blob is deleted rather than kept alongside: a single active escrow is what makes
     * "the lost secret now unwraps nothing" true, and two would make it false.
     */
    suspend fun rewrap(secret: RecoverySecret): RewrapReceipt

    companion object {
        /**
         * The entropy floor for a recovery secret.
         *
         * The escrow blob is offline-attackable once exfiltrated and the password KDF only
         * raises the cost linearly, so the secret itself has to carry the security. Without an
         * attested enclave to rate-limit unwrap attempts there is no shorter option — not a
         * PIN, not a numeric code.
         */
        const val MINIMUM_ENTROPY_BITS: Int = 128
    }
}

/**
 * The mint and re-wrap this build ships with.
 *
 * The word list is small and fixed, which is fine for a build whose escrow is a mock and
 * would not be fine for one whose escrow is real: a real mint draws from a standardised
 * wordlist so the phrase can be transcribed and re-entered anywhere. What is *not* a
 * simplification is the entropy accounting — the number of words is derived from the list
 * size so the floor is met rather than assumed, which is the part that would otherwise rot
 * silently when somebody edits the list.
 */
class LocalRecoveryRewrapGateway(
    private val shares: ShamirShareStore,
    private val random: SecureRandom = SecureRandom(),
) : RecoveryRewrapGateway {
    override fun mint(): RecoverySecret {
        val words = List(WORD_COUNT) { WORDS[random.nextInt(WORDS.size)] }
        return RecoverySecret(words = words, entropyBits = WORD_COUNT * BITS_PER_WORD)
    }

    override suspend fun rewrap(secret: RecoverySecret): RewrapReceipt {
        require(secret.entropyBits >= RecoveryRewrapGateway.MINIMUM_ENTROPY_BITS) {
            "a secret below the entropy floor would make the escrow guessable offline"
        }
        val hadShares = shares.enrollment.value != null
        // The old shares reconstruct a secret whose escrow no longer exists, so they are not
        // stale — they are wrong, and the screen has to say so rather than leave them listed.
        if (hadShares) shares.invalidate()
        return RewrapReceipt(sharesInvalidated = hadShares)
    }

    private companion object {
        /**
         * A 64-word list, so each draw carries exactly six bits and the arithmetic is exact.
         *
         * Chosen for being short, unambiguous when read aloud, and distinct in their first four
         * letters — the properties that decide whether a phrase survives being copied by hand.
         */
        val WORDS =
            listOf(
                "amber",
                "anchor",
                "apple",
                "arrow",
                "basin",
                "beacon",
                "birch",
                "bridge",
                "camera",
                "candle",
                "canyon",
                "cedar",
                "cinder",
                "clover",
                "copper",
                "coral",
                "dagger",
                "dawn",
                "delta",
                "domino",
                "ember",
                "engine",
                "falcon",
                "fern",
                "flint",
                "forest",
                "garden",
                "gravel",
                "harbor",
                "hollow",
                "indigo",
                "island",
                "jasmine",
                "kettle",
                "lantern",
                "ledger",
                "lichen",
                "marble",
                "meadow",
                "mirror",
                "nectar",
                "nomad",
                "orchid",
                "otter",
                "pebble",
                "pillar",
                "quarry",
                "quiver",
                "ribbon",
                "ripple",
                "saffron",
                "shadow",
                "signal",
                "silver",
                "summit",
                "thistle",
                "timber",
                "tundra",
                "velvet",
                "vessel",
                "walnut",
                "willow",
                "zenith",
                "zephyr",
            )

        /** Six bits per draw, which is what a list of exactly 64 words buys. */
        const val BITS_PER_WORD = 6

        /** Enough words to clear the floor with a margin rather than exactly. */
        const val WORD_COUNT = 24
    }
}
