package com.justin13888.capsule.core.ui.gallery

/**
 * A fully solved gallery layout: where every section header and every justified row sits,
 * for one exact combination of [GalleryMetrics] and library contents.
 *
 * The plan is **exact, not estimated**. Everything it needs — one byte of aspect ratio
 * per asset and the section boundaries — is cheap enough to hold for an entire library,
 * so there is no need to guess at the height of content the user has not scrolled to and
 * then correct it later. That is what makes the scrollbar honest, the date scrubber land
 * on the right day, and pinch-zoom keep the asset under the finger: all three are just
 * lookups into the arrays below.
 *
 * Stored as parallel primitive arrays rather than a list of objects. A million-asset
 * library is a few hundred thousand rows, and one object per row would be both a
 * significant allocation and a scroll-time pointer chase.
 */
class LayoutPlan internal constructor(
    val metrics: GalleryMetrics,
    /** Aspect ratio per asset, indexed globally; shared with the caller, never mutated. */
    private val aspects: FloatArray,
    private val slots: SlotTable,
    private val rows: RowTable,
    /** First asset of each section, inclusive; `sectionStart.size` is the section count. */
    private val sectionStart: IntArray,
    /** Total content height in pixels. */
    val totalHeight: Float,
) {
    /**
     * Where each slot — a section header or a justified row — sits vertically.
     *
     * @param y top of each slot in content pixels, ascending.
     * @param height height of each slot.
     * @param section section index for a header slot, or -1 when the slot is a row.
     * @param row index into [RowTable] for a row slot, or -1 when the slot is a header.
     */
    internal class SlotTable(
        val y: FloatArray,
        val height: FloatArray,
        val section: IntArray,
        val row: IntArray,
    )

    /**
     * The justified rows, and the slot each one occupies.
     *
     * @param from first asset of each row, inclusive.
     * @param until one past the last asset of each row.
     * @param height common draw height of each row.
     * @param slot slot index of each row — the inverse of [SlotTable.row].
     */
    internal class RowTable(
        val from: IntArray,
        val until: IntArray,
        val height: FloatArray,
        val slot: IntArray,
    )

    /** How many slots — headers plus rows — the plan holds. */
    val slotCount: Int get() = slots.y.size

    /** How many sections the plan covers. */
    val sectionCount: Int get() = sectionStart.size

    /** How many assets the plan covers. */
    val assetCount: Int get() = aspects.size

    /** True when slot [slot] is a section header rather than a row of assets. */
    fun isHeader(slot: Int): Boolean = slots.section[slot] >= 0

    /** The section a header slot belongs to. Only valid when [isHeader] is true. */
    fun sectionOf(slot: Int): Int = slots.section[slot]

    /** The top of slot [slot] in content pixels. */
    fun offsetOf(slot: Int): Float = slots.y[slot]

    /** The height of slot [slot] in pixels. */
    fun heightOf(slot: Int): Float = slots.height[slot]

    /** The first asset of a row slot, inclusive. Only valid when [isHeader] is false. */
    fun rowFrom(slot: Int): Int = rows.from[slots.row[slot]]

    /** One past the last asset of a row slot. Only valid when [isHeader] is false. */
    fun rowUntil(slot: Int): Int = rows.until[slots.row[slot]]

    /** The draw height of a row slot. Only valid when [isHeader] is false. */
    fun rowHeight(slot: Int): Float = rows.height[slots.row[slot]]

    /** The first asset of section [section], inclusive. */
    fun sectionStart(section: Int): Int = sectionStart[section]

    /** One past the last asset of section [section]. */
    fun sectionEnd(section: Int): Int = if (section + 1 < sectionStart.size) sectionStart[section + 1] else aspects.size

    /** How many assets section [section] holds. */
    fun sectionSize(section: Int): Int = sectionEnd(section) - sectionStart(section)

    /**
     * The width asset [asset] is drawn at inside the row slot [slot], and its horizontal
     * offset from the row's left edge. Callers lay a row out left to right.
     */
    fun assetWidth(
        slot: Int,
        asset: Int,
    ): Float = aspects[asset] * rowHeight(slot)

    /** The horizontal offset of [asset] within its row slot [slot]. */
    fun assetX(
        slot: Int,
        asset: Int,
    ): Float {
        var x = 0f
        for (i in rowFrom(slot) until asset) {
            x += aspects[i] * rowHeight(slot) + metrics.spacing
        }
        return x
    }

    /**
     * The last slot whose top is at or above [y].
     *
     * This is the entry point for scrolling: the renderer binary-searches once per frame
     * and composes forward from the result, so the cost of scrolling does not grow with
     * the size of the library.
     */
    fun slotAt(y: Float): Int {
        if (slots.y.isEmpty()) return -1
        var low = 0
        var high = slots.y.size - 1
        while (low < high) {
            val mid = (low + high + 1) ushr 1
            if (slots.y[mid] <= y) low = mid else high = mid - 1
        }
        return low
    }

    /** The slot holding asset [asset], or -1 if the plan does not cover it. */
    fun slotOfAsset(asset: Int): Int {
        if (asset !in 0 until aspects.size) return -1
        var low = 0
        var high = rows.from.size - 1
        while (low < high) {
            val mid = (low + high + 1) ushr 1
            if (rows.from[mid] <= asset) low = mid else high = mid - 1
        }
        return rows.slot[low]
    }

    /** The section holding asset [asset], or -1 if the plan does not cover it. */
    fun sectionOfAsset(asset: Int): Int {
        if (asset !in 0 until aspects.size) return -1
        var low = 0
        var high = sectionStart.size - 1
        while (low < high) {
            val mid = (low + high + 1) ushr 1
            if (sectionStart[mid] <= asset) low = mid else high = mid - 1
        }
        return low
    }

    /**
     * The asset at content position ([x], [y]), or -1 when the point lands on a header,
     * in the gap between two assets, or past the end of a row.
     *
     * Drag-to-select hit-tests against this rather than against composed nodes, so a drag
     * that outruns composition still selects the right range.
     */
    fun assetAt(
        x: Float,
        y: Float,
    ): Int {
        val slot = slotAt(y)
        if (slot < 0 || isHeader(slot)) return -1
        if (y > slots.y[slot] + slots.height[slot]) return -1
        val height = rowHeight(slot)
        var cursor = 0f
        for (asset in rowFrom(slot) until rowUntil(slot)) {
            val width = aspects[asset] * height
            if (x >= cursor && x <= cursor + width) return asset
            cursor += width + metrics.spacing
        }
        return -1
    }

    /**
     * Scroll offset that puts asset [asset] at the top of the viewport, clamped so the
     * caller never scrolls past the end. Used to restore position across a re-plan.
     */
    fun offsetOfAsset(
        asset: Int,
        viewportHeight: Float,
    ): Float {
        val slot = slotOfAsset(asset)
        if (slot < 0) return 0f
        return slots.y[slot].coerceIn(0f, (totalHeight - viewportHeight).coerceAtLeast(0f))
    }
}
