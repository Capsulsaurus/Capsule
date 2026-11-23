package com.justin13888.capsule.feature.recovery

import java.security.SecureRandom

/**
 * Splitting one secret into shares, any `m` of which rebuild it.
 *
 * Shamir over GF(2^8) with the AES reduction polynomial, byte by byte: each byte of the
 * secret becomes the constant term of a degree `m-1` polynomial whose other coefficients are
 * random, and share `i` is that polynomial evaluated at `x = i`. Fewer than `m` shares leave
 * the constant term uniformly distributed, which is the property that makes a single share
 * safe to hand to somebody.
 *
 * This lives in the feature rather than in `core` because the split is what the *screen*
 * needs to have something to hand out, and because `capsule-core::backup` owns the real one —
 * the one that splits the account's actual recovery seed. Keeping this here means the screen
 * is complete and testable now, and that replacing it is one call site.
 *
 * Byte arithmetic is done in `Int` throughout and masked on the way out. Kotlin's `Byte` is
 * signed, and the sign extension in the obvious implementation is the classic way this
 * algorithm silently produces shares that do not recombine.
 */
object ShamirSplitter {
    /** The smallest useful threshold: one share alone must never be enough. */
    const val MIN_THRESHOLD: Int = 2

    /** The largest share count, bounded by the number of non-zero evaluation points. */
    const val MAX_SHARES: Int = 255

    /**
     * Splits [secret] into [total] shares, [threshold] of which reconstruct it.
     *
     * @throws IllegalArgumentException when the parameters could not describe a real scheme.
     */
    fun split(
        secret: ByteArray,
        threshold: Int,
        total: Int,
        random: SecureRandom = SecureRandom(),
    ): List<ShamirShareBytes> {
        require(threshold >= MIN_THRESHOLD) { "a threshold below $MIN_THRESHOLD would not be a secret" }
        require(total in threshold..MAX_SHARES) { "cannot need $threshold of $total shares" }
        require(secret.isNotEmpty()) { "there is nothing to split" }

        val coefficients =
            Array(secret.size) { index ->
                IntArray(threshold).also { row ->
                    row[0] = secret[index].toInt() and BYTE
                    for (degree in 1 until threshold) row[degree] = random.nextInt(FIELD)
                }
            }
        return (1..total).map { x ->
            val payload =
                ByteArray(secret.size) { index -> evaluate(coefficients[index], x).toByte() }
            ShamirShareBytes(x = x, payload = payload)
        }
    }

    /**
     * Rebuilds the secret from [shares].
     *
     * Lagrange interpolation at zero. Passing fewer than the threshold does not fail — it
     * returns a different, equally well-formed value — which is exactly why a share carries no
     * hint of how many others are needed: the scheme's security rests on that indistinguishability.
     */
    fun combine(shares: List<ShamirShareBytes>): ByteArray {
        require(shares.isNotEmpty()) { "there is nothing to combine" }
        val length = shares.first().payload.size
        require(shares.all { it.payload.size == length }) { "these shares are not of one secret" }
        require(shares.map(ShamirShareBytes::x).toSet().size == shares.size) { "a share was supplied twice" }

        return ByteArray(length) { index ->
            var accumulator = 0
            shares.forEach { share ->
                var basis = 1
                shares.filter { it.x != share.x }.forEach { other ->
                    basis = multiply(basis, divide(other.x, other.x xor share.x))
                }
                accumulator = accumulator xor multiply(share.payload[index].toInt() and BYTE, basis)
            }
            accumulator.toByte()
        }
    }

    private fun evaluate(
        coefficients: IntArray,
        x: Int,
    ): Int {
        var result = 0
        for (degree in coefficients.indices.reversed()) {
            result = multiply(result, x) xor coefficients[degree]
        }
        return result
    }

    private fun multiply(
        a: Int,
        b: Int,
    ): Int = if (a == 0 || b == 0) 0 else EXP[(LOG[a] + LOG[b]) % ORDER]

    private fun divide(
        a: Int,
        b: Int,
    ): Int {
        require(b != 0) { "two shares cannot share an evaluation point" }
        return if (a == 0) 0 else EXP[(LOG[a] - LOG[b] + ORDER) % ORDER]
    }

    private const val BYTE = 0xFF
    private const val FIELD = 256
    private const val ORDER = 255
    private const val REDUCTION = 0x1B

    private val EXP = IntArray(FIELD)
    private val LOG = IntArray(FIELD)

    init {
        // Powers of the generator 3, which cycles through every non-zero element of the field.
        var value = 1
        for (power in 0 until ORDER) {
            EXP[power] = value
            LOG[value] = power
            value = value xor xtime(value)
        }
    }

    /** Multiplication by 2 in GF(2^8), reduced by the AES polynomial. */
    private fun xtime(value: Int): Int {
        val shifted = value shl 1
        return (if (shifted and FIELD != 0) shifted xor REDUCTION else shifted) and BYTE
    }
}

/** One share, as the arithmetic produces it: an evaluation point and the bytes at it. */
data class ShamirShareBytes(
    /** The non-zero point this share was evaluated at. Two shares never share one. */
    val x: Int,
    /** The polynomial's value at [x], byte for byte with the secret. */
    val payload: ByteArray,
) {
    /** Content equality, because a share is its bytes rather than its identity. */
    override fun equals(other: Any?): Boolean =
        this === other || (other is ShamirShareBytes && x == other.x && payload.contentEquals(other.payload))

    override fun hashCode(): Int = 31 * x + payload.contentHashCode()
}
