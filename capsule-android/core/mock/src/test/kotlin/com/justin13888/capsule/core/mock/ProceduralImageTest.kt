package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.mock.image.ArgbImage
import com.justin13888.capsule.core.mock.image.ChromaGrid
import com.justin13888.capsule.core.mock.image.ChromaHash
import com.justin13888.capsule.core.mock.image.ImageMemoryCache
import com.justin13888.capsule.core.mock.image.ProceduralImageRequest
import com.justin13888.capsule.core.mock.image.ProceduralImages
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.RgbColor
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Procedural imagery: reproducible, distinct per asset, and bounded in memory.
 *
 * Asserted on the pixel buffer rather than on a `Bitmap`, which is exactly why the renderer
 * produces an [ArgbImage] in the first place — a Robolectric shadow can turn a draw into a
 * no-op and pass a test that proves nothing, while an `IntArray` cannot.
 */
class ProceduralImageTest {
    private val library = MockFixtures.LIBRARY

    private fun requestFor(
        asset: Asset,
        tier: RepresentationTier = RepresentationTier.Thumbnail,
        longEdge: Int = 48,
    ) = ProceduralImageRequest(
        assetId = asset.id,
        lqip = asset.lqip,
        aspectRatio = asset.dimensions?.aspectRatio ?: 1f,
        tier = tier,
        longEdgePx = longEdge,
    )

    @Test
    fun `a chroma grid survives a round trip through its wire form`() {
        val grid = ChromaGrid(List(ChromaGrid.CELL_COUNT) { RgbColor(it * 28, 255 - it * 28, 128) })
        val decoded = requireNotNull(ChromaHash.decode(ChromaHash.encode(grid)))
        // Four bits a channel, so the round trip is lossy by design — but only to the nearest
        // sixteenth, which is invisible once the placeholder is stretched over a tile.
        decoded.cells.forEachIndexed { index, cell ->
            assertTrue(kotlin.math.abs(cell.red - grid.cells[index].red) <= CHANNEL_TOLERANCE)
        }
    }

    @Test
    fun `an unrecognised payload decodes to nothing rather than throwing`() {
        // The renderer's response to all three failures is identical — fill with the dominant
        // colour — so an exception would only force that decision to be written twice.
        assertNull(ChromaHash.decode(""))
        assertNull(ChromaHash.decode("9" + "0".repeat(27)))
        assertNull(ChromaHash.decode("1" + "z".repeat(27)))
    }

    @Test
    fun `the same asset renders identically in two independent instances`() {
        val asset = library.assets.first()
        val first = ProceduralImages().decode(requestFor(asset))
        val second = ProceduralImages().decode(requestFor(asset))
        assertNotNull(first)
        assertArrayEquals(first!!.pixels, second!!.pixels)
    }

    @Test
    fun `different assets render visibly differently`() {
        val images = ProceduralImages()
        val distinct =
            library.assets
                .take(SAMPLE_SIZE)
                .mapNotNull { images.decode(requestFor(it))?.pixels?.toList() }
                .distinct()
        assertEquals(SAMPLE_SIZE, distinct.size, "two assets produced the same tile")
    }

    @Test
    fun `a thumbnail is the placeholder plus the asset's own geometry`() {
        val asset = library.assets.first()
        val images = ProceduralImages()
        val lqip = requireNotNull(images.decode(requestFor(asset, RepresentationTier.Lqip)))
        val thumbnail = requireNotNull(images.decode(requestFor(asset, RepresentationTier.Thumbnail)))
        assertEquals(lqip.width, thumbnail.width)
        // The seeded geometry is what makes tiles distinguishable at grid size; without it the
        // grid is a wall of near-identical blurs and tells a layout reviewer nothing.
        assertFalse(lqip.pixels.contentEquals(thumbnail.pixels))
        val changed = lqip.pixels.indices.count { lqip.pixels[it] != thumbnail.pixels[it] }
        assertTrue(
            changed > lqip.pixels.size / 4,
            "geometry touched only $changed of ${lqip.pixels.size} pixels",
        )
    }

    @Test
    fun `an asset with no usable placeholder still renders something`() {
        val unreadable = library.assetsById.getValue(requireNotNull(library.landmarks.unreadableAssetId))
        // Its payload carries an unrecognised format version, so decoding falls back to the
        // dominant colour rather than misrendering unknown bytes.
        assertNull(ChromaHash.decode(unreadable.lqip?.chromahash.orEmpty()))
        val rendered = requireNotNull(ProceduralImages().decode(requestFor(unreadable)))
        assertTrue(rendered.pixels.isNotEmpty())
    }

    @Test
    fun `rendered dimensions follow the asset's aspect ratio`() {
        val panorama = library.assetsById.getValue(requireNotNull(library.landmarks.panoramaAssetId))
        val request = requestFor(panorama, longEdge = 120)
        assertEquals(120, request.width)
        assertTrue(request.height < request.width)
        val rendered = requireNotNull(ProceduralImages().decode(request))
        assertEquals(request.width, rendered.width)
        assertEquals(request.height, rendered.height)
    }

    @Test
    fun `the cache serves a repeat request and evicts under budget`() {
        val budget = ArgbImage(TILE, TILE, IntArray(TILE * TILE)).byteCount * 2
        val cache = ImageMemoryCache(budget)
        val images = ProceduralImages(cache = cache)
        val assets = library.assets.take(4)
        assets.forEach { images.decode(requestFor(it, longEdge = TILE)) }
        assertTrue(cache.bytesInUse <= budget)
        assertTrue(cache.size <= 2, "cache held ${cache.size} entries over a two-entry budget")
        // A repeat request is served from the cache rather than re-rendered, which is what
        // keeps a fling smooth.
        val cached = images.decode(requestFor(assets.last(), longEdge = TILE))
        assertTrue(cached === images.decode(requestFor(assets.last(), longEdge = TILE)))
        cache.clear()
        assertEquals(0, cache.bytesInUse)
    }

    @Test
    fun `the cache budget scales with the device's heap and stays bounded`() {
        val small = ImageMemoryCache.forMemoryClass(16)
        val large = ImageMemoryCache.forMemoryClass(1024)
        assertTrue(small.maxBytes <= large.maxBytes)
        // Floored and capped: a fixture that provokes the OutOfMemoryError it was meant to
        // help debug is worse than useless.
        assertTrue(small.maxBytes >= MIN_BUDGET)
        assertTrue(large.maxBytes <= MAX_BUDGET)
    }

    @Test
    fun `a refused decode returns nothing to draw`() {
        val images = ProceduralImages(decodable = { false })
        assertNull(images.decode(requestFor(library.assets.first())))
    }

    private companion object {
        const val CHANNEL_TOLERANCE = 17
        const val SAMPLE_SIZE = 24
        const val TILE = 32
        const val MIN_BUDGET = 4 * 1024 * 1024
        const val MAX_BUDGET = 96 * 1024 * 1024
    }
}
