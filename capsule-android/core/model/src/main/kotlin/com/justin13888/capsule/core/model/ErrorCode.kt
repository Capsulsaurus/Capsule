package com.justin13888.capsule.core.model

/**
 * A stable `error.*` key from the canonical message catalogue.
 *
 * Clients localize the **code**, never a raw HTTP status: a status tells the user nothing
 * they can act on, and the English detail message that travels beside the code stays English
 * on purpose so it remains greppable in a support bundle. The code is the contract; the
 * wording behind it can change without breaking a client.
 */
@JvmInline
value class ErrorCode(
    val value: String,
)
