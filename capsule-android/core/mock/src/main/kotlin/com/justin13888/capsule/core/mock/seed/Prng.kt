package com.justin13888.capsule.core.mock.seed

/**
 * The one source of randomness in the mock backend, and the reason every generated library
 * is reproducible.
 *
 * `kotlin.random.Random(seed)` would also be seedable, but its algorithm is an
 * implementation detail of the standard library rather than a documented contract: a
 * stdlib upgrade could re-shuffle every screenshot golden in the repo without a single
 * line of Capsule changing. SplitMix64 is thirty lines, fully specified, and produces the
 * same stream on every JVM and every Kotlin version, so a golden captured today still
 * matches next year.
 *
 * Instances are **not** thread-safe by design. A shared, locked generator would make the
 * output depend on the interleaving of the threads that drew from it, which is exactly the
 * non-determinism this class exists to remove. Generation is single-threaded, and a
 * concurrent consumer takes its own [derive]d stream instead.
 */
class Prng(
    seed: Long,
) {
    private var state: Long = seed

    /** The next 64 bits of the stream. */
    fun nextLong(): Long {
        state += GOLDEN_GAMMA
        var z = state
        z = (z xor (z ushr SHIFT_A)) * MIX_A
        z = (z xor (z ushr SHIFT_B)) * MIX_B
        return z xor (z ushr SHIFT_C)
    }

    /** A uniform value in `0.0 until 1.0`. */
    fun nextDouble(): Double = (nextLong() ushr MANTISSA_SHIFT).toDouble() * MANTISSA_SCALE

    /** A uniform value in `0.0f until 1.0f`. */
    fun nextFloat(): Float = nextDouble().toFloat()

    /**
     * A uniform value in `0 until bound`.
     *
     * Rejection-free modulo: the bias is at most one part in 2^63 for any bound a generator
     * of mock content will ever pass, and accepting it keeps the draw count per call fixed —
     * which matters, because a variable number of draws would make the whole downstream
     * stream depend on values the caller never sees.
     */
    fun nextInt(bound: Int): Int {
        require(bound > 0) { "bound must be positive, was $bound" }
        return ((nextLong() ushr 1) % bound).toInt()
    }

    /** A uniform value in `min..max`, inclusive at both ends. */
    fun nextInt(
        min: Int,
        max: Int,
    ): Int {
        require(max >= min) { "max ($max) must not be below min ($min)" }
        return min + nextInt(max - min + 1)
    }

    /** A uniform value in `min..max`. */
    fun nextDouble(
        min: Double,
        max: Double,
    ): Double = min + nextDouble() * (max - min)

    /** `true` with probability [probability]. */
    fun chance(probability: Double): Boolean = nextDouble() < probability

    /** One element of [items], uniformly. */
    fun <T> pick(items: List<T>): T = items[nextInt(items.size)]

    /**
     * One element of [items], with [weights] proportional to how often it should appear.
     *
     * Weighted rather than uniform because a uniform draw is what makes generated content
     * look generated: a real library is mostly 3:2 and 4:3 with a rare panorama, not an even
     * spread over every aspect ratio the codec can represent.
     */
    fun <T> pickWeighted(
        items: List<T>,
        weights: List<Double>,
    ): T {
        require(items.size == weights.size) { "items and weights must be the same length" }
        val total = weights.sum()
        var target = nextDouble() * total
        for (index in items.indices) {
            target -= weights[index]
            if (target <= 0.0) return items[index]
        }
        return items.last()
    }

    /** [items] in a shuffled order — a Fisher-Yates draw, so the permutation is uniform. */
    fun <T> shuffled(items: List<T>): List<T> {
        val copy = items.toMutableList()
        for (index in copy.indices.reversed()) {
            val swap = nextInt(index + 1)
            val held = copy[index]
            copy[index] = copy[swap]
            copy[swap] = held
        }
        return copy
    }

    /**
     * An independent stream, named rather than numbered.
     *
     * Sub-streams are what stop one part of the generator perturbing every other part.
     * Adding a field to the EXIF profile would otherwise consume an extra draw and shift the
     * capture times, the GPS track and every id after it; deriving `"exif"` from the root
     * seed keeps that edit local to the EXIF stream.
     */
    fun derive(label: String): Prng = Prng(mixSeed(state, label))

    companion object {
        /** Derives a stable stream seed from a root seed and a label, without consuming a draw. */
        fun mixSeed(
            seed: Long,
            label: String,
        ): Long {
            var mixed = seed
            for (character in label) {
                mixed = (mixed xor character.code.toLong()) * FNV_PRIME
            }
            return mixed
        }

        private const val GOLDEN_GAMMA = -0x61c8864680b583ebL
        private const val MIX_A = -0x40a7b892e31b1a47L
        private const val MIX_B = -0x6b2fb644ecceee15L
        private const val SHIFT_A = 30
        private const val SHIFT_B = 27
        private const val SHIFT_C = 31
        private const val MANTISSA_SHIFT = 11
        private const val MANTISSA_SCALE = 1.0 / (1L shl 53).toDouble()
        private const val FNV_PRIME = 0x100000001b3L
    }
}
