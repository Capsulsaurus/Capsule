package com.justin13888.capsule.core.data

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.io.File

/**
 * Holds the error hierarchy to the canonical message catalogue, in both directions.
 *
 * The contract clients depend on is that a server sends a stable `error.*` code and the
 * client localizes it. Two failures break that, and only a bidirectional check catches both:
 * a variant naming a code the catalogue does not hold would render a missing-key placeholder,
 * and a catalogue code with no variant would arrive unrecognised and surface the raw key at
 * the user.
 *
 * The catalogue is read from `locales/en.json` rather than mirrored here on purpose. A
 * hardcoded copy would be a second source of truth that drifts silently — which is the exact
 * failure this test exists to prevent.
 */
class CapsuleErrorCatalogueTest {
    private val catalogueCodes: Set<String> = readCatalogueCodes()
    private val declaredCodes: List<String> = ALL_CODED_ERRORS.map { requireNotNull(it.code).value }

    @Test
    fun `the catalogue was found and is not empty`() {
        // Guards the rest of the class: a silently empty catalogue would make every
        // assertion below vacuously pass.
        assertTrue(catalogueCodes.size > MIN_EXPECTED_CODES, "found only ${catalogueCodes.size} codes")
    }

    @Test
    fun `every declared variant names a real catalogue key`() {
        val unknown = declaredCodes.filterNot { it in catalogueCodes }.sorted()
        assertEquals(emptyList<String>(), unknown, "these codes are not in locales/en.json")
    }

    @Test
    fun `every catalogue key has a variant to carry it`() {
        val uncovered = (catalogueCodes - declaredCodes.toSet()).sorted()
        assertEquals(emptyList<String>(), uncovered, "these catalogue codes have no CapsuleError variant")
    }

    @Test
    fun `no two variants claim the same code`() {
        val duplicates =
            declaredCodes
                .groupingBy { it }
                .eachCount()
                .filterValues { it > 1 }
                .keys
                .sorted()
        assertEquals(emptyList<String>(), duplicates, "a code is claimed by more than one variant")
    }

    @Test
    fun `every variant carries a non-empty English detail`() {
        // The detail stays English beside the localized code so it remains greppable in a
        // support bundle; an empty one would defeat that.
        ALL_CODED_ERRORS.forEach {
            assertTrue(it.detail.isNotBlank(), "${it.code?.value} has no detail message")
        }
    }

    @Test
    fun `a coded error carries its code and its message`() {
        assertEquals("error.quota.exceeded", QuotaError.Exceeded.code?.value)
        assertEquals("Your storage is full.", QuotaError.Exceeded.detail)
        assertEquals(QuotaError.Exceeded.detail, QuotaError.Exceeded.message)
    }

    @Test
    fun `the awaiting-original code is distinct from any permanent failure`() {
        // The transient pending state must never be conflated with a gone blob, or a client
        // would show a failure for an asset that is simply still uploading elsewhere.
        assertEquals("error.blob.pending_upload", BlobError.PendingUpload.code?.value)
        assertTrue(declaredCodes.contains("error.blob.pending_upload"))
    }

    @Test
    fun `local failures deliberately carry no code`() {
        assertNull(LocalError.Offline.code)
        assertNull(LocalError.NotFound("asset").code)
        assertNull(LocalError.Cancelled.code)
        assertTrue(ALL_CODED_ERRORS.none { it is LocalError })
    }

    @Test
    fun `a local failure keeps the cause it wrapped`() {
        val root = IllegalStateException("disk on fire")
        val wrapped = LocalError.Unexpected(root)
        assertEquals(root, wrapped.cause)
        assertEquals(root, wrapped.underlying)
    }

    @Test
    fun `every coded error is a CapsuleError and therefore throwable`() {
        val thrown = runCatching { throw QuotaError.GraceLocked }.exceptionOrNull()
        assertNotNull(thrown)
        assertTrue(thrown is CapsuleError)
        assertEquals("error.quota.grace_locked", (thrown as CapsuleError).code?.value)
    }

    private fun readCatalogueCodes(): Set<String> {
        val catalogue = locateCatalogue()
        return CODE_KEY.findAll(catalogue.readText()).map { it.groupValues[1] }.toSet()
    }

    /**
     * Walks up from the working directory to the repository root.
     *
     * Resolved rather than hardcoded relative to the module so the test keeps working
     * wherever the runner sets its working directory.
     */
    private fun locateCatalogue(): File {
        var dir: File? = File(System.getProperty("user.dir")).absoluteFile
        while (dir != null) {
            val candidate = File(dir, CATALOGUE_PATH)
            if (candidate.isFile) return candidate
            dir = dir.parentFile
        }
        error("could not find $CATALOGUE_PATH above ${System.getProperty("user.dir")}")
    }

    private companion object {
        const val CATALOGUE_PATH = "locales/en.json"
        val CODE_KEY = Regex("\"(error\\.[a-z0-9_.]+)\"\\s*:")

        /** A floor, not a count — the catalogue grows and this test should not fight it. */
        const val MIN_EXPECTED_CODES = 50
    }
}
