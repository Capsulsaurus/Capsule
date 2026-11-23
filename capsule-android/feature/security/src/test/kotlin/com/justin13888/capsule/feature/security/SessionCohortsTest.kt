package com.justin13888.capsule.feature.security

import com.justin13888.capsule.core.mock.MockScenario
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The grouping that makes a session ledger readable.
 *
 * Asserted as a pure function, because that is what it is: the cohort hash is client-asserted
 * and unverifiable, so the only thing it ever does is decide what a row says.
 */
class SessionCohortsTest {
    @Test
    fun `sessions from one device land under one heading`() {
        val groups = SessionCohorts.group(SecurityFixtures.ledger(), SecurityFixtures.cohorts())

        val returning = groups.first { it.cohortHash == SecurityFixtures.RETURNING_COHORT }
        assertEquals(2, returning.sessionCount, "a reinstall adds a session, not a device")
    }

    @Test
    fun `this device sorts first because it is the row the user checks themselves against`() {
        val groups = SessionCohorts.group(SecurityFixtures.ledger(), SecurityFixtures.cohorts())

        assertTrue(groups.first().isCurrent)
        assertEquals(SecurityFixtures.CURRENT_COHORT, groups.first().cohortHash)
    }

    @Test
    fun `sessions that asserted no cohort group together and sort last`() {
        val groups = SessionCohorts.group(SecurityFixtures.ledger(), SecurityFixtures.cohorts())

        assertEquals(null, groups.last().cohortHash)
        assertEquals(1, groups.last().sessionCount)
    }

    @Test
    fun `a grouped cohort that is not this device may be asserted as seen before`() {
        val groups = SessionCohorts.group(SecurityFixtures.ledger(), SecurityFixtures.cohorts())

        val returning = groups.first { it.cohortHash == SecurityFixtures.RETURNING_COHORT }
        assertTrue(returning.seenBefore, "the durable map's row is the whole of the claim")
        assertFalse(groups.first().seenBefore, "this device is named, not described as familiar")
        assertFalse(groups.last().seenBefore, "an ungrouped session claims nothing at all")
    }

    @Test
    fun `a cohort with no durable row still groups and dates itself from its sessions`() {
        val groups = SessionCohorts.group(SecurityFixtures.ledger(), cohorts = emptyList())

        val returning = groups.first { it.cohortHash == SecurityFixtures.RETURNING_COHORT }
        assertEquals(2, returning.sessionCount, "a missing map row is not a reason to split a device")
        assertNotNull(returning.firstSeen, "the sessions themselves date the group")
    }

    @Test
    fun `sessions inside a group are ordered by how recently they were used`() {
        val groups = SessionCohorts.group(SecurityFixtures.ledger(), SecurityFixtures.cohorts())

        val returning = groups.first { it.cohortHash == SecurityFixtures.RETURNING_COHORT }
        assertEquals(
            returning.sessions.sortedByDescending { it.lastActiveAt },
            returning.sessions,
        )
    }

    @Test
    fun `the seeded ledger groups without losing a session`() =
        runTest {
            val repositories = SecurityFixtures.repositories(MockScenario.Default)
            val ledger = repositories.sessions.sessions().first()
            val cohorts = repositories.sessions.cohorts().first()

            val groups = SessionCohorts.group(ledger, cohorts)

            assertEquals(ledger.size, groups.sumOf { it.sessionCount }, "grouping never drops a row")
            assertTrue(groups.any(SessionCohortGroup::isCurrent), "the current session is somewhere in it")
        }
}
