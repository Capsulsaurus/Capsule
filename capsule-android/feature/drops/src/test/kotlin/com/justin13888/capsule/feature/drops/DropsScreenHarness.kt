package com.justin13888.capsule.feature.drops

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.test.junit4.ComposeContentTestRule
import androidx.test.core.app.ApplicationProvider
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.BlobHash
import com.justin13888.capsule.core.model.ContentType
import com.justin13888.capsule.core.model.DropDescriptor
import com.justin13888.capsule.core.model.DropId
import com.justin13888.capsule.core.model.LinkCaps
import com.justin13888.capsule.core.model.PendingDrop
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.UploadLinkId
import com.justin13888.capsule.core.model.asWire
import java.time.Instant

/**
 * The pieces every screen test in this module needs.
 *
 * Screens render inside a real [CapsuleTheme] rather than bare, because much of what these
 * tests check — a disabled action, a degraded banner, a quarantine-toned badge — is expressed
 * through the theme's semantic colours, and a composition without it is not the one that
 * ships.
 */
internal object DropsScreenHarness {
    /** The app context, for resolving the catalogue strings an assertion matches against. */
    val context: Context get() = ApplicationProvider.getApplicationContext()

    /** A catalogue string, so no test ever hard-codes user-facing copy. */
    fun string(id: Int): String = context.getString(id)

    /** Renders [content] under the real theme and settles the composition. */
    fun ComposeContentTestRule.setThemedContent(content: @Composable () -> Unit) {
        setContent { CapsuleTheme(darkTheme = false, dynamicColor = false) { content() } }
        waitForIdle()
    }

    /** An upload link with known caps and usage. */
    fun uploadLink(
        caps: LinkCaps =
            LinkCaps(
                expiresAt = Instant.parse("2026-09-01T00:00:00Z"),
                maxTotalBytes = 8L * 1000 * 1000 * 1000,
                maxFileCount = 200,
            ),
        bytesReceived: Long = 412L * 1000 * 1000,
        filesReceived: Int = 3,
        requiresPassphrase: Boolean = false,
    ): UploadLink =
        UploadLink(
            id = UploadLinkId("upload-link-under-test"),
            opaqueId = "3c59dc048e8850243be8079a5c74d079",
            caps = caps,
            createdAt = Instant.parse("2026-06-01T00:00:00Z"),
            requiresPassphrase = requiresPassphrase,
            bytesReceived = bytesReceived,
            filesReceived = filesReceived,
        )

    /** One pending deposit, with the guest's own unverified filename on it. */
    fun drop(
        id: String = "drop-under-test",
        suggestedFilename: String? = "wedding-ceremony-0431.jpg",
    ): PendingDrop =
        PendingDrop(
            id = DropId(id),
            descriptor =
                DropDescriptor(
                    contentType = ContentType.Jpeg.asWire(),
                    plaintextSize = 4_200_000,
                    ciphertextHash = BlobHash("a".repeat(64)),
                    suggestedFilename = suggestedFilename,
                ),
            viaLink = UploadLinkId("upload-link-under-test"),
            receivedAt = Instant.parse("2026-06-15T09:00:00Z"),
        )
}
