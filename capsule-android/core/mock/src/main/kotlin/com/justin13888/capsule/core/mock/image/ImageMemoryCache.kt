package com.justin13888.capsule.core.mock.image

/**
 * A byte-budgeted least-recently-used cache of rendered images.
 *
 * Synthesis is cheap but not free — a 256 px tile is sixty-five thousand bicubic samples —
 * and a gallery fling asks for the same tiles again within a second or two, so a cache is
 * what keeps the mock's scroll indistinguishable from a real one. The budget is in **bytes
 * rather than entries** because entry counts say nothing useful when a full-screen preview
 * costs two hundred times a grid tile.
 *
 * Backed by an access-ordered `LinkedHashMap`, which gives both the LRU ordering and — unlike
 * a `HashMap` — a deterministic iteration order, so eviction cannot depend on hash-bucket
 * layout and a test can assert exactly which entry left.
 */
class ImageMemoryCache(
    /** The ceiling on the sum of every held image's [ArgbImage.byteCount]. */
    val maxBytes: Int,
) {
    private val entries = LinkedHashMap<ProceduralImageRequest, ArgbImage>(INITIAL_CAPACITY, LOAD_FACTOR, true)
    private var heldBytes = 0

    /** Bytes currently held. */
    val bytesInUse: Int get() = synchronized(entries) { heldBytes }

    /** How many images are held. */
    val size: Int get() = synchronized(entries) { entries.size }

    /**
     * The cached image for [request], or the result of [render] stored under it.
     *
     * A single get-or-render entry point rather than a separate `get` and `put`: two calls
     * invite a caller to render, forget to store, and quietly lose the whole benefit.
     */
    fun getOrRender(
        request: ProceduralImageRequest,
        render: (ProceduralImageRequest) -> ArgbImage,
    ): ArgbImage {
        synchronized(entries) { entries[request] }?.let { return it }
        val rendered = render(request)
        synchronized(entries) {
            if (entries.put(request, rendered) == null) {
                heldBytes += rendered.byteCount
            }
            evictDownToBudget()
        }
        return rendered
    }

    /** Drops everything — what a memory-pressure callback calls. */
    fun clear() {
        synchronized(entries) {
            entries.clear()
            heldBytes = 0
        }
    }

    /** Evicts from the least-recently-used end until the budget is met. */
    private fun evictDownToBudget() {
        val iterator = entries.entries.iterator()
        while (heldBytes > maxBytes && iterator.hasNext()) {
            val entry = iterator.next()
            heldBytes -= entry.value.byteCount
            iterator.remove()
        }
    }

    companion object {
        /**
         * The budget for a device whose `ActivityManager.memoryClass` is [memoryClassMb].
         *
         * An eighth of the per-process heap. Android's own guidance for a bitmap cache is a
         * seventh to an eighth, and the mock deliberately takes the low end: it is a
         * development fixture sitting underneath a real app, and a fixture that provokes the
         * `OutOfMemoryError` it was meant to help debug is worse than useless.
         */
        fun forMemoryClass(memoryClassMb: Int): ImageMemoryCache {
            val heapBytes = memoryClassMb.coerceAtLeast(MIN_MEMORY_CLASS_MB).toLong() * BYTES_PER_MB
            val budget = (heapBytes / HEAP_FRACTION).coerceIn(MIN_BUDGET_BYTES, MAX_BUDGET_BYTES)
            return ImageMemoryCache(budget.toInt())
        }

        /** The budget used when no device is available to ask — tests, previews, the JVM. */
        const val DEFAULT_BUDGET_BYTES = 8 * 1024 * 1024

        private const val INITIAL_CAPACITY = 64
        private const val LOAD_FACTOR = 0.75f
        private const val BYTES_PER_MB = 1024L * 1024L
        private const val HEAP_FRACTION = 8L
        private const val MIN_MEMORY_CLASS_MB = 16
        private const val MIN_BUDGET_BYTES = 4L * 1024 * 1024
        private const val MAX_BUDGET_BYTES = 96L * 1024 * 1024
    }
}
