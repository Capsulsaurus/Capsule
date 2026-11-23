package com.justin13888.capsule.feature.sharing.qr

import kotlin.math.abs
import kotlin.math.max

/**
 * An independent reading of a QR symbol, written against the standard rather than against
 * the encoder.
 *
 * Deliberately *not* sharing the encoder's private helpers. A decoder that reused the same
 * function-module map and the same mask table would confirm only that the encoder agrees
 * with itself; reimplementing both from the specification means a mistake in either one
 * shows up as a failed round trip.
 */
internal object QrReference {
    /** Decodes a byte-mode symbol back to its text, or throws if the structure is wrong. */
    fun decode(code: QrCode): String {
        val spec = QrVersionSpec.LEVEL_M.first { it.version == code.version }
        val mask = readMask(code)
        val stream = readCodewords(code, spec, mask)
        val data = deinterleave(stream, spec)
        return readPayload(data, spec)
    }

    /** The mask index the symbol's format information declares. */
    fun readMask(code: QrCode): Int = formatValue(code) ushr 10 and 0b111

    /** The error-correction level the symbol's format information declares; 0 is level M. */
    fun readLevel(code: QrCode): Int = formatValue(code) ushr 13 and 0b11

    /** Whether the module at [x], [y] belongs to a function pattern rather than the payload. */
    fun isFunction(
        size: Int,
        version: Int,
        x: Int,
        y: Int,
    ): Boolean =
        isTiming(x, y) ||
            isFinderBlock(size, x, y) ||
            isVersionBlock(size, version, x, y) ||
            isAlignment(version, x, y)

    /** The two lines a decoder measures module pitch against. */
    private fun isTiming(
        x: Int,
        y: Int,
    ): Boolean = x == 6 || y == 6

    /** A finder eye, its separator, or the format bits that sit against it. */
    private fun isFinderBlock(
        size: Int,
        x: Int,
        y: Int,
    ): Boolean {
        val topLeft = x < 9 && y < 9
        val topRight = x >= size - 8 && y < 9
        val bottomLeft = x < 9 && y >= size - 8
        return topLeft || topRight || bottomLeft
    }

    /** The two version-information blocks, present only from version 7 up. */
    private fun isVersionBlock(
        size: Int,
        version: Int,
        x: Int,
        y: Int,
    ): Boolean {
        if (version < 7) return false
        val nearTopRight = x >= size - 11 && x < size - 8 && y < 6
        val nearBottomLeft = y >= size - 11 && y < size - 8 && x < 6
        return nearTopRight || nearBottomLeft
    }

    /** Whether the module at [x], [y] falls inside an alignment pattern. */
    fun isAlignment(
        version: Int,
        x: Int,
        y: Int,
    ): Boolean {
        val centres = QrVersionSpec.LEVEL_M.first { it.version == version }.alignmentCentres
        for (row in centres.indices) {
            for (column in centres.indices) {
                val corner =
                    (row == 0 && column == 0) ||
                        (row == 0 && column == centres.lastIndex) ||
                        (row == centres.lastIndex && column == 0)
                if (corner) continue
                if (max(abs(x - centres[column]), abs(y - centres[row])) <= 2) return true
            }
        }
        return false
    }

    /** The standard's eight mask predicates, restated. */
    fun masked(
        pattern: Int,
        x: Int,
        y: Int,
    ): Boolean =
        when (pattern) {
            0 -> (x + y) % 2 == 0
            1 -> y % 2 == 0
            2 -> x % 3 == 0
            3 -> (x + y) % 3 == 0
            4 -> (y / 2 + x / 3) % 2 == 0
            5 -> (x * y) % 2 + (x * y) % 3 == 0
            6 -> ((x * y) % 2 + (x * y) % 3) % 2 == 0
            else -> ((x + y) % 2 + (x * y) % 3) % 2 == 0
        }

    private fun formatValue(code: QrCode): Int {
        var bits = 0
        for (i in 0..5) if (code.isDark(8, i)) bits = bits or (1 shl i)
        if (code.isDark(8, 7)) bits = bits or (1 shl 6)
        if (code.isDark(8, 8)) bits = bits or (1 shl 7)
        if (code.isDark(7, 8)) bits = bits or (1 shl 8)
        for (i in 9..14) if (code.isDark(14 - i, 8)) bits = bits or (1 shl i)
        return bits xor 0x5412
    }

    private fun readCodewords(
        code: QrCode,
        spec: QrVersionSpec,
        mask: Int,
    ): IntArray {
        val size = code.size
        val bits = ArrayList<Boolean>(spec.totalCodewords * 8)
        var upward = true
        var right = size - 1
        while (right > 0) {
            if (right == 6) right -= 1
            for (step in 0 until size) {
                val y = if (upward) size - 1 - step else step
                readPair(code, spec, mask, right, y, bits)
            }
            upward = !upward
            right -= 2
        }
        val stream = IntArray(spec.totalCodewords)
        for (index in stream.indices) {
            var byte = 0
            for (bit in 0 until 8) {
                byte = (byte shl 1) or if (bits[index * 8 + bit]) 1 else 0
            }
            stream[index] = byte
        }
        return stream
    }

    @Suppress("LongParameterList")
    private fun readPair(
        code: QrCode,
        spec: QrVersionSpec,
        mask: Int,
        right: Int,
        y: Int,
        into: MutableList<Boolean>,
    ) {
        for (column in 0..1) {
            val x = right - column
            if (isFunction(code.size, spec.version, x, y)) continue
            into += code.isDark(x, y) xor masked(mask, x, y)
        }
    }

    private fun deinterleave(
        stream: IntArray,
        spec: QrVersionSpec,
    ): IntArray {
        val lengths =
            IntArray(spec.blockCount) { index ->
                if (index < spec.shortBlocks) spec.shortBlockData else spec.longBlockData
            }
        val blocks = Array(spec.blockCount) { IntArray(lengths[it]) }
        var cursor = 0
        for (position in 0 until spec.longBlockData) {
            for (block in blocks.indices) {
                if (position < lengths[block]) blocks[block][position] = stream[cursor++]
            }
        }
        return blocks.fold(IntArray(0)) { acc, block -> acc + block }
    }

    private fun readPayload(
        data: IntArray,
        spec: QrVersionSpec,
    ): String {
        var cursor = 0

        fun take(count: Int): Int {
            var value = 0
            repeat(count) {
                val byte = data[cursor / 8]
                value = (value shl 1) or ((byte ushr (7 - cursor % 8)) and 1)
                cursor += 1
            }
            return value
        }
        require(take(QrVersionSpec.MODE_BITS) == 0b0100) { "not a byte-mode segment" }
        val length = take(spec.characterCountBits)
        val bytes = ByteArray(length) { take(8).toByte() }
        return String(bytes, Charsets.UTF_8)
    }
}
