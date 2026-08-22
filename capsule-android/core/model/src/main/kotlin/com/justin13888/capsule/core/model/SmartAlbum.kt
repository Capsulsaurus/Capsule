package com.justin13888.capsule.core.model

import java.time.Instant

/**
 * The type class a [QueryField] belongs to.
 *
 * The class, not the field, decides which [Operator]s are legal and what shape the operand
 * takes. Grouping fields this way is what keeps the grammar small enough to validate
 * exhaustively: eleven fields share six rule sets rather than needing eleven of their own.
 */
enum class QueryFieldClass {
    /** An instant. Compared with `before` / `after` / `in_range` against RFC 3339 values. */
    Temporal,

    /** A value from the field's own closed enum. Compared with `eq` / `in`. */
    Enumerated,

    /** An unsigned integer. Compared with `eq` / `gte` / `lte` / `in_range`. */
    Numeric,

    /** A trinary or boolean flag. Compared with `is` against the field's own literal. */
    TrinaryOrBool,

    /** A set of strings or ids. Compared with the `contains` family. */
    SetValued,

    /** A field that is either present or not. Compared with `exists`. */
    Presence,
}

/**
 * A field a smart-album predicate may query — a closed set, one type class each.
 *
 * A term naming a field outside this set is a **structural rejection** at the validator,
 * never a future value to tolerate. That asymmetry is deliberate: a definition is stored
 * data that syncs to every one of the owner's devices and must evaluate identically on all
 * of them, so a device that silently skipped a term it did not understand would compute a
 * different membership than its peers and break the convergence guarantee.
 *
 * Adding a field is a new, later-dated `predicate_schema`.
 */
enum class QueryField(
    override val wire: String,
    val fieldClass: QueryFieldClass,
) : WireEnum {
    /** When the shot was taken. */
    CaptureTimestamp("capture_timestamp", QueryFieldClass.Temporal),

    /** When the file entered the library. */
    ImportTimestamp("import_timestamp", QueryFieldClass.Temporal),

    /** The precise format — a [ContentType]. */
    ContentType("content_type", QueryFieldClass.Enumerated),

    /** Image or video, derived from the content type — a [MediaKind]. */
    MediaKind("media_kind", QueryFieldClass.Enumerated),

    /** The datum a stored fix is expressed in — a [GpsDatum]. */
    GpsDatum("gps.datum", QueryFieldClass.Enumerated),

    /** Stars, `0..5`. */
    Rating("rating", QueryFieldClass.Numeric),

    /** Pixel width. */
    DimensionsWidth("dimensions.width", QueryFieldClass.Numeric),

    /** Pixel height. */
    DimensionsHeight("dimensions.height", QueryFieldClass.Numeric),

    /** The trinary culling flag — `pick | neutral | reject`. */
    Cull("cull", QueryFieldClass.TrinaryOrBool),

    /** The visibility flag — `true | false`. */
    Hidden("hidden", QueryFieldClass.TrinaryOrBool),

    /** The user-written tag set. */
    TagsUser("tags_user", QueryFieldClass.SetValued),

    /** The model-suggested tag set. Requires a [Term.modelSlot]. */
    TagsAi("tags_ai", QueryFieldClass.SetValued),

    /** The stack types an asset participates in. */
    StackType("stack_type", QueryFieldClass.SetValued),

    /** The face clusters an asset appears in. Requires a [Term.modelSlot]. */
    PeopleCluster("people_cluster", QueryFieldClass.SetValued),

    /** The container album an asset lives in. */
    AlbumId("album_id", QueryFieldClass.SetValued),

    /** Whether a GPS fix is stored at all. */
    Gps("gps", QueryFieldClass.Presence),

    /** Whether a camera identity is stored at all. */
    CameraId("camera_id", QueryFieldClass.Presence),
    ;

    /**
     * Whether a term over this field must name the model slot it queries.
     *
     * True for the two AI-derived fields, because a term that did not name its slot would
     * have to compare across model versions to evaluate — which is forbidden, since the
     * vector spaces differ and the comparison is meaningless.
     */
    val requiresModelSlot: Boolean get() = this == TagsAi || this == PeopleCluster

    companion object : WireCodec<QueryField>({ entries })
}

/**
 * A comparison a term may apply — a closed set; an operator outside it is a structural
 * rejection, and an operator outside its field's [QueryFieldClass] is too.
 */
enum class Operator(
    override val wire: String,
    val appliesTo: Set<QueryFieldClass>,
) : WireEnum {
    /** Strictly before an instant. */
    Before("before", setOf(QueryFieldClass.Temporal)),

    /** Strictly after an instant. */
    After("after", setOf(QueryFieldClass.Temporal)),

    /** Within a range — half-open `[start, end)` for temporal, inclusive `[lo, hi]` for numeric. */
    InRange("in_range", setOf(QueryFieldClass.Temporal, QueryFieldClass.Numeric)),

    /** Exactly equal to one value. */
    Eq("eq", setOf(QueryFieldClass.Enumerated, QueryFieldClass.Numeric)),

    /** A member of a set of enum values. */
    In("in", setOf(QueryFieldClass.Enumerated)),

    /** Greater than or equal to. */
    Gte("gte", setOf(QueryFieldClass.Numeric)),

    /** Less than or equal to. */
    Lte("lte", setOf(QueryFieldClass.Numeric)),

    /** Equal to the field's own literal. */
    Is("is", setOf(QueryFieldClass.TrinaryOrBool)),

    /** The field's set contains every listed member — a one-member `contains_all`. */
    Contains("contains", setOf(QueryFieldClass.SetValued)),

    /** The field's set intersects the listed members. */
    ContainsAny("contains_any", setOf(QueryFieldClass.SetValued)),

    /** The field's set is a superset of the listed members. */
    ContainsAll("contains_all", setOf(QueryFieldClass.SetValued)),

    /** Whether the field is present, or absent. */
    Exists("exists", setOf(QueryFieldClass.Presence)),
    ;

    companion object : WireCodec<Operator>({ entries })
}

/**
 * A typed literal, whose shape is fixed by the `(field, op)` pair it appears with.
 *
 * An operand mistyped for its pair is a structural rejection. Modelling the shapes as a
 * closed sealed hierarchy rather than a loose JSON value means most mistypings are caught by
 * the compiler at construction, and the rest by one exhaustive `when` in the validator.
 */
sealed interface Operand {
    /** A single instant, for `before` / `after`. */
    data class TemporalPoint(
        val at: Instant,
    ) : Operand

    /** A half-open `[start, endExclusive)` window, for temporal `in_range`. */
    data class TemporalRange(
        val start: Instant,
        val endExclusive: Instant,
    ) : Operand

    /**
     * One value of the field's own closed enum, for `eq` on an enumerated field.
     *
     * Held as its raw token so a value from a newer `predicate_schema` survives the
     * round-trip; the validator is what refuses it, not the parser.
     */
    data class EnumValue(
        val raw: String,
    ) : Operand

    /** A set of values of the field's own closed enum, for `in`. */
    data class EnumSet(
        val raw: List<String>,
    ) : Operand

    /** An unsigned integer, for numeric `eq` / `gte` / `lte`. */
    data class Number(
        val value: Long,
    ) : Operand

    /** An inclusive `[lo, hi]` pair, for numeric `in_range`. */
    data class NumberRange(
        val lo: Long,
        val hi: Long,
    ) : Operand

    /** A culling flag, for `is` on `cull`. */
    data class CullLiteral(
        val flag: CullFlag,
    ) : Operand

    /** A boolean, for `is` on `hidden`. */
    data class BoolLiteral(
        val value: Boolean,
    ) : Operand

    /** A bounded set of strings or ids, for the `contains` family. */
    data class StringSet(
        val members: List<String>,
    ) : Operand

    /** Whether the field must be present, for `exists`. */
    data class Presence(
        val mustExist: Boolean,
    ) : Operand
}

/**
 * One comparison in a predicate.
 *
 * [field] and [op] are [Wire] values rather than bare constants because a definition
 * authored on a newer client can name tokens this build does not know. Representing them is
 * required — the definition must survive a sync round-trip verbatim rather than being
 * stripped — while acting on them is refused by the validator.
 */
data class Term(
    /** The field being queried. */
    val field: Wire<QueryField>,
    /** The comparison. Must be valid for the field's [QueryFieldClass]. */
    val op: Wire<Operator>,
    /** The literal. Its shape is fixed by `(field, op)`. */
    val operand: Operand,
    /**
     * The `(model_id, model_version)` slot this term queries.
     *
     * Required for `tags_ai` and `people_cluster`, forbidden elsewhere. A term over a slot
     * whose canonical model has changed evaluates as **stale-excluded** until the values are
     * regenerated — never compared across model versions.
     */
    val modelSlot: ModelSlot? = null,
)

/**
 * The predicate tree — a closed, declarative grammar carrying no code, no regex, and no
 * unbounded input.
 *
 * Declarative is the whole point: a definition is stored data that syncs to every one of the
 * owner's devices, so it must evaluate identically everywhere. Anything executable would
 * evaluate differently on a client with a different runtime, and anything unbounded would let
 * a synced document turn every one of the user's devices into a denial-of-service target.
 *
 * Bounds are [MAX_DEPTH] and [MAX_TERMS], enforced before evaluation.
 */
sealed interface Predicate {
    /** Conjunction. An empty [All] matches **every** asset. */
    data class All(
        val operands: List<Predicate>,
    ) : Predicate

    /** Disjunction. An empty [Any] matches **no** asset. */
    data class Any(
        val operands: List<Predicate>,
    ) : Predicate

    /** Negation. */
    data class Not(
        val operand: Predicate,
    ) : Predicate

    /** A leaf comparison. */
    data class Leaf(
        val term: Term,
    ) : Predicate

    companion object {
        /** The maximum nesting depth of the tree; a leaf is depth 1. */
        const val MAX_DEPTH = 8

        /** The maximum number of [Leaf] terms anywhere in the tree. */
        const val MAX_TERMS = 64

        /** The maximum number of members in a set-valued or enum-set operand. */
        const val MAX_SET_MEMBERS = 64

        /** The maximum encoded length of one set member. */
        const val MAX_SET_MEMBER_BYTES = 256
    }
}

/** The key a smart album's results are ordered by. */
enum class SortKey(
    override val wire: String,
) : WireEnum {
    /** When the shot was taken — the default. */
    CaptureTimestamp("capture_timestamp"),

    /** When the file entered the library. */
    ImportTimestamp("import_timestamp"),

    /** Stars. */
    Rating("rating"),
    ;

    companion object : WireCodec<SortKey>({ entries })
}

/** Ascending or descending. */
enum class SortDirection(
    override val wire: String,
) : WireEnum {
    /** Oldest or smallest first. */
    Ascending("asc"),

    /** Newest or largest first — the default. */
    Descending("desc"),
    ;

    companion object : WireCodec<SortDirection>({ entries })
}

/** How a smart album's results are ordered. Defaults to newest capture first. */
data class SortSpec(
    /** What to order by. */
    val key: Wire<SortKey> = SortKey.CaptureTimestamp.asWire(),
    /** Which way. */
    val direction: Wire<SortDirection> = SortDirection.Descending.asWire(),
)

/**
 * A user-defined smart album — one entry of the library-settings document's `smart_albums`
 * map, stored as the value of a single LWW register.
 *
 * Being one register is what makes authoring or editing a smart album a single stamped write
 * that converges: there is never a partial-predicate merge, so two devices editing the same
 * definition cannot produce a tree neither of them wrote.
 *
 * **Membership is computed, never stored.** Evaluation is a pure function of the definition
 * and the assets the viewer can decrypt, processed in sorted asset-id order, so recomputation
 * is idempotent and identical across devices. Editing a definition, or an asset's attributes,
 * moves and re-encrypts nothing.
 */
data class SmartAlbumDefinition(
    /** The map key this definition is stored under — UUIDv7. */
    val id: SmartAlbumId,
    /**
     * The closed-grammar version.
     *
     * A value above the reader's maximum gates evaluation: the definition is preserved
     * verbatim and surfaced as "created by a newer app version", never evaluated and never
     * stripped.
     */
    val predicateSchema: Int,
    /** The user-visible name, converging like a caption. Bounded at [MAX_DISPLAY_NAME_BYTES]. */
    val displayName: Lww<String>,
    /** The tree evaluated against every decryptable asset. */
    val predicate: Predicate,
    /** Ordering; defaults to newest capture first when absent. */
    val sort: SortSpec? = null,
) {
    companion object {
        /** The highest `predicate_schema` this build can evaluate. */
        const val MAX_KNOWN_PREDICATE_SCHEMA = 1

        /** The cap on a display name's encoded length. */
        const val MAX_DISPLAY_NAME_BYTES = 256
    }
}
