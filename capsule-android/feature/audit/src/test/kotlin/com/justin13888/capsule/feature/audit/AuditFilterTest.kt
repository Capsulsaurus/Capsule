package com.justin13888.capsule.feature.audit

import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The filter's own rules, checked without a screen.
 *
 * Worth its own cases because the filter is written as inclusions: the empty selection is the
 * one shape a reader is most likely to assume means "everything", and it deliberately does
 * not.
 */
class AuditFilterTest {
    @Test
    fun `the default filter lets every entry through`() {
        val filter = AuditFilter()

        assertTrue(filter.isUnfiltered)
        assertTrue(filter.accepts(AuditFixtures.CREATE))
        assertTrue(filter.accepts(AuditFixtures.TAKEDOWN))
    }

    @Test
    fun `an empty category selection matches nothing rather than everything`() {
        val filter = AuditFilter(categories = emptySet())

        assertFalse(filter.isUnfiltered)
        assertFalse(filter.accepts(AuditFixtures.CREATE))
        assertFalse(filter.accepts(AuditFixtures.TAKEDOWN))
    }

    @Test
    fun `narrowing to one device excludes what another device signed`() {
        val filter = AuditFilter(onlyDevice = AuditFixtures.THIS_DEVICE.id)

        assertTrue(filter.accepts(AuditFixtures.CREATE))
        assertFalse(filter.accepts(AuditFixtures.DELETE))
    }

    @Test
    fun `no device signs an operator action, so a device filter always excludes one`() {
        val filter = AuditFilter(onlyDevice = AuditFixtures.THIS_DEVICE.id)

        assertFalse(filter.accepts(AuditFixtures.TAKEDOWN))
    }
}
