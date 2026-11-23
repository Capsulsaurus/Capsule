package com.justin13888.capsule.feature.sharing.qr

import androidx.compose.runtime.Immutable

/**
 * A rendered QR symbol: a square grid of dark and light modules.
 *
 * Encoded in-app rather than through a barcode library. The one thing being encoded is a
 * share URL whose fragment carries the read secret, so the encoder is on the same trust
 * boundary as the clipboard: shipping that string through a third-party dependency to draw
 * squares is a dependency on the wrong side of the line, and byte-mode level-M encoding is
 * a few hundred lines of arithmetic a test can pin exactly.
 *
 * Immutable by construction — the module array is never handed out — so a composition can
 * read it without a defensive copy.
 */
@Immutable
class QrCode internal constructor(
    /** The symbol version, 1 through 10. Determines [size]. */
    val version: Int,
    /** The symbol's width and height in modules, quiet zone excluded. */
    val size: Int,
    private val modules: BooleanArray,
) {
    /** Whether the module at [x], [y] is dark. Out-of-range coordinates read light. */
    fun isDark(
        x: Int,
        y: Int,
    ): Boolean = x in 0 until size && y in 0 until size && modules[y * size + x]

    companion object {
        /**
         * Encodes [text] in byte mode at error-correction level M.
         *
         * Returns `null` when the text does not fit version 10 — the honest answer for a
         * caller that must then not draw a symbol, rather than a truncated code that scans
         * to the wrong URL.
         */
        fun encode(text: String): QrCode? {
            val payload = text.toByteArray(Charsets.UTF_8)
            val spec = QrVersionSpec.smallestFor(payload.size) ?: return null
            val codewords = interleave(spec, payload)
            return QrSymbol(spec).build(codewords)
        }

        /**
         * Splits the payload into blocks, appends each block's error correction, and
         * interleaves the result.
         *
         * Interleaving is what makes the code survive a thumb over one corner: a burst of
         * damage lands a few codewords in each block rather than destroying one block
         * outright, and every block can then be repaired within its own correction budget.
         */
        private fun interleave(
            spec: QrVersionSpec,
            payload: ByteArray,
        ): IntArray {
            val buffer = QrBitBuffer()
            buffer.append(BYTE_MODE, QrVersionSpec.MODE_BITS)
            buffer.append(payload.size, spec.characterCountBits)
            payload.forEach { buffer.appendByte(it.toInt() and 0xFF) }
            val data = buffer.toCodewords(spec.dataCodewords)

            val blocks = ArrayList<IntArray>(spec.blockCount)
            val correction = ArrayList<IntArray>(spec.blockCount)
            var offset = 0
            for (index in 0 until spec.blockCount) {
                val length = if (index < spec.shortBlocks) spec.shortBlockData else spec.longBlockData
                val block = data.copyOfRange(offset, offset + length)
                offset += length
                blocks += block
                correction += QrGaloisField.errorCorrection(block, spec.ecPerBlock)
            }

            val result = IntArray(spec.totalCodewords)
            var cursor = 0
            for (position in 0 until spec.longBlockData) {
                for (block in blocks) {
                    if (position < block.size) result[cursor++] = block[position]
                }
            }
            for (position in 0 until spec.ecPerBlock) {
                for (block in correction) result[cursor++] = block[position]
            }
            return result
        }

        /** Byte mode's indicator value. */
        private const val BYTE_MODE = 0b0100
    }
}
