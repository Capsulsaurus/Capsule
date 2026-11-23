package com.justin13888.capsule.feature.sharing.qr

import kotlin.math.abs

/**
 * The eight data masks and the penalty score that chooses between them.
 *
 * Masking exists because an unmasked payload can produce large blank areas or accidental
 * copies of the finder pattern, either of which a camera reads as a different symbol —
 * or as no symbol at all. The penalty rules are the standard's, and their weights are not
 * tunable: a decoder assumes the encoder minimised exactly this score.
 */
internal object QrMask {
    /** How many masks the standard defines. */
    const val COUNT = 8

    /**
     * Exclusive-ors mask [pattern] into every module no function pattern claimed.
     *
     * Applying twice restores the original grid, which is what lets the chooser try all
     * eight against one buffer instead of copying the symbol eight times.
     */
    fun apply(
        pattern: Int,
        size: Int,
        modules: BooleanArray,
        functions: BooleanArray,
    ) {
        for (y in 0 until size) {
            for (x in 0 until size) {
                val index = y * size + x
                if (functions[index]) continue
                if (inverts(pattern, x, y)) modules[index] = !modules[index]
            }
        }
    }

    /** The standard's total penalty for a masked grid. Lower is easier to scan. */
    fun penalty(
        size: Int,
        modules: BooleanArray,
    ): Int =
        runPenalty(size, modules) +
            blockPenalty(size, modules) +
            finderLookalikePenalty(size, modules) +
            balancePenalty(size, modules)

    private fun inverts(
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

    /** Rule 1: five or more same-coloured modules in a row or column. */
    private fun runPenalty(
        size: Int,
        modules: BooleanArray,
    ): Int {
        var total = 0
        for (line in 0 until size) {
            total += lineRunPenalty(size) { index -> modules[line * size + index] }
            total += lineRunPenalty(size) { index -> modules[index * size + line] }
        }
        return total
    }

    private inline fun lineRunPenalty(
        size: Int,
        module: (Int) -> Boolean,
    ): Int {
        var total = 0
        var run = 1
        for (index in 1 until size) {
            if (module(index) == module(index - 1)) {
                run += 1
                if (run == RUN_THRESHOLD) {
                    total += RUN_BASE
                } else if (run > RUN_THRESHOLD) {
                    total += 1
                }
            } else {
                run = 1
            }
        }
        return total
    }

    /** Rule 2: every two-by-two block of one colour. */
    private fun blockPenalty(
        size: Int,
        modules: BooleanArray,
    ): Int {
        var total = 0
        for (y in 0 until size - 1) {
            for (x in 0 until size - 1) {
                val first = modules[y * size + x]
                val uniform =
                    first == modules[y * size + x + 1] &&
                        first == modules[(y + 1) * size + x] &&
                        first == modules[(y + 1) * size + x + 1]
                if (uniform) total += BLOCK_PENALTY
            }
        }
        return total
    }

    /** Rule 3: a run that looks like a finder pattern, in either direction. */
    private fun finderLookalikePenalty(
        size: Int,
        modules: BooleanArray,
    ): Int {
        var total = 0
        for (line in 0 until size) {
            total += lineFinderPenalty(size) { index -> modules[line * size + index] }
            total += lineFinderPenalty(size) { index -> modules[index * size + line] }
        }
        return total
    }

    private inline fun lineFinderPenalty(
        size: Int,
        module: (Int) -> Boolean,
    ): Int {
        var total = 0
        var window = 0
        for (index in 0 until size) {
            window = ((window shl 1) or if (module(index)) 1 else 0) and WINDOW_MASK
            if (index >= LOOKALIKE_WIDTH - 1 && (window == LOOKALIKE_A || window == LOOKALIKE_B)) {
                total += LOOKALIKE_PENALTY
            }
        }
        return total
    }

    /** Rule 4: how far the proportion of dark modules strays from half. */
    private fun balancePenalty(
        size: Int,
        modules: BooleanArray,
    ): Int {
        val dark = modules.count { it }
        val total = size * size
        val deviation = abs(dark * 100 - total * 50) / total
        return deviation / BALANCE_STEP * BALANCE_PENALTY
    }

    /** Where rule 1 starts charging. */
    private const val RUN_THRESHOLD = 5

    /** What a run of exactly [RUN_THRESHOLD] costs; each further module adds one. */
    private const val RUN_BASE = 3

    /** What one uniform two-by-two block costs. */
    private const val BLOCK_PENALTY = 3

    /** How many modules the finder-lookalike window spans. */
    private const val LOOKALIKE_WIDTH = 11

    /** The window's bit mask. */
    private const val WINDOW_MASK = 0b111_1111_1111

    /** Dark-light-dark-dark-dark-light-dark followed by four light. */
    private const val LOOKALIKE_A = 0b101_1101_0000

    /** The same run reversed. */
    private const val LOOKALIKE_B = 0b000_0101_1101

    /** What one lookalike costs. */
    private const val LOOKALIKE_PENALTY = 40

    /** Rule 4 charges once per this many percentage points away from half. */
    private const val BALANCE_STEP = 5

    /** What each of those steps costs. */
    private const val BALANCE_PENALTY = 10
}
