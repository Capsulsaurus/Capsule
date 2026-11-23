package com.justin13888.capsule.feature.security

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The dispute path's output.
 *
 * Asserted for what it contains *and* for what it does not: the report exists so a person can
 * act on a grouping the user cannot adjudicate, and a report that carried more than the
 * identifiers would be a privacy cost for no extra help.
 */
class SupportBundleTest {
    @Test
    fun `it names the exact hash a person has to look up`() {
        val group = groups().first { it.cohortHash == SecurityFixtures.RETURNING_COHORT }

        val report = SupportBundle.render(group)

        assertTrue(report.contains(SecurityFixtures.RETURNING_COHORT))
    }

    @Test
    fun `it lists every session under the cohort with its device`() {
        val group = groups().first { it.cohortHash == SecurityFixtures.RETURNING_COHORT }

        val report = SupportBundle.render(group)

        group.sessions.forEach { session ->
            assertTrue(report.contains(session.id.value), "session ${session.id.value} is listed")
            assertTrue(report.contains(session.deviceId?.value.orEmpty()), "with the device it enrolled as")
        }
    }

    @Test
    fun `a group that asserted no cohort still produces a usable report`() {
        val group = groups().first { it.cohortHash == null }

        val report = SupportBundle.render(group)

        assertTrue(report.contains("none"), "the absent hash is stated rather than left blank")
        assertTrue(
            report.contains(
                group.sessions
                    .first()
                    .id.value,
            ),
        )
    }

    @Test
    fun `it carries nothing about the library`() {
        val report = SupportBundle.render(groups().first())

        assertFalse(report.contains("asset"), "a session dispute is not a reason to send photo ids")
        assertFalse(report.contains("album"))
    }

    @Test
    fun `timestamps are in UTC so they line up with the server side records`() {
        val report = SupportBundle.render(groups().first())

        assertTrue(report.contains("Z"), "ISO-8601 instants, not a reader's locale")
    }

    private fun groups() = SessionCohorts.group(SecurityFixtures.ledger(), SecurityFixtures.cohorts())
}
