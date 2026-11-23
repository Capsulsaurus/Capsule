package com.justin13888.capsule.feature.sharing

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.junit4.ComposeContentTestRule
import androidx.test.core.app.ApplicationProvider
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.ShareLinkId
import com.justin13888.capsule.core.model.ShareScope
import java.time.Instant

/**
 * The pieces every screen test in this module needs.
 *
 * Screens are rendered inside a real [CapsuleTheme] rather than bare, because half of what
 * these tests check — a disabled control, a degraded banner — is expressed through the
 * theme's semantic colours, and a composition without it is not the composition that ships.
 */
internal object ScreenHarness {
    /** The app context, for resolving the catalogue strings an assertion matches against. */
    val context: Context get() = ApplicationProvider.getApplicationContext()

    /** A catalogue string, so no test ever hard-codes user-facing copy. */
    fun string(id: Int): String = context.getString(id)

    /** Renders [content] under the real theme and settles the composition. */
    fun ComposeContentTestRule.setThemedContent(content: @Composable () -> Unit) {
        setContent { CapsuleTheme(darkTheme = false, dynamicColor = false) { content() } }
        waitForIdle()
    }

    /** A share link with a known opaque id and secret, for URL and QR assertions. */
    fun link(
        secret: String? = "c9f0f895fb98ab9159f51fd0297e236d",
        expiresAt: Instant? = null,
        requiresPassphrase: Boolean = false,
    ): ShareLink =
        ShareLink(
            id = ShareLinkId("link-under-test"),
            opaqueId = "8f14e45fceea167a5a36dedd4bea2543",
            scope = ShareScope.WholeAlbum(AlbumId("album-1")),
            expiresAt = expiresAt,
            createdAt = Instant.parse("2026-06-15T19:00:00Z"),
            requiresPassphrase = requiresPassphrase,
            fragmentSecret = secret,
        )
}
