package com.justin13888.capsule.feature.people

import com.justin13888.capsule.core.model.PersonId
import java.nio.ByteBuffer
import java.security.SecureRandom

/** Bytes in a UUID. */
private const val UUID_BYTES = 16

/** Where the 48-bit big-endian millisecond timestamp ends. */
private const val TIMESTAMP_BYTES = 6

private const val BYTE_MASK = 0xFF

private const val VERSION_BYTE = 6

private const val VARIANT_BYTE = 8

private val RANDOM = SecureRandom()

/**
 * Mints the id for a person the user is creating.
 *
 * Client-minted ids are the exception in this system — the catalog mints everything else at
 * import — but `PeopleRepository.assignCluster` takes the person's id as an argument and has
 * no create call of its own, so the id has to exist before the write. UUIDv7 rather than v4
 * because a person is ordinary user content whose creation order is not sensitive, and v7's
 * time prefix keeps the eventual database index from fragmenting.
 *
 * Layout per RFC 9562 §5.7: 48 bits of big-endian Unix milliseconds, the version nibble `7`,
 * the variant bits `10`, and randomness for the rest.
 */
fun mintPersonId(nowMillis: Long = System.currentTimeMillis()): PersonId {
    val bytes = ByteArray(UUID_BYTES).also(RANDOM::nextBytes)
    for (index in 0 until TIMESTAMP_BYTES) {
        bytes[index] = ((nowMillis shr ((TIMESTAMP_BYTES - 1 - index) * Byte.SIZE_BITS)) and 0xFF).toByte()
    }
    bytes[VERSION_BYTE] = ((bytes[VERSION_BYTE].toInt() and 0x0F) or 0x70).toByte()
    bytes[VARIANT_BYTE] = ((bytes[VARIANT_BYTE].toInt() and 0x3F) or 0x80).toByte()
    val buffer = ByteBuffer.wrap(bytes)
    return PersonId(java.util.UUID(buffer.long, buffer.long).toString())
}

/** The unsigned value of one byte of a minted id, for the tests that assert its shape. */
internal fun Byte.unsigned(): Int = toInt() and BYTE_MASK
