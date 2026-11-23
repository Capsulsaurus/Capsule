package com.justin13888.capsule.feature.sharing.qr

/**
 * A growable most-significant-bit-first bit stream.
 *
 * QR's payload is defined in bits that do not fall on byte boundaries — a four-bit mode
 * indicator, then an eight- or sixteen-bit count — so the encoder assembles bits and only
 * then pads to codewords. Doing that by hand at each call site is how a padding bug hides.
 */
internal class QrBitBuffer {
    private val bits = ArrayList<Boolean>()

    /** How many bits have been appended. */
    val size: Int get() = bits.size

    /** Appends the low [count] bits of [value], most significant first. */
    fun append(
        value: Int,
        count: Int,
    ) {
        for (i in count - 1 downTo 0) {
            bits.add((value ushr i) and 1 != 0)
        }
    }

    /** Appends every bit of [byte], most significant first. */
    fun appendByte(byte: Int) = append(byte, Byte.SIZE_BITS)

    /**
     * The stream as codewords, terminated and padded to [codewordCount].
     *
     * The padding is the standard's and is not arbitrary: up to four zero bits to close the
     * segment, zeroes to the next byte boundary, then `0xEC 0x11` repeating. A decoder reads
     * the declared length and stops, so the pad bytes are never mistaken for payload — but a
     * short or differently-padded stream shifts the error-correction blocks under it.
     */
    fun toCodewords(codewordCount: Int): IntArray {
        val capacityBits = codewordCount * Byte.SIZE_BITS
        val terminator = minOf(TERMINATOR_BITS, capacityBits - bits.size)
        append(0, terminator)
        append(0, (Byte.SIZE_BITS - bits.size % Byte.SIZE_BITS) % Byte.SIZE_BITS)

        val codewords = IntArray(codewordCount)
        for (index in bits.indices) {
            if (bits[index]) {
                codewords[index / Byte.SIZE_BITS] =
                    codewords[index / Byte.SIZE_BITS] or (1 shl (7 - index % Byte.SIZE_BITS))
            }
        }
        var pad = bits.size / Byte.SIZE_BITS
        var alternate = true
        while (pad < codewordCount) {
            codewords[pad] = if (alternate) PAD_EC else PAD_11
            alternate = !alternate
            pad++
        }
        return codewords
    }

    private companion object {
        /** The mode terminator, truncated when the symbol has less room than this. */
        const val TERMINATOR_BITS = 4

        /** The first of the two alternating pad codewords. */
        const val PAD_EC = 0xEC

        /** The second of the two alternating pad codewords. */
        const val PAD_11 = 0x11
    }
}
