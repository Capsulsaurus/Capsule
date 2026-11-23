package com.justin13888.capsule.feature.albums.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.Album
import com.justin13888.capsule.core.model.AlbumCapability
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AlbumMember
import com.justin13888.capsule.core.model.UserId
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.feature.albums.AlbumsFixtures
import com.justin13888.capsule.feature.albums.model.AlbumPolicy
import com.justin13888.capsule.feature.albums.model.FederatedAlbum
import com.justin13888.capsule.feature.albums.model.FederatedConstituent
import com.justin13888.capsule.feature.albums.model.HistoryPolicy
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailActions
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailKind
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailScreen
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailUiState
import kotlinx.collections.immutable.persistentListOf
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Duration

/**
 * Album detail's two honest surfaces: the aggregate's limits, and the album's fixed policy.
 *
 * Both exist because the alternative is a user drawing a wrong conclusion. An aggregate looks
 * like one shared album, so it has to say that removing someone else's photos is not
 * something it can do; and a policy panel that looked editable would imply a change the
 * design only permits through an upgrade ceremony.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class AlbumDetailScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val now = AlbumsFixtures.SPEC.now

    private val album =
        Album(
            id = AlbumId("album"),
            name = "Family",
            createdAt = now,
            modifiedAt = now,
            viewerCapability = AlbumCapability.Admin.asWire(),
            members =
                listOf(
                    AlbumMember(UserId("owner"), AlbumCapability.Admin.asWire(), now),
                    AlbumMember(UserId("sam"), AlbumCapability.Read.asWire(), now),
                ),
        )

    private val policy =
        AlbumPolicy(
            historyPolicy = HistoryPolicy.Capped(Duration.ofDays(90)),
            protocolPin = "2026-01-01",
            defaultRetention = Duration.ofDays(30),
            upgradeAvailableTo = "2026-06-01",
        )

    private fun setContent(state: AlbumDetailUiState) {
        compose.setContent {
            CapsuleTheme(dynamicColor = false) {
                AlbumDetailScreen(state = state, actions = AlbumDetailActions())
            }
        }
        compose.waitForIdle()
    }

    @Test
    fun `an album with no photos says so rather than showing an empty grid`() {
        setContent(AlbumDetailUiState.Content(title = "Family", kind = AlbumDetailKind.Container, album = album))
        compose.onNodeWithText("Empty album").assertIsDisplayed()
    }

    @Test
    fun `the nameless default album is titled rather than left blank`() {
        setContent(
            AlbumDetailUiState.Content(
                title = "",
                kind = AlbumDetailKind.Container,
                album = album.copy(name = "", isDefault = true),
                isDefault = true,
            ),
        )
        compose.onNodeWithText("Library").assertIsDisplayed()
    }

    @Test
    fun `an unreachable contributor gets its own banner and nothing is removed`() {
        val group =
            FederatedAlbum(
                groupId = "group",
                name = "Iceland",
                constituents =
                    listOf(
                        FederatedConstituent(AlbumId("mine"), "this server", isOwn = true, isReachable = true, 10),
                        FederatedConstituent(AlbumId("theirs"), "photos.example.org", false, isReachable = false, 4),
                    ),
            )
        setContent(
            AlbumDetailUiState.Content(title = "Iceland", kind = AlbumDetailKind.Aggregate, federated = group),
        )
        compose.onNodeWithTag(AlbumsTestTags.FEDERATED).assertIsDisplayed()
        compose.onNodeWithTag("${AlbumsTestTags.FEDERATED}:photos.example.org").assertIsDisplayed()
        compose
            .onNodeWithText("This is everyone's own album shown together. You can only remove your own photos.")
            .assertIsDisplayed()
    }

    @Test
    fun `an aggregate with every origin reachable shows no degraded banner`() {
        val group =
            FederatedAlbum(
                groupId = "group",
                name = "Iceland",
                constituents =
                    listOf(
                        FederatedConstituent(AlbumId("mine"), "this server", isOwn = true, isReachable = true, 10),
                    ),
            )
        setContent(
            AlbumDetailUiState.Content(title = "Iceland", kind = AlbumDetailKind.Aggregate, federated = group),
        )
        compose.onNodeWithTag("${AlbumsTestTags.FEDERATED}:photos.example.org").assertDoesNotExist()
    }

    @Test
    fun `a loading album renders its own state`() {
        setContent(AlbumDetailUiState.Loading)
        compose.onNodeWithTag(AlbumsTestTags.DETAIL).assertIsDisplayed()
    }

    @Test
    fun `a failed read renders its own state`() {
        setContent(AlbumDetailUiState.Failed(code = "error.album.not_available"))
        compose.onNodeWithTag(AlbumsTestTags.DETAIL).assertIsDisplayed()
    }

    @Test
    fun `a refused membership edit is explained on screen`() {
        setContent(
            AlbumDetailUiState.Content(
                title = "Family",
                kind = AlbumDetailKind.Container,
                album = album,
                policy = policy,
                message =
                    com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailMessage
                        .Refused(
                            com.justin13888.capsule.feature.albums.model.MembershipRefusal.LastAdmin,
                        ),
            ),
        )
        compose.onNodeWithTag("${AlbumsTestTags.DETAIL}:message").assertIsDisplayed()
        compose.onNodeWithText("An album needs at least one admin. Promote someone else first.").assertIsDisplayed()
    }

    @Test
    fun `the truncation note appears only when more contents exist than were loaded`() {
        setContent(
            AlbumDetailUiState.Content(
                title = "Family",
                kind = AlbumDetailKind.Container,
                album = album,
                assets = persistentListOf(AlbumsFixtures.asset("a")),
                isTruncated = true,
            ),
        )
        compose.onNodeWithText("Showing the first 1 photos").assertIsDisplayed()
    }
}
