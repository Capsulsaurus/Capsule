package com.justin13888.capsule.feature.sharing

import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.ShareError
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.ShareLinkId
import com.justin13888.capsule.core.model.ShareScope
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import java.time.Instant

/**
 * The URL a share is handed out as, and the failure classification behind every sharing
 * screen's remediation.
 *
 * The fragment assertions are the important ones. A URL that loses everything after `#` still
 * resolves, still looks like a link, and decrypts to nothing — so the only way to keep a
 * later "tidy-up" from shipping dead links is to state the requirement as a test.
 */
class ShareUrlsTest {
    private val origin = LinkOrigin("https://capsule.test")

    private fun link(secret: String?) =
        ShareLink(
            id = ShareLinkId("link-1"),
            opaqueId = "8f14e45fceea167a5a36dedd4bea2543",
            scope = ShareScope.WholeAlbum(AlbumId("album-1")),
            createdAt = Instant.parse("2026-06-15T19:00:00Z"),
            fragmentSecret = secret,
        )

    @Test
    fun `the URL carries the read secret in its fragment`() {
        val url = link("c9f0f895fb98ab9159f51fd0297e236d").shareUrl(origin)
        assertEquals(
            "https://capsule.test/s/8f14e45fceea167a5a36dedd4bea2543#c9f0f895fb98ab9159f51fd0297e236d",
            url,
        )
        assertTrue(url!!.substringAfter('#').isNotEmpty(), "the fragment is what decrypts the share")
    }

    @Test
    fun `the path half carries no scope`() {
        val url = requireNotNull(link("secret").shareUrl(origin))
        val path = url.substringBefore('#')
        assertTrue(!path.contains("album"), "the opaque id must not leak what the link points at")
    }

    @Test
    fun `a link with no secret has no shareable URL at all`() {
        // Not a shorter URL: a link the server handed back cannot be completed here, and
        // offering the prefix would be a copy button that produces something unopenable.
        assertNull(link(null).shareUrl(origin))
    }

    @Test
    fun `the privacy strip is a fixed list, not a selection`() {
        assertEquals(StrippedField.entries.toList(), StrippedField.ALL)
        assertEquals(4, StrippedField.ALL.size)
    }

    @Test
    fun `offline is classified apart from a server refusal`() {
        assertEquals(SharingFailureKind.Offline, SharingFailure.of(LocalError.Offline).kind)

        val reported = SharingFailure.of(ShareError.RateLimited)
        assertEquals(SharingFailureKind.Reported, reported.kind)
        assertEquals("error.share.rate_limited", reported.code?.value)

        val unknown = SharingFailure.of(IllegalStateException("boom"))
        assertEquals(SharingFailureKind.Unknown, unknown.kind)
        assertNull(unknown.code)
    }
}
