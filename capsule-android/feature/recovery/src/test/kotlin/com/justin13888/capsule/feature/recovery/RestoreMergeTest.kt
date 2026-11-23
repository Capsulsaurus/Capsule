package com.justin13888.capsule.feature.recovery

import com.justin13888.capsule.core.model.RestoreConflictKind
import com.justin13888.capsule.core.model.knownOrNull
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The rule that decides whether a restore can destroy anything.
 *
 * Tested as a pure function rather than through the screen, because it is the property the
 * whole flow exists to guarantee and it must hold whatever the screen happens to render:
 * **only an explicit choice of the archived copy rolls anything back.**
 */
class RestoreMergeTest {
    @Test
    fun `an untouched screen rolls nothing back`() {
        val plan = RestoreMerge.plan(CONFLICTS, emptyMap())

        assertTrue(plan.rolledBack.isEmpty(), "silence is not consent")
        assertEquals(CONFLICTS.map { it.assetId }, plan.keptLocal)
        assertFalse(plan.changesAnything)
    }

    @Test
    fun `deciding to keep is the same as not deciding`() {
        val decided =
            RestoreMerge.plan(
                CONFLICTS,
                CONFLICTS.associate { it.assetId.value to RestoreDecision.KeepLocal },
            )

        assertEquals(RestoreMerge.plan(CONFLICTS, emptyMap()), decided)
    }

    @Test
    fun `only an explicit choice of the archived copy rolls one back`() {
        val plan =
            RestoreMerge.plan(
                CONFLICTS,
                mapOf(RecoveryFixtures.NEWER_LOCALLY.assetId.value to RestoreDecision.UseBackup),
            )

        assertEquals(listOf(RecoveryFixtures.NEWER_LOCALLY.assetId), plan.rolledBack)
        assertEquals(listOf(RecoveryFixtures.DIVERGENT.assetId), plan.keptLocal)
        assertTrue(plan.changesAnything)
    }

    @Test
    fun `an asset deleted after the export stays deleted unless the user says otherwise`() {
        // The divergent case is what a locally-tombstoned asset looks like on the ladder, so
        // this is the six-month-old-backup-resurrects-a-deleted-photo case in miniature.
        assertEquals(RestoreConflictKind.Divergent, RecoveryFixtures.DIVERGENT.kind.knownOrNull)

        val plan = RestoreMerge.plan(CONFLICTS, mapOf())

        assertFalse(RecoveryFixtures.DIVERGENT.assetId in plan.rolledBack)
    }

    @Test
    fun `undecided conflicts are counted so the screen can say how many are left`() {
        assertEquals(2, RestoreMerge.undecidedCount(CONFLICTS, emptyMap()))
        assertEquals(
            1,
            RestoreMerge.undecidedCount(
                CONFLICTS,
                mapOf(RecoveryFixtures.DIVERGENT.assetId.value to RestoreDecision.UseBackup),
            ),
        )
    }

    @Test
    fun `a decision about an asset that is not in conflict changes nothing`() {
        val plan =
            RestoreMerge.plan(
                CONFLICTS,
                mapOf("0197a0d0-0000-7000-8000-00000000ffff" to RestoreDecision.UseBackup),
            )

        assertTrue(plan.rolledBack.isEmpty())
    }

    private companion object {
        val CONFLICTS = listOf(RecoveryFixtures.NEWER_LOCALLY, RecoveryFixtures.DIVERGENT)
    }
}
