package com.justin13888.capsule.feature.importer

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportCandidate
import com.justin13888.capsule.core.model.ImportDecision
import com.justin13888.capsule.core.model.ImportFileState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The per-file ladder, which is the only thing that makes a run legible while it is running.
 *
 * Pure by construction, so every property the streaming design rests on — the window bound,
 * the pause, the halt, the cancel — is asserted here without a clock, a repository or a
 * composition anywhere near it.
 */
class ImportProgressionTest {
    @Test
    fun `a file walks the whole ladder in order`() {
        var progression = ImportProgression.of(candidates(1), windowSize = 1)
        val seen = mutableListOf(progression.rows.single().state)
        repeat(5) {
            progression = progression.advance()
            seen += progression.rows.single().state
        }
        assertEquals(
            listOf(
                ImportFileState.Queued,
                ImportFileState.Processing,
                ImportFileState.Encrypting,
                ImportFileState.Uploading,
                ImportFileState.Done,
                ImportFileState.Done,
            ),
            seen,
        )
    }

    @Test
    fun `only the window is ever materialized`() {
        var progression = ImportProgression.of(candidates(10), windowSize = ImportProgression.STREAMING_WINDOW)
        repeat(3) { progression = progression.advance() }

        assertEquals(ImportProgression.STREAMING_WINDOW, progression.inFlight.size)
        val untouched = progression.rows.drop(ImportProgression.STREAMING_WINDOW)
        assertTrue(untouched.all { it.state == ImportFileState.Queued })
    }

    @Test
    fun `the window slides as files finish`() {
        var progression = ImportProgression.of(candidates(6), windowSize = 2)
        repeat(4) { progression = progression.advance() }

        assertEquals(2, progression.completed)
        assertEquals(listOf("f0", "f1"), progression.rows.take(2).map(ImportFileRow::displayName))
        assertTrue(progression.rows.take(2).all { it.state == ImportFileState.Done })
        assertTrue(progression.inFlight.none { it.isTerminal })
    }

    @Test
    fun `a paused run admits nothing and advances nothing`() {
        val started = ImportProgression.of(candidates(4), windowSize = 2).advance()
        val paused = started.paused(true)

        assertEquals(started.rows, paused.advance().rows)
        assertTrue(paused.isPaused)
        assertFalse(paused.isFinished)
    }

    @Test
    fun `a halt fails only what was in flight`() {
        val running = ImportProgression.of(candidates(5), windowSize = 2).advance()
        val halted = running.halt(ErrorCode("error.quota.exceeded"))

        assertEquals(2, halted.failed)
        assertEquals(3, halted.rows.count { it.state == ImportFileState.Queued })
        assertEquals(ErrorCode("error.quota.exceeded"), halted.haltCode)
        assertEquals(halted.rows, halted.advance().rows)
    }

    @Test
    fun `cancelling keeps what finished and drops what never started`() {
        var progression = ImportProgression.of(candidates(5), windowSize = 1)
        repeat(4) { progression = progression.advance() }
        val cancelled = progression.cancelled()

        assertEquals(1, cancelled.total)
        assertEquals(1, cancelled.imported)
        assertTrue(cancelled.isFinished)
    }

    @Test
    fun `an empty run is finished and complete`() {
        val progression = ImportProgression.of(emptyList())

        assertTrue(progression.isFinished)
        assertEquals(1f, progression.fraction, 0f)
    }

    private fun candidates(count: Int): List<ImportCandidate> =
        (0 until count).map { index ->
            ImportCandidate(
                sourceLocator = "DCIM/Camera/f$index",
                sizeBytes = 1_024L * (index + 1),
                decision = ImportDecision.Import(AlbumId("album")),
            )
        }
}
