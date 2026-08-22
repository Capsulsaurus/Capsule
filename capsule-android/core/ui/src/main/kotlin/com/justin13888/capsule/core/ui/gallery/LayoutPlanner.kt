package com.justin13888.capsule.core.ui.gallery

/**
 * Solves a whole library into a [LayoutPlan].
 *
 * Sections are solved independently — a section is a day bucket, and a row never spans a
 * date boundary because each date carries its own header. That independence is what keeps
 * the cost linear: solving is `O(assets × RowSolver.MAX_ROW_RUN)` overall, a few
 * milliseconds for a million assets, so the planner runs eagerly on a background
 * dispatcher rather than estimating heights and correcting them during a scroll.
 *
 * The planner is pure. Given the same aspects, section boundaries and [GalleryMetrics] it
 * produces the same plan, which is what lets the gallery cache plans by those inputs and
 * lets tests compare against fixtures.
 */
object LayoutPlanner {
    /**
     * Solve a plan.
     *
     * @param aspects aspect ratio (width / height) per asset, in display order.
     * @param sectionStarts index of the first asset of each section, strictly ascending,
     *   starting at 0. A section is normally one day of captures.
     * @param metrics the pixel measurements to solve against.
     */
    fun plan(
        aspects: FloatArray,
        sectionStarts: IntArray,
        metrics: GalleryMetrics,
    ): LayoutPlan {
        requireValidSections(aspects.size, sectionStarts)

        val rowFrom = ArrayList<Int>()
        val rowUntil = ArrayList<Int>()
        val rowHeight = ArrayList<Float>()
        val slotY = ArrayList<Float>()
        val slotHeight = ArrayList<Float>()
        val slotSection = ArrayList<Int>()
        val slotRow = ArrayList<Int>()
        val rowSlot = ArrayList<Int>()

        var y = 0f
        for (section in sectionStarts.indices) {
            val start = sectionStarts[section]
            val end = if (section + 1 < sectionStarts.size) sectionStarts[section + 1] else aspects.size

            if (section > 0) y += metrics.sectionSpacing

            slotY += y
            slotHeight += metrics.headerHeight
            slotSection += section
            slotRow += -1
            y += metrics.headerHeight

            val rows =
                RowSolver.solve(
                    aspects = aspects,
                    from = start,
                    until = end,
                    targetHeight = metrics.targetRowHeight,
                    viewportWidth = metrics.viewportWidth,
                    spacing = metrics.spacing,
                )

            rows.forEachIndexed { indexInSection, row ->
                if (indexInSection > 0) y += metrics.spacing
                slotY += y
                slotHeight += row.height
                slotSection += -1
                slotRow += rowFrom.size
                rowSlot += slotY.size - 1
                rowFrom += row.from
                rowUntil += row.until
                rowHeight += row.height
                y += row.height
            }
        }

        return LayoutPlan(
            metrics = metrics,
            aspects = aspects,
            slots =
                LayoutPlan.SlotTable(
                    y = slotY.toFloatArray(),
                    height = slotHeight.toFloatArray(),
                    section = slotSection.toIntArray(),
                    row = slotRow.toIntArray(),
                ),
            rows =
                LayoutPlan.RowTable(
                    from = rowFrom.toIntArray(),
                    until = rowUntil.toIntArray(),
                    height = rowHeight.toFloatArray(),
                    slot = rowSlot.toIntArray(),
                ),
            sectionStart = sectionStarts.copyOf(),
            totalHeight = y,
        )
    }

    private fun requireValidSections(
        assetCount: Int,
        sectionStarts: IntArray,
    ) {
        require(sectionStarts.isNotEmpty()) { "a plan needs at least one section" }
        require(sectionStarts.first() == 0) { "sections must start at asset 0, got ${sectionStarts.first()}" }
        require(sectionStarts.last() <= assetCount) {
            "section start ${sectionStarts.last()} is past the $assetCount assets available"
        }
        for (i in 1 until sectionStarts.size) {
            require(sectionStarts[i] > sectionStarts[i - 1]) {
                "section starts must strictly ascend; ${sectionStarts[i - 1]} then ${sectionStarts[i]}"
            }
        }
    }
}
