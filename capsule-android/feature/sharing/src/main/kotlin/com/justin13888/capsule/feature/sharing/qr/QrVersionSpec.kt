package com.justin13888.capsule.feature.sharing.qr

/**
 * The block structure one QR version takes at error-correction level M.
 *
 * Only level M is tabulated. A share URL is read once, from a screen, at close range, and M
 * recovers 15% of the symbol — enough for a camera at an angle without spending a third of
 * the modules the way H does. Carrying the other three levels would be three more tables
 * nothing in this app selects.
 */
internal data class QrVersionSpec(
    /** The version number, 1 through 10. */
    val version: Int,
    /** Total codewords in the symbol, data and error correction together. */
    val totalCodewords: Int,
    /** Error-correction codewords in every block. */
    val ecPerBlock: Int,
    /** How many blocks carry [shortBlockData] codewords. */
    val shortBlocks: Int,
    /** Data codewords in a short block. */
    val shortBlockData: Int,
    /** How many blocks carry one more codeword than a short block. */
    val longBlocks: Int,
) {
    /** Data codewords in a long block — always exactly one more than a short one. */
    val longBlockData: Int get() = shortBlockData + 1

    /** Every block in the symbol. */
    val blockCount: Int get() = shortBlocks + longBlocks

    /** How many codewords the symbol carries payload in. */
    val dataCodewords: Int get() = shortBlocks * shortBlockData + longBlocks * longBlockData

    /** The symbol's width and height in modules. */
    val size: Int get() = 4 * version + 17

    /**
     * How many bits the character-count indicator takes in byte mode.
     *
     * Eight below version 10 and sixteen from there up. The boundary is the standard's, not
     * a choice: a decoder reads the count with the width its version implies, so guessing
     * wrong shifts every following bit.
     */
    val characterCountBits: Int get() = if (version < COUNT_BITS_BOUNDARY) 8 else 16

    /** The largest byte-mode payload this version holds. */
    val byteCapacity: Int get() = (dataCodewords * 8 - MODE_BITS - characterCountBits) / 8

    /**
     * Where alignment patterns are centred, along both axes.
     *
     * Version 1 has none; every later version puts one at 6 and one six modules in from the
     * far edge, with the rest spread evenly between them.
     */
    val alignmentCentres: List<Int>
        get() =
            when (version) {
                1 -> emptyList()
                in 2..6 -> listOf(6, 4 * version + 10)
                else -> listOf(6, 2 * version + 8, 4 * version + 10)
            }

    internal companion object {
        /** Byte mode's four-bit indicator width. */
        const val MODE_BITS = 4

        /** The first version whose character count needs sixteen bits. */
        const val COUNT_BITS_BOUNDARY = 10

        /**
         * Versions 1 through 10 at level M.
         *
         * Ten is the ceiling because a share URL is around a hundred bytes and version 10
         * holds 213 — headroom of twice the longest URL this app can produce, without
         * carrying thirty more rows that would never be selected.
         */
        val LEVEL_M: List<QrVersionSpec> =
            listOf(
                QrVersionSpec(1, 26, 10, 1, 16, 0),
                QrVersionSpec(2, 44, 16, 1, 28, 0),
                QrVersionSpec(3, 70, 26, 1, 44, 0),
                QrVersionSpec(4, 100, 18, 2, 32, 0),
                QrVersionSpec(5, 134, 24, 2, 43, 0),
                QrVersionSpec(6, 172, 16, 4, 27, 0),
                QrVersionSpec(7, 196, 18, 4, 31, 0),
                QrVersionSpec(8, 242, 22, 2, 38, 2),
                QrVersionSpec(9, 292, 22, 3, 36, 2),
                QrVersionSpec(10, 346, 26, 4, 43, 1),
            )

        /** The smallest version that holds [byteCount] bytes, or `null` if none does. */
        fun smallestFor(byteCount: Int): QrVersionSpec? = LEVEL_M.firstOrNull { byteCount <= it.byteCapacity }
    }
}
