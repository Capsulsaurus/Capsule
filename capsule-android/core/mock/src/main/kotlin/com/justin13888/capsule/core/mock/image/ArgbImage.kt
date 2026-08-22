package com.justin13888.capsule.core.mock.image

/**
 * A decoded image as a flat row-major `ARGB_8888` buffer.
 *
 * Deliberately **not** an `android.graphics.Bitmap`. Everything interesting about procedural
 * synthesis — the gradient, the geometry, the palette — is arithmetic, and keeping it in a
 * plain `IntArray` means the whole of it runs and is asserted on in a fast JVM unit test with
 * no Robolectric shadow, no `GraphicsMode` annotation, and no chance of a shadow silently
 * turning `drawRect` into a no-op and passing a test that proves nothing.
 *
 * Converting to a `Bitmap` is then one call at the very edge, in whatever Coil fetcher or
 * `Painter` needs one — see [ProceduralImages] for why that boundary sits where it does.
 */
class ArgbImage(
    /** Width in pixels. */
    val width: Int,
    /** Height in pixels. */
    val height: Int,
    /** `width * height` packed `ARGB_8888` pixels, row-major from the top-left. */
    val pixels: IntArray,
) {
    init {
        require(width > 0 && height > 0) { "an image must have positive dimensions" }
        require(pixels.size == width * height) {
            "expected ${width * height} pixels for ${width}x$height, got ${pixels.size}"
        }
    }

    /** Bytes this image occupies, which is what an LRU budget is spent in. */
    val byteCount: Int get() = pixels.size * Int.SIZE_BYTES

    /** The pixel at [x], [y]. */
    fun pixelAt(
        x: Int,
        y: Int,
    ): Int = pixels[y * width + x]

    companion object {
        /** Packs opaque [red], [green], [blue] channels into one `ARGB_8888` pixel. */
        fun pack(
            red: Int,
            green: Int,
            blue: Int,
        ): Int =
            OPAQUE_ALPHA or
                (red.coerceIn(0, CHANNEL_MAX) shl RED_SHIFT) or
                (green.coerceIn(0, CHANNEL_MAX) shl GREEN_SHIFT) or
                blue.coerceIn(0, CHANNEL_MAX)

        /** The red channel of [pixel]. */
        fun red(pixel: Int): Int = (pixel shr RED_SHIFT) and CHANNEL_MAX

        /** The green channel of [pixel]. */
        fun green(pixel: Int): Int = (pixel shr GREEN_SHIFT) and CHANNEL_MAX

        /** The blue channel of [pixel]. */
        fun blue(pixel: Int): Int = pixel and CHANNEL_MAX

        private const val OPAQUE_ALPHA = 0xFF shl 24
        private const val RED_SHIFT = 16
        private const val GREEN_SHIFT = 8
        private const val CHANNEL_MAX = 255
    }
}
