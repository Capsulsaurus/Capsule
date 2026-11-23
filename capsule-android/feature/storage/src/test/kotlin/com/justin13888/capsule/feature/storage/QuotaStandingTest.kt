package com.justin13888.capsule.feature.storage

import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.QuotaState
import com.justin13888.capsule.core.model.QuotaStatus
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Duration

/**
 * The ladder, and the advice that hangs off each rung.
 *
 * Driven from the named scenario presets rather than from hand-built quota numbers: the
 * presets are the app's documented states, so a change to what "grace expired" means shows up
 * here rather than in a fixture nobody re-reads.
 */
class QuotaStandingTest {
    @Test
    fun `a healthy account is asked for nothing`() =
        runTest {
            val standing = standingFor(MockScenario.Default)

            assertEquals(QuotaState.Ok, standing.state)
            assertEquals(QuotaRemediation.None, standing.remediation)
            assertTrue(standing.allowsUpload)
            assertTrue(standing.allowsMetadataGrowth)
        }

    @Test
    fun `the soft warning still uploads and points at what is large`() =
        runTest {
            val standing = standingFor(MockScenario.QuotaSoftWarning)

            assertEquals(QuotaState.SoftWarning, standing.state)
            assertEquals(QuotaRemediation.ReviewLargest, standing.remediation)
            assertTrue(standing.allowsUpload, "the soft limit warns; it does not refuse")
        }

    @Test
    fun `a full account refuses uploads and still admits edits`() =
        runTest {
            val standing = standingFor(MockScenario.QuotaExhausted)

            assertEquals(QuotaState.HardExceeded, standing.state)
            assertEquals(QuotaRemediation.FreeSpace, standing.remediation)
            assertFalse(standing.allowsUpload)
            assertTrue(standing.allowsMetadataGrowth, "metadata edits survive the hard limit")
        }

    @Test
    fun `the hard limit runs a grace countdown the screen can show`() =
        runTest {
            val standing = standingFor(MockScenario.QuotaExhausted)

            val remaining = standing.graceRemaining
            assertNotNull(remaining, "a running window has a remainder to show")
            assertTrue(remaining != null && remaining > Duration.ZERO)
            assertTrue(
                remaining != null && remaining < QuotaStatus.DEFAULT_GRACE_WINDOW,
                "a day of the window has already elapsed",
            )
        }

    @Test
    fun `grace expiry pauses edits but is never a dead end`() =
        runTest {
            val standing = standingFor(MockScenario.QuotaGraceExpired)

            assertEquals(QuotaState.GraceExpired, standing.state)
            assertEquals(QuotaRemediation.FreeSpaceToResumeEdits, standing.remediation)
            assertFalse(standing.allowsMetadataGrowth)
            assertTrue(
                standing.allowsRecoveryWrites,
                "a user must be able to delete their way back under quota",
            )
            assertNull(standing.graceRemaining, "an expired window has nothing left to count")
        }

    @Test
    fun `a suspension is the one state the user cannot resolve alone`() =
        runTest {
            val standing = standingFor(MockScenario.AccountSuspended)

            assertEquals(QuotaState.Suspended, standing.state)
            assertEquals(QuotaRemediation.ContactSupport, standing.remediation)
            assertFalse(standing.allowsRecoveryWrites, "only an administrative state closes this door")
        }

    @Test
    fun `an unlimited deployment reads as usage rather than as a nearly-empty bar`() {
        val status = QuotaStatus(used = 10L, softLimit = 100L, hardLimit = null)

        val standing = QuotaStanding.of(status, StorageFixtures.NOW)

        assertNull(standing.hardLimit)
        assertEquals(QuotaState.Ok, standing.state)
    }

    @Test
    fun `the fraction is clamped so a full account never overruns its own bar`() {
        val status = QuotaStatus(used = 500L, softLimit = 100L, hardLimit = 200L)

        val standing = QuotaStanding.of(status, StorageFixtures.NOW)

        assertEquals(1f, standing.fraction)
    }

    private suspend fun standingFor(scenario: MockScenario): QuotaStanding {
        val repositories = StorageFixtures.repositories(scenario)
        return QuotaStanding.of(repositories.storage.quota().first(), StorageFixtures.NOW)
    }
}
