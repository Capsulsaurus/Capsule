package com.justin13888.capsule.feature.sharing.qr

import kotlin.math.abs
import kotlin.math.max

/**
 * Lays one version's modules out and picks its mask.
 *
 * A mutable builder rather than a pile of free functions because every step reads what the
 * previous one wrote, and the function-module map — which positions the data stream must
 * step over — has to be built by the same code that draws them. Keeping the two arrays
 * together makes it impossible for a pattern to be drawn without being reserved.
 */
internal class QrSymbol(
    private val spec: QrVersionSpec,
) {
    private val size = spec.size
    private val modules = BooleanArray(size * size)
    private val functions = BooleanArray(size * size)

    /** Draws the whole symbol and returns it. */
    fun build(codewords: IntArray): QrCode {
        drawFunctionPatterns()
        placeCodewords(codewords)
        val mask = chooseMask()
        QrMask.apply(mask, size, modules, functions)
        drawFormatBits(mask)
        return QrCode(spec.version, size, modules.copyOf())
    }

    private fun drawFunctionPatterns() {
        for (index in 0 until size) {
            // The timing patterns are what a decoder measures module pitch against.
            setFunction(6, index, index % 2 == 0)
            setFunction(index, 6, index % 2 == 0)
        }
        drawFinder(3, 3)
        drawFinder(size - 4, 3)
        drawFinder(3, size - 4)
        drawAlignmentPatterns()
        drawVersionBits()
        // Drawn now with a placeholder mask purely to reserve the cells; the real bits land
        // once the mask is chosen, which cannot happen before the data is in place.
        drawFormatBits(0)
    }

    private fun drawFinder(
        centreX: Int,
        centreY: Int,
    ) {
        for (dy in -FINDER_REACH..FINDER_REACH) {
            for (dx in -FINDER_REACH..FINDER_REACH) {
                val distance = max(abs(dx), abs(dy))
                // Distance 2 is the light ring inside the eye; distance 4 is the separator.
                setFunction(centreX + dx, centreY + dy, distance != 2 && distance != FINDER_REACH)
            }
        }
    }

    private fun drawAlignmentPatterns() {
        val centres = spec.alignmentCentres
        for (row in centres.indices) {
            for (column in centres.indices) {
                // The three finder corners already own those positions.
                val isCorner =
                    (row == 0 && column == 0) ||
                        (row == 0 && column == centres.lastIndex) ||
                        (row == centres.lastIndex && column == 0)
                if (isCorner) continue
                drawAlignment(centres[column], centres[row])
            }
        }
    }

    private fun drawAlignment(
        centreX: Int,
        centreY: Int,
    ) {
        for (dy in -2..2) {
            for (dx in -2..2) {
                setFunction(centreX + dx, centreY + dy, max(abs(dx), abs(dy)) != 1)
            }
        }
    }

    /**
     * Writes the fifteen format bits, twice.
     *
     * Two copies in two corners, because losing the format information loses the symbol
     * entirely — a decoder that cannot read the mask cannot unmask anything else.
     */
    private fun drawFormatBits(mask: Int) {
        val data = (LEVEL_M_BITS shl 3) or mask
        var remainder = data
        repeat(FORMAT_EC_BITS) { remainder = (remainder shl 1) xor ((remainder ushr 9) * FORMAT_GENERATOR) }
        val bits = ((data shl FORMAT_EC_BITS) or remainder) xor FORMAT_MASK

        for (i in 0..5) setFunction(8, i, bitAt(bits, i))
        setFunction(8, 7, bitAt(bits, 6))
        setFunction(8, 8, bitAt(bits, 7))
        setFunction(7, 8, bitAt(bits, 8))
        for (i in 9..14) setFunction(14 - i, 8, bitAt(bits, i))

        for (i in 0..7) setFunction(size - 1 - i, 8, bitAt(bits, i))
        for (i in 8..14) setFunction(8, size - 15 + i, bitAt(bits, i))
        // The one module that is always dark, whatever the format says.
        setFunction(8, size - 8, true)
    }

    /** Writes the eighteen version bits, present only from version 7 up. */
    private fun drawVersionBits() {
        if (spec.version < VERSION_BITS_FROM) return
        var remainder = spec.version
        repeat(VERSION_EC_BITS) { remainder = (remainder shl 1) xor ((remainder ushr 11) * VERSION_GENERATOR) }
        val bits = (spec.version shl VERSION_EC_BITS) or remainder
        for (i in 0 until 18) {
            val bit = bitAt(bits, i)
            val far = size - 11 + i % 3
            val near = i / 3
            setFunction(far, near, bit)
            setFunction(near, far, bit)
        }
    }

    /**
     * Threads the codeword stream through every module no function pattern claimed.
     *
     * Two modules wide, bottom-right upwards, reversing at each edge, skipping the vertical
     * timing column. Any module left over past the end of the stream stays light: those are
     * the standard's remainder bits, which carry nothing.
     */
    private fun placeCodewords(codewords: IntArray) {
        var bitIndex = 0
        var upward = true
        var right = size - 1
        while (right > 0) {
            if (right == 6) right -= 1
            for (step in 0 until size) {
                val y = if (upward) size - 1 - step else step
                bitIndex = placePair(codewords, bitIndex, right, y)
            }
            upward = !upward
            right -= 2
        }
    }

    /** Writes the two modules of one column pair at [y], returning the new stream position. */
    private fun placePair(
        codewords: IntArray,
        startBit: Int,
        right: Int,
        y: Int,
    ): Int {
        var bitIndex = startBit
        val totalBits = codewords.size * Byte.SIZE_BITS
        for (column in 0..1) {
            val x = right - column
            if (functions[y * size + x]) continue
            if (bitIndex < totalBits) {
                val byte = codewords[bitIndex / Byte.SIZE_BITS]
                modules[y * size + x] = bitAt(byte, 7 - bitIndex % Byte.SIZE_BITS)
            }
            bitIndex += 1
        }
        return bitIndex
    }

    /** The mask whose penalty score is lowest, ties going to the lowest-numbered mask. */
    private fun chooseMask(): Int {
        var best = 0
        var lowest = Int.MAX_VALUE
        for (candidate in 0 until QrMask.COUNT) {
            QrMask.apply(candidate, size, modules, functions)
            drawFormatBits(candidate)
            val penalty = QrMask.penalty(size, modules)
            if (penalty < lowest) {
                lowest = penalty
                best = candidate
            }
            // Exclusive-or is its own inverse, so re-applying restores the unmasked grid.
            QrMask.apply(candidate, size, modules, functions)
        }
        return best
    }

    private fun setFunction(
        x: Int,
        y: Int,
        dark: Boolean,
    ) {
        if (x !in 0 until size || y !in 0 until size) return
        modules[y * size + x] = dark
        functions[y * size + x] = true
    }

    private fun bitAt(
        value: Int,
        index: Int,
    ): Boolean = (value ushr index) and 1 != 0

    private companion object {
        /** How far a finder pattern reaches from its centre, separator included. */
        const val FINDER_REACH = 4

        /** Level M's two-bit code inside the format information. */
        const val LEVEL_M_BITS = 0b00

        /** BCH(15,5) error-correction bits on the format information. */
        const val FORMAT_EC_BITS = 10

        /** The BCH generator polynomial for the format information. */
        const val FORMAT_GENERATOR = 0x537

        /** The constant every format field is exclusive-ored with, so all-zero never occurs. */
        const val FORMAT_MASK = 0x5412

        /** The first version that carries a version-information block. */
        const val VERSION_BITS_FROM = 7

        /** BCH(18,6) error-correction bits on the version information. */
        const val VERSION_EC_BITS = 12

        /** The BCH generator polynomial for the version information. */
        const val VERSION_GENERATOR = 0x1F25
    }
}
