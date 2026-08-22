package com.justin13888.capsule.core.mock.image

import com.justin13888.capsule.core.mock.seed.Prng
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.Lqip
import com.justin13888.capsule.core.model.RepresentationTier
import com.justin13888.capsule.core.model.RgbColor

/**
 * Everything needed to synthesize one image, and nothing else.
 *
 * A data class so it doubles as the cache key: two requests that would render identically
 * *are* identical, and using the request itself as the key removes any chance of a key that
 * forgets a field and serves a 256 px tile where a 1080 px preview was asked for.
 */
data class ProceduralImageRequest(
    /** The asset being drawn. Its id seeds every shape in the picture. */
    val assetId: AssetId,
    /** The sidecar's placeholder, when metadata has synced. */
    val lqip: Lqip?,
    /** Width ÷ height of the source frame. */
    val aspectRatio: Float,
    /** Which rung of the ladder is being drawn. */
    val tier: RepresentationTier,
    /** The long edge to render at, in pixels. */
    val longEdgePx: Int,
) {
    /** The rendered width for [longEdgePx] at [aspectRatio]. */
    val width: Int get() = if (aspectRatio >= 1f) longEdgePx else (longEdgePx * aspectRatio).toInt().coerceAtLeast(1)

    /** The rendered height for [longEdgePx] at [aspectRatio]. */
    val height: Int get() = if (aspectRatio >= 1f) (longEdgePx / aspectRatio).toInt().coerceAtLeast(1) else longEdgePx
}

/**
 * The surface a Coil 3 `Fetcher` is built on.
 *
 * Declared as an interface even though there is exactly one implementation, because that is
 * the seam the whole design turns on: when real decoding lands, a `Fetcher` written against
 * this interface is repointed at the real decoder and **no UI changes at all**. If the
 * gallery called `ProceduralImages` directly, swapping in real images would mean editing
 * every composable that draws one.
 *
 * `decode` returning `null` is the sandboxed-decode-failure case, not an error: the decoder
 * process refused the bytes, the caller draws the placeholder, and the asset stays listed.
 */
interface ProceduralImageSource {
    /** The image for [request], or `null` when decoding was refused. */
    fun decode(request: ProceduralImageRequest): ArgbImage?
}

/**
 * Synthesizes an asset's imagery from its id, with no bundled photographs anywhere.
 *
 * Two properties matter and both come from the same decision — everything is derived from
 * [ProceduralImageRequest.assetId] through a [Prng]. Reproducibility: the same asset draws
 * the same picture in every process, so a screenshot golden of a gallery is stable. Weight:
 * a forty-thousand-asset fixture adds not one byte to the repository, where even 8 KB of
 * bundled JPEG per asset would add a third of a gigabyte.
 *
 * A tile is a mesh gradient from the asset's own chroma grid with a little seeded geometry
 * over it. Noise would also be distinct per asset and would be useless: a grid of static is
 * unpleasant to look at, tells a reviewer nothing about whether the layout is right, and
 * hides the very banding and contrast problems a design review is looking for.
 */
class ProceduralImages(
    /** The rendered-image cache. Shared across every caller on the process. */
    val cache: ImageMemoryCache = ImageMemoryCache(ImageMemoryCache.DEFAULT_BUDGET_BYTES),
    /**
     * Whether an asset's bytes can be decoded at all on this device.
     *
     * The hook that makes the sandboxed-decode-failure state reachable from the UI without a
     * rebuild: a scenario points this at a predicate that refuses one landmark asset, and the
     * gallery renders exactly what it would render against a genuinely undecodable file.
     */
    private val decodable: (AssetId) -> Boolean = { true },
) : ProceduralImageSource {
    override fun decode(request: ProceduralImageRequest): ArgbImage? {
        if (!decodable(request.assetId)) return null
        return cache.getOrRender(request, ::render)
    }

    /** The chroma grid for [request] — its decoded payload, or one derived from its id. */
    fun gridFor(request: ProceduralImageRequest): ChromaGrid {
        val payload = request.lqip?.chromahash
        val decoded = payload?.let(ChromaHash::decode)
        if (decoded != null) return decoded
        val fallback = request.lqip?.dominantColor
        return if (fallback != null) flatGrid(fallback) else syntheticGrid(request.assetId)
    }

    private fun render(request: ProceduralImageRequest): ArgbImage {
        val grid = gridFor(request)
        val base = MeshGradient.render(grid, request.width, request.height)
        return if (request.tier == RepresentationTier.Lqip) base else withGeometry(base, request)
    }

    /**
     * Lays the asset's seeded geometry over the gradient.
     *
     * Drawn straight into the pixel buffer rather than through a `Canvas`: the whole point of
     * [ArgbImage] is that synthesis stays testable off-device, and reaching for `Canvas` here
     * would drag the one interesting part of the renderer back behind a Robolectric shadow.
     */
    private fun withGeometry(
        base: ArgbImage,
        request: ProceduralImageRequest,
    ): ArgbImage {
        val prng = Prng(Prng.mixSeed(GEOMETRY_SEED, request.assetId.value))
        val horizon = prng.nextDouble(HORIZON_LOW, HORIZON_HIGH)
        shadeHorizon(base, horizon)
        repeat(prng.nextInt(BLOB_MIN, BLOB_MAX)) {
            paintBlob(base, prng)
        }
        applyVignette(base)
        return base
    }

    /** Darkens everything below the horizon line, which is what gives a tile a "ground". */
    private fun shadeHorizon(
        image: ArgbImage,
        horizon: Double,
    ) {
        val horizonRow = (image.height * horizon).toInt().coerceIn(0, image.height)
        for (y in horizonRow until image.height) {
            val depth = (y - horizonRow).toDouble() / (image.height - horizonRow).coerceAtLeast(1)
            val scale = 1.0 - depth * GROUND_DARKENING
            for (x in 0 until image.width) {
                val index = y * image.width + x
                pixelScaled(image, index, scale)
            }
        }
    }

    /** Blends one soft radial blob into the buffer — a subject, a light, a highlight. */
    private fun paintBlob(
        image: ArgbImage,
        prng: Prng,
    ) {
        val centreX = prng.nextDouble() * image.width
        val centreY = prng.nextDouble() * image.height
        val radius = prng.nextDouble(BLOB_RADIUS_MIN, BLOB_RADIUS_MAX) * minOf(image.width, image.height)
        val tint = ArgbImage.pack(prng.nextInt(TINT_MAX), prng.nextInt(TINT_MAX), prng.nextInt(TINT_MAX))
        val strength = prng.nextDouble(BLOB_STRENGTH_MIN, BLOB_STRENGTH_MAX)
        val left = (centreX - radius).toInt().coerceAtLeast(0)
        val right = (centreX + radius).toInt().coerceAtMost(image.width - 1)
        val top = (centreY - radius).toInt().coerceAtLeast(0)
        val bottom = (centreY + radius).toInt().coerceAtMost(image.height - 1)
        for (y in top..bottom) {
            for (x in left..right) {
                val dx = (x - centreX) / radius
                val dy = (y - centreY) / radius
                val distance = dx * dx + dy * dy
                if (distance >= 1.0) continue
                val falloff = (1.0 - distance) * (1.0 - distance) * strength
                blend(image, y * image.width + x, tint, falloff)
            }
        }
    }

    /** A gentle corner darkening, so tiles read as photographs rather than as swatches. */
    private fun applyVignette(image: ArgbImage) {
        val halfWidth = image.width / 2.0
        val halfHeight = image.height / 2.0
        val maxDistance = halfWidth * halfWidth + halfHeight * halfHeight
        for (y in 0 until image.height) {
            for (x in 0 until image.width) {
                val dx = x - halfWidth
                val dy = y - halfHeight
                val fraction = (dx * dx + dy * dy) / maxDistance
                pixelScaled(image, y * image.width + x, 1.0 - fraction * VIGNETTE_STRENGTH)
            }
        }
    }

    private fun pixelScaled(
        image: ArgbImage,
        index: Int,
        scale: Double,
    ) {
        val pixel = image.pixels[index]
        image.pixels[index] =
            ArgbImage.pack(
                (ArgbImage.red(pixel) * scale).toInt(),
                (ArgbImage.green(pixel) * scale).toInt(),
                (ArgbImage.blue(pixel) * scale).toInt(),
            )
    }

    private fun blend(
        image: ArgbImage,
        index: Int,
        tint: Int,
        weight: Double,
    ) {
        val pixel = image.pixels[index]
        val keep = 1.0 - weight
        image.pixels[index] =
            ArgbImage.pack(
                (ArgbImage.red(pixel) * keep + ArgbImage.red(tint) * weight).toInt(),
                (ArgbImage.green(pixel) * keep + ArgbImage.green(tint) * weight).toInt(),
                (ArgbImage.blue(pixel) * keep + ArgbImage.blue(tint) * weight).toInt(),
            )
    }

    private fun flatGrid(colour: RgbColor): ChromaGrid = ChromaGrid(List(ChromaGrid.CELL_COUNT) { colour })

    /** A grid invented from the asset id, for an asset whose metadata has not arrived. */
    private fun syntheticGrid(assetId: AssetId): ChromaGrid {
        val prng = Prng(Prng.mixSeed(GRID_SEED, assetId.value))
        return ChromaGrid(
            List(ChromaGrid.CELL_COUNT) {
                RgbColor(prng.nextInt(CHANNEL_STEPS), prng.nextInt(CHANNEL_STEPS), prng.nextInt(CHANNEL_STEPS))
            },
        )
    }

    private companion object {
        const val GEOMETRY_SEED = 0x5EED_6E0DL
        const val GRID_SEED = 0x5EED_6215L
        const val CHANNEL_STEPS = 256
        const val HORIZON_LOW = 0.35
        const val HORIZON_HIGH = 0.75
        const val GROUND_DARKENING = 0.35
        const val BLOB_MIN = 2
        const val BLOB_MAX = 5
        const val BLOB_RADIUS_MIN = 0.18
        const val BLOB_RADIUS_MAX = 0.55
        const val BLOB_STRENGTH_MIN = 0.18
        const val BLOB_STRENGTH_MAX = 0.55
        const val TINT_MAX = 256
        const val VIGNETTE_STRENGTH = 0.30
    }
}
