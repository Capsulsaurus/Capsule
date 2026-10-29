package com.justin13888.capsule.screens.mock

import com.justin13888.capsule.screens.domain.AlbumId
import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.BlobHash
import com.justin13888.capsule.screens.domain.RegionId
import com.justin13888.capsule.screens.domain.StackId
import com.justin13888.capsule.screens.domain.UploadId

/**
 * Deterministic identifier minting for the mock lane: every id is a pure function of
 * `(seed, kind, ordinal)`, shaped like the real thing (UUIDv7 text for the time-ordered
 * kinds, 32 bytes of hex for a content address), so the same seed builds the same world
 * and a test can name what it expects. No random UUID anywhere.
 */
object MockIdentifiers {
    private enum class Namespace(
        val tag: Long,
    ) {
        ASSET(1),
        STACK(5),
        ALBUM(6),
        UPLOAD(10),
        BLOB(12),
        REGION(15),
    }

    fun assetId(
        seed: Long,
        ordinal: Int,
    ): AssetId = AssetId(timeOrdered(seed, Namespace.ASSET, ordinal))

    fun stackId(
        seed: Long,
        ordinal: Int,
    ): StackId = StackId(timeOrdered(seed, Namespace.STACK, ordinal))

    fun albumId(
        seed: Long,
        ordinal: Int,
    ): AlbumId = AlbumId(timeOrdered(seed, Namespace.ALBUM, ordinal))

    fun uploadId(
        seed: Long,
        ordinal: Int,
    ): UploadId = UploadId(timeOrdered(seed, Namespace.UPLOAD, ordinal))

    fun regionId(
        seed: Long,
        ordinal: Int,
    ): RegionId = RegionId(timeOrdered(seed, Namespace.REGION, ordinal))

    /** A content address: 64 lowercase hex digits, the length of a SHA-256 digest. */
    fun blobHash(
        seed: Long,
        ordinal: Int,
    ): BlobHash {
        val first = mix(seed xor (Namespace.BLOB.tag * GOLDEN))
        return BlobHash((0 until 4).joinToString("") { step -> hex(mix(first + ordinal * 4L + step), 16) })
    }

    /** A UUIDv7-shaped string: version nibble 7, RFC 4122 variant, derived entropy. */
    private fun timeOrdered(
        seed: Long,
        namespace: Namespace,
        ordinal: Int,
    ): String {
        val first = mix(mix(seed xor (namespace.tag * GOLDEN)) + ordinal)
        val second = mix(first)
        return listOf(
            hex(first ushr 32, 8),
            hex((first ushr 16) and 0xFFFF, 4),
            hex((first and 0x0FFF) or 0x7000, 4),
            hex(((second ushr 48) and 0x3FFF) or 0x8000, 4),
            hex(second and 0xFFFF_FFFF_FFFFL, 12),
        ).joinToString("-")
    }

    /** SplitMix64's finalizer: a bijective scramble of 64 bits. */
    private fun mix(input: Long): Long {
        var z = input + GOLDEN
        z = (z xor (z ushr 30)) * -0x40a7b892e31b1a47L
        z = (z xor (z ushr 27)) * -0x6b2fb644ecceee15L
        return z xor (z ushr 31)
    }

    private fun hex(
        value: Long,
        digits: Int,
    ): String = java.lang.Long.toHexString(value).padStart(16, '0').takeLast(digits)

    private const val GOLDEN = -0x61c8864680b583ebL
}
