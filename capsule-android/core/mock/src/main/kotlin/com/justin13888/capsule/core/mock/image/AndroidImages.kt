package com.justin13888.capsule.core.mock.image

import android.app.ActivityManager
import android.content.Context
import android.graphics.Bitmap

/*
 * The two lines of this module that touch the Android framework.
 *
 * Everything above them — the gradient, the geometry, the cache policy — is plain arithmetic
 * over an [IntArray] and is tested as such. Keeping the framework contact down to a heap
 * query and a `Bitmap` wrap is what makes that possible, and it is also what makes the
 * eventual swap to real decoding a small change: the real decoder produces a `Bitmap` the
 * same way, at the same boundary.
 */

/**
 * An [ImageMemoryCache] budgeted from this device's per-process heap.
 *
 * `ActivityManager.memoryClass` rather than `Runtime.maxMemory()`: the former is the limit
 * the platform will actually enforce on an ordinary (non-`largeHeap`) process, while the
 * latter reports whatever the VM happens to have been started with and reads high on a
 * device that would still kill the process well below it.
 */
fun Context.proceduralImageCache(): ImageMemoryCache {
    val activityManager = getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager
    val memoryClass = activityManager?.memoryClass ?: FALLBACK_MEMORY_CLASS_MB
    return ImageMemoryCache.forMemoryClass(memoryClass)
}

/**
 * Wraps this buffer in a `Bitmap` without a copy of the pixel data beyond the one
 * `createBitmap` performs itself.
 *
 * `ARGB_8888` and not a lower-depth config: the mesh gradients are smooth by construction and
 * `RGB_565` would band them visibly, which would make the mock look worse than the real thing
 * for reasons that have nothing to do with the real thing.
 */
fun ArgbImage.toBitmap(): Bitmap = Bitmap.createBitmap(pixels, width, height, Bitmap.Config.ARGB_8888)

/** The heap class assumed when the framework declines to answer — the platform minimum. */
private const val FALLBACK_MEMORY_CLASS_MB = 16
