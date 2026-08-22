package com.justin13888.capsule.core.mock.seed

import java.util.Locale

/**
 * Mints the identifiers a generated library needs, without ever calling `UUID.randomUUID()`.
 *
 * The domain's identifier rules are not decoration and the mock honours them, because a UI
 * built against ids that sort differently from the real ones will look right in a screenshot
 * and wrong in production. So an asset id is laid out as a genuine UUIDv7 — 48 bits of
 * Unix-millisecond prefix, version 7, variant 10 — and therefore sorts by capture time the
 * way `AssetRecord.uuid` does, while a device id is laid out as a UUIDv4 and carries no
 * ordering at all, exactly as [com.justin13888.capsule.core.model.DeviceId] documents.
 *
 * Every byte below the version nibble comes from a [Prng], so the whole set is a pure
 * function of the seed.
 */
object DeterministicIds {
    /**
     * A UUIDv7 string whose time prefix is [unixMillis].
     *
     * Two ids minted for the same millisecond differ in their random tail, so a burst of
     * twenty frames captured inside one millisecond still gets twenty distinct, ordered ids.
     */
    fun uuidV7(
        unixMillis: Long,
        prng: Prng,
    ): String {
        val bytes = ByteArray(UUID_BYTES)
        val timestamp = unixMillis and TIMESTAMP_MASK
        for (index in 0 until TIMESTAMP_BYTES) {
            val shift = (TIMESTAMP_BYTES - 1 - index) * Byte.SIZE_BITS
            bytes[index] = ((timestamp ushr shift) and BYTE_MASK).toByte()
        }
        fillRandom(bytes, from = TIMESTAMP_BYTES, prng = prng)
        stampVersion(bytes, version = 7)
        return format(bytes)
    }

    /**
     * A UUIDv4 string.
     *
     * Used where the domain deliberately forbids a time-ordered id — a device id must not
     * leak when the device was enrolled — so the mock must not quietly hand out a v7 there.
     */
    fun uuidV4(prng: Prng): String {
        val bytes = ByteArray(UUID_BYTES)
        fillRandom(bytes, from = 0, prng = prng)
        stampVersion(bytes, version = 4)
        return format(bytes)
    }

    /**
     * A lowercase 64-character hex digest, standing in for a SHA-256 over content.
     *
     * The mock never hashes real bytes — there are none — so this is a *shaped* stand-in
     * rather than a computed one: the same length and alphabet a real `hash_sha256` has, so
     * a UI that truncates or monospaces it lays out identically against real data.
     */
    fun hexDigest(prng: Prng): String {
        val builder = StringBuilder(DIGEST_HEX_LENGTH)
        repeat(DIGEST_HEX_LENGTH / LONG_HEX_LENGTH) {
            builder.append(String.format(Locale.ROOT, "%016x", prng.nextLong()))
        }
        return builder.toString()
    }

    /** A lowercase hex string of [byteCount] bytes — an opaque link id, a cohort hash. */
    fun hexToken(
        byteCount: Int,
        prng: Prng,
    ): String {
        val builder = StringBuilder(byteCount * 2)
        repeat(byteCount) { builder.append(String.format(Locale.ROOT, "%02x", prng.nextInt(BYTE_VALUES))) }
        return builder.toString()
    }

    private fun fillRandom(
        bytes: ByteArray,
        from: Int,
        prng: Prng,
    ) {
        var index = from
        while (index < bytes.size) {
            var word = prng.nextLong()
            var remaining = Long.SIZE_BYTES
            while (remaining > 0 && index < bytes.size) {
                bytes[index] = (word and BYTE_MASK).toByte()
                word = word ushr Byte.SIZE_BITS
                remaining--
                index++
            }
        }
    }

    /** Writes the version nibble and the RFC 4122 variant bits in place. */
    private fun stampVersion(
        bytes: ByteArray,
        version: Int,
    ) {
        bytes[VERSION_BYTE] = ((bytes[VERSION_BYTE].toInt() and LOW_NIBBLE) or (version shl 4)).toByte()
        bytes[VARIANT_BYTE] = ((bytes[VARIANT_BYTE].toInt() and VARIANT_CLEAR) or VARIANT_SET).toByte()
    }

    private fun format(bytes: ByteArray): String {
        val hex = StringBuilder(UUID_TEXT_LENGTH)
        bytes.forEachIndexed { index, byte ->
            if (index in DASH_AFTER) hex.append('-')
            hex.append(String.format(Locale.ROOT, "%02x", byte.toInt() and BYTE_INT_MASK))
        }
        return hex.toString()
    }

    private const val UUID_BYTES = 16
    private const val UUID_TEXT_LENGTH = 36
    private const val TIMESTAMP_BYTES = 6
    private const val TIMESTAMP_MASK = 0xFFFF_FFFF_FFFFL
    private const val BYTE_MASK = 0xFFL
    private const val BYTE_INT_MASK = 0xFF
    private const val BYTE_VALUES = 256
    private const val LOW_NIBBLE = 0x0F
    private const val VARIANT_CLEAR = 0x3F
    private const val VARIANT_SET = 0x80
    private const val VERSION_BYTE = 6
    private const val VARIANT_BYTE = 8
    private const val DIGEST_HEX_LENGTH = 64
    private const val LONG_HEX_LENGTH = 16
    private val DASH_AFTER = setOf(4, 6, 8, 10)
}
