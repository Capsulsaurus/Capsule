package com.justin13888.capsule.feature.storage

import com.justin13888.capsule.core.mock.MockScenario
import com.justin13888.capsule.core.model.HeldRepresentations
import com.justin13888.capsule.core.model.RepresentationTier
import kotlinx.coroutines.test.runTest
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * The estimate behind the consumer ranking.
 *
 * Asserted as an *ordering* rather than as byte figures, because ordering is the only thing
 * the screen actually claims: the user is asking which item is worth clearing first, and that
 * answer survives the constant factor being approximate.
 */
class LocalFootprintTest {
    @Test
    fun `the placeholder tier costs nothing because it rides inside the metadata`() =
        runTest {
            val asset = anyAsset()

            assertEquals(0L, LocalFootprint.bytesOf(asset, RepresentationTier.Lqip))
        }

    @Test
    fun `the ladder costs more the further up it goes`() =
        runTest {
            val asset = anyAsset()

            val thumbnail = LocalFootprint.bytesOf(asset, RepresentationTier.Thumbnail)
            val preview = LocalFootprint.bytesOf(asset, RepresentationTier.Preview)
            val original = LocalFootprint.bytesOf(asset, RepresentationTier.Original)

            assertTrue(thumbnail < preview, "a grid thumbnail is cheaper than a screen preview")
            assertTrue(preview < original, "a screen preview is cheaper than the original")
        }

    @Test
    fun `holding more rungs costs more than holding fewer`() =
        runTest {
            val asset = anyAsset()
            val thin = asset.copy(heldRepresentations = HeldRepresentations.LQIP_ONLY)
            val thick =
                asset.copy(
                    heldRepresentations =
                        HeldRepresentations(
                            setOf(
                                RepresentationTier.Lqip,
                                RepresentationTier.Thumbnail,
                                RepresentationTier.Preview,
                                RepresentationTier.Original,
                            ),
                        ),
                )

            assertTrue(LocalFootprint.bytesOf(thin) < LocalFootprint.bytesOf(thick))
        }

    @Test
    fun `an asset the extractor could not measure still gets a usable estimate`() =
        runTest {
            val asset = anyAsset().copy(dimensions = null)

            assertTrue(
                LocalFootprint.bytesOf(asset, RepresentationTier.Original) > 0,
                "an unmeasurable frame still occupies room and must still be rankable",
            )
        }

    private suspend fun anyAsset() =
        StorageFixtures
            .repositories(MockScenario.Default)
            .timeline
            .timelineWindow(0, 1)
            .first()
}
