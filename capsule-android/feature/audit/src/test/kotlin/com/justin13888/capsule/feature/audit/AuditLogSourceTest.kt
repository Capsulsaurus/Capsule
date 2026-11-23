package com.justin13888.capsule.feature.audit

import com.justin13888.capsule.core.data.QuotaError
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.LifecycleAction
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

/**
 * What the log actually says, read out of the seeded library rather than out of a stub.
 *
 * The point of driving the real fakes here is that the assertions are about the *evidence*:
 * that a delete's create record survives beside it, that an operator's action lands in the
 * same list as the user's own writes, and that a partial reading admits to being partial.
 * A hand-rolled double would let all three pass while proving none of them.
 */
class AuditLogSourceTest {
    @Test
    fun `the log merges the user's own writes with operator actions, newest first`() =
        runTest {
            val log = sourceOver(repositories()).snapshots().first().log

            val categories = log.entries.map(AuditEntry::category).toSet()
            assertEquals(AuditCategory.entries.toSet(), categories, "both halves of the log must be present")
            assertEquals(
                log.entries.map(AuditEntry::at).sortedDescending(),
                log.entries.map(AuditEntry::at),
                "the log is ordered newest first",
            )
        }

    @Test
    fun `a delete keeps the create record beside it, so the history stays legible`() =
        runTest {
            val log = sourceOver(repositories()).snapshots().first().log

            val byAsset =
                log.entries
                    .filterIsInstance<AuditEntry.Lifecycle>()
                    .groupBy { it.record.assetId }
            val deleted = byAsset.values.firstOrNull { chain -> chain.any { it.action == LifecycleAction.Delete } }
            assertTrue(deleted != null, "the seeded library holds at least one trashed asset")
            val actions = deleted.orEmpty().map(AuditEntry.Lifecycle::action)
            assertTrue(LifecycleAction.Create in actions, "the delete's create record is never removed")
            assertTrue(LifecycleAction.Delete in actions, "the delete record itself is listed")
        }

    @Test
    fun `every lifecycle entry carries the record behind it, not a summary of one`() =
        runTest {
            val log = sourceOver(repositories()).snapshots().first().log

            val lifecycle = log.entries.filterIsInstance<AuditEntry.Lifecycle>()
            assertTrue(lifecycle.isNotEmpty(), "the fixture produces lifecycle entries")
            assertTrue(
                lifecycle.all { it.record.recordHash.isNotBlank() },
                "a record with no hash would be an entry the user cannot check",
            )
            assertTrue(
                lifecycle.filter { it.action != LifecycleAction.Create }.all { it.record.priorProvenanceHash != null },
                "only a create has no prior link",
            )
        }

    @Test
    fun `a library with nothing in it produces an empty log rather than a failure`() =
        runTest {
            val log = sourceOver(repositories(MockScenario.FirstRun)).snapshots().first().log

            assertEquals(emptyList<AuditEntry>(), log.entries)
            assertFalse(log.truncated, "there is no history beyond an empty library")
        }

    @Test
    fun `a reading that fills its page admits the history is partial`() =
        runTest {
            val log = sourceOver(repositories(), scanLimit = SMALL_PAGE).snapshots().first().log

            assertTrue(log.truncated, "a full page says nothing about what lies past it")
            assertEquals(SMALL_PAGE, log.scannedAssets)
        }

    @Test
    fun `a repository refusal reaches the caller rather than being reported as an empty log`() =
        runTest {
            val scenario = MockScenario.Default.failing(MockOperation.TimelineRead, QuotaError.Exceeded)

            val thrown =
                assertThrows<QuotaError.Exceeded>(
                    "an empty log and a refused read must never look the same",
                ) { sourceOver(repositories(scenario)).snapshots().first() }
            assertEquals(QuotaError.Exceeded.code, thrown.code)
        }

    private fun repositories(scenario: MockScenario = MockScenario.Default): MockRepositories =
        AuditFixtures.repositories(
            scenario,
        )

    private fun sourceOver(
        mock: MockRepositories,
        scanLimit: Int = DEFAULT_PAGE,
    ): AuditLogSource =
        AuditLogSource(
            timeline = mock.timeline,
            assets = mock.assets,
            moderation = mock.moderation,
            sessions = mock.sessions,
            scanLimit = scanLimit,
        )

    private companion object {
        const val DEFAULT_PAGE = 60
        const val SMALL_PAGE = 4
    }
}
