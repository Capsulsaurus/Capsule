package com.justin13888.capsule.core.model

/**
 * A closed enum whose authoritative value set is owned by a design doc, together with the
 * canonical token it takes on the wire.
 *
 * UniFFI passes every enum-typed catalog field across the boundary as a plain `String`
 * carrying its canonical snake_case value (`"photo"`, `"live_photo"`, `"offset_exif"`, …),
 * so the Kotlin side is the first place the value is interpreted at all. Declaring the
 * token on the enum rather than deriving it from the constant name keeps the two free to
 * differ — `Jpeg` is `image/jpeg`, `SemanticSearch` is kebab-cased `semantic-search` — and
 * makes the mapping something a test can assert rather than something a renaming refactor
 * can silently break.
 */
interface WireEnum {
    /** The canonical token this value takes on the wire. */
    val wire: String
}

/**
 * A wire value that either resolved to a known [WireEnum] constant or did not.
 *
 * The design's closed-enum rule cuts two ways and this type is what lets a client honour
 * both at once. Unknown values are **structural errors**: a `stack_type` of
 * `"future-stack-type"` is rejected, never quietly treated as a future value to tolerate.
 * But unknown data is also **never dropped** — stripping a field a newer peer wrote would
 * invalidate its signature and lose the user's state. So the parse keeps the raw token in
 * [Unknown] instead of collapsing it to `null` or to a catch-all constant: callers that
 * need a decision get a compiler-checked `when` that forces them to handle the unknown arm,
 * and callers that merely carry the value forward re-emit exactly what they received.
 */
sealed interface Wire<out E : WireEnum> {
    /** The token as it appeared on the wire, known or not. */
    val raw: String

    /** A token that matched a constant of the closed set. */
    data class Known<E : WireEnum>(
        val value: E,
    ) : Wire<E> {
        override val raw: String get() = value.wire
    }

    /**
     * A token outside the closed set — written by a newer peer, or malformed.
     *
     * Preserved verbatim so it survives a round-trip; refused wherever the value has to
     * drive behaviour.
     */
    data class Unknown(
        override val raw: String,
    ) : Wire<Nothing>
}

/** The constant this value resolved to, or `null` when the token was outside the set. */
val <E : WireEnum> Wire<E>.knownOrNull: E?
    get() =
        when (this) {
            is Wire.Known -> value
            is Wire.Unknown -> null
        }

/** Whether the token resolved to a constant of the closed set. */
val <E : WireEnum> Wire<E>.isKnown: Boolean
    get() = this is Wire.Known

/** Lifts a constant into a [Wire] value, for building records to send back across the boundary. */
fun <E : WireEnum> E.asWire(): Wire<E> = Wire.Known(this)

/**
 * The wire codec for one closed enum, mixed into its companion object.
 *
 * Constants are supplied as a lambda rather than a value so the lookup table is built on
 * first use: an enum's companion object is initialised inside the enum's own static
 * initialiser, and reading `entries` eagerly from there is exactly the ordering hazard that
 * produces a mysterious empty map. Deferring it sidesteps the question entirely.
 */
open class WireCodec<E : WireEnum>(
    private val constants: () -> Iterable<E>,
) {
    private val byWire: Map<String, E> by lazy { constants().associateBy(WireEnum::wire) }

    /** Every constant of the closed set, keyed by its wire token. */
    val allByWire: Map<String, E> get() = byWire

    /**
     * Resolves [raw] against the closed set, preserving it as [Wire.Unknown] when it is not
     * a member. Never throws — refusal is the caller's decision to make, at the point where
     * the value would drive behaviour.
     */
    fun fromWire(raw: String): Wire<E> = byWire[raw]?.let { Wire.Known(it) } ?: Wire.Unknown(raw)

    /** Resolves [raw] to a constant, or `null` when it is outside the closed set. */
    fun fromWireOrNull(raw: String): E? = byWire[raw]

    /**
     * Resolves [raw] to a constant, or throws.
     *
     * For the boundaries where an unknown value genuinely cannot be carried forward — a
     * validator rejecting a malformed definition rather than a reader tolerating a newer
     * peer.
     */
    fun fromWireOrThrow(raw: String): E = byWire[raw] ?: throw IllegalArgumentException(rejected(raw))

    private fun rejected(raw: String) = "'$raw' is not a value of this closed enum"
}
