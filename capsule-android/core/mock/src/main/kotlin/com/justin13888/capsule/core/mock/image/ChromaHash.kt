package com.justin13888.capsule.core.mock.image

import com.justin13888.capsule.core.model.RgbColor
import java.util.Locale

/**
 * A 3×3 grid of chroma samples — the payload behind
 * [com.justin13888.capsule.core.model.Lqip.chromahash].
 *
 * Nine cells is the smallest grid that still carries a picture's *composition*: sky over
 * ground, a bright subject against a dark surround, a horizon a third of the way up. A single
 * average colour loses all of that and makes every placeholder in the grid look the same,
 * which is exactly the loading state the design is trying to avoid.
 */
data class ChromaGrid(
    /** Nine cells in row-major order, top-left first. */
    val cells: List<RgbColor>,
) {
    init {
        require(cells.size == CELL_COUNT) { "a chroma grid holds exactly $CELL_COUNT cells" }
    }

    /** The cell at [column], [row], both `0..2`. */
    fun cell(
        column: Int,
        row: Int,
    ): RgbColor = cells[row * SIDE + column]

    /** The channel-wise mean of every cell — the fallback fill for an undecodable payload. */
    fun mean(): RgbColor =
        RgbColor(
            red = cells.sumOf(RgbColor::red) / CELL_COUNT,
            green = cells.sumOf(RgbColor::green) / CELL_COUNT,
            blue = cells.sumOf(RgbColor::blue) / CELL_COUNT,
        )

    companion object {
        /** Cells along one edge. */
        const val SIDE = 3

        /** Cells in the whole grid. */
        const val CELL_COUNT = SIDE * SIDE
    }
}

/**
 * The wire form of a [ChromaGrid]: a version nibble followed by one hex nibble per channel
 * per cell.
 *
 * Four bits a channel is not a shortcut — it is the point. The placeholder is stretched over
 * a whole tile and then blurred, so a channel resolution finer than sixteen steps is
 * invisible, while the payload has to ride inside the encrypted metadata blob that syncs
 * before any derivative exists. Twenty-eight characters is small enough to be free there.
 *
 * Decoding is total: a payload this build does not recognise returns `null` rather than
 * throwing, so the renderer falls back to the sidecar's dominant colour exactly as
 * [com.justin13888.capsule.core.model.Lqip] documents.
 */
object ChromaHash {
    /** The encoding version this build writes. */
    const val CURRENT_VERSION = 1

    /** Encodes [grid] under [version]. */
    fun encode(
        grid: ChromaGrid,
        version: Int = CURRENT_VERSION,
    ): String {
        val builder = StringBuilder(1 + ChromaGrid.CELL_COUNT * CHANNELS)
        builder.append(String.format(Locale.ROOT, "%x", version and NIBBLE_MASK))
        grid.cells.forEach { cell ->
            builder.append(nibble(cell.red))
            builder.append(nibble(cell.green))
            builder.append(nibble(cell.blue))
        }
        return builder.toString()
    }

    /**
     * Decodes a payload, or returns `null` when it is the wrong length, the wrong version, or
     * not hex at all.
     *
     * Every rejection is a `null` rather than an exception because the caller's response to
     * all three is identical — fill with the dominant colour — and an exception would only
     * force that decision to be written twice.
     */
    fun decode(payload: String): ChromaGrid? {
        if (payload.length != 1 + ChromaGrid.CELL_COUNT * CHANNELS) return null
        if (payload[0].digitToIntOrNull(HEX_RADIX) != CURRENT_VERSION) return null
        val nibbles = payload.drop(1).map { it.digitToIntOrNull(HEX_RADIX) }
        if (nibbles.any { it == null }) return null
        val cells =
            (0 until ChromaGrid.CELL_COUNT).map { cell ->
                val base = cell * CHANNELS
                RgbColor(
                    red = expand(nibbles[base] ?: 0),
                    green = expand(nibbles[base + 1] ?: 0),
                    blue = expand(nibbles[base + 2] ?: 0),
                )
            }
        return ChromaGrid(cells)
    }

    /** The `0..255` channel [value] as one hex nibble. */
    private fun nibble(value: Int): Char = (value.coerceIn(0, CHANNEL_MAX) shr NIBBLE_SHIFT).digitToChar(HEX_RADIX)

    /** A nibble back to `0..255`, replicating the nibble so `0xF` reaches a true 255. */
    private fun expand(nibble: Int): Int = (nibble shl NIBBLE_SHIFT) or nibble

    private const val CHANNELS = 3
    private const val NIBBLE_MASK = 0xF
    private const val NIBBLE_SHIFT = 4
    private const val CHANNEL_MAX = 255
    private const val HEX_RADIX = 16
}
