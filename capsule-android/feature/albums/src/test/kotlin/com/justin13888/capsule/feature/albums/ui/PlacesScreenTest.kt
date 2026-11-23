package com.justin13888.capsule.feature.albums.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import com.justin13888.capsule.core.designsystem.theme.CapsuleTheme
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.GpsDatum
import com.justin13888.capsule.feature.albums.model.MapBounds
import com.justin13888.capsule.feature.albums.model.PlaceCluster
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.places.PlacesScreen
import com.justin13888.capsule.feature.albums.ui.places.PlacesUiState
import kotlinx.collections.immutable.persistentListOf
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Places draws its own map and admits when a pin is approximate.
 *
 * The datum banner is the interesting case. Coordinates are stored verbatim in whatever datum
 * their source supplied and are never converted at rest, so a GCJ-02 fix plotted on a WGS-84
 * map sits a few hundred metres from where the photograph was taken — and saying so is the
 * only honest option, since the conversion has no exact inverse.
 */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w412dp-h915dp-xhdpi")
class PlacesScreenTest {
    @get:Rule
    val compose = createComposeRule()

    private val cluster =
        PlaceCluster(
            latitude = 49.3,
            longitude = -123.1,
            datum = GpsDatum.Wgs84,
            assetIds = listOf(AssetId("a"), AssetId("b")),
        )

    private fun setContent(state: PlacesUiState) {
        compose.setContent {
            CapsuleTheme(dynamicColor = false) {
                PlacesScreen(state = state, onBack = {}, onSelect = {}, onOpenAsset = {}, onRetry = {})
            }
        }
        compose.waitForIdle()
    }

    @Test
    fun `a library with no located photographs says so instead of drawing an empty map`() {
        setContent(PlacesUiState.Content())
        compose.onNodeWithTag(AlbumsTestTags.PLACES_MAP).assertDoesNotExist()
        compose.onNodeWithText("No places yet").assertIsDisplayed()
    }

    @Test
    fun `the map is drawn from the pins alone, with no tile request anywhere`() {
        setContent(
            PlacesUiState.Content(
                clusters = persistentListOf(cluster),
                bounds = requireNotNull(MapBounds.around(listOf(cluster))),
            ),
        )
        compose.onNodeWithTag(AlbumsTestTags.PLACES_MAP).assertIsDisplayed()
    }

    @Test
    fun `a second datum is disclosed rather than silently converted`() {
        val gcj = cluster.copy(datum = GpsDatum.Gcj02)
        setContent(
            PlacesUiState.Content(
                clusters = persistentListOf(gcj),
                bounds = requireNotNull(MapBounds.around(listOf(gcj))),
                hasSecondDatum = true,
            ),
        )
        compose.onNodeWithTag("${AlbumsTestTags.PLACES}:datum").assertIsDisplayed()
    }

    @Test
    fun `selecting a pin names how many photographs it stands for`() {
        setContent(
            PlacesUiState.Content(
                clusters = persistentListOf(cluster),
                bounds = requireNotNull(MapBounds.around(listOf(cluster))),
                selected = cluster,
            ),
        )
        compose.onNodeWithTag("${AlbumsTestTags.PLACES}:selected").assertIsDisplayed()
        compose.onNodeWithText("2 photos here").assertIsDisplayed()
    }
}
