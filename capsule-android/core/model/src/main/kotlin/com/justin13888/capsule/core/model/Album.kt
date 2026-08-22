package com.justin13888.capsule.core.model

import java.time.Instant

/**
 * Which of the two things the UI calls an "album" this one is.
 *
 * The design keeps them strictly separate and so does this type, because the difference is
 * not cosmetic: a [Container] is an MLS group holding a per-epoch key and owning its assets,
 * while a [View] holds no key, owns nothing, and is not a sharing boundary. Collapsing them
 * into one type would make "share this album" a question with no safe answer.
 */
enum class AlbumKind(
    override val wire: String,
) : WireEnum {
    /** The real cryptographic unit. Every asset belongs to exactly one. */
    Container("container"),

    /** A derived, key-free presentation computed client-side. Never an import destination. */
    View("view"),
    ;

    companion object : WireCodec<AlbumKind>({ entries })
}

/**
 * A member's capability on a container album.
 *
 * These are key tiers, not access-control rows: the capability *is* which keys MLS delivered
 * to that member's devices, so a client cannot grant itself a capability by editing local
 * state. A role change is an MLS commit and bumps the album's key epoch.
 */
enum class AlbumCapability(
    override val wire: String,
) : WireEnum {
    /** Album master key only — can decrypt and view. */
    Read("read"),

    /** Also the write-tier key — can add, edit, and delete assets. */
    Write("write"),

    /** Also the admin-tier key — can invite, remove, and change roles. */
    Admin("admin"),
    ;

    companion object : WireCodec<AlbumCapability>({ entries })
}

/** One member of a container album, with the capability MLS delivered to their devices. */
data class AlbumMember(
    /** The member's account. */
    val userId: UserId,
    /** What their devices hold keys for. */
    val capability: Wire<AlbumCapability>,
    /** When they accepted the MLS `Welcome`. */
    val joinedAt: Instant,
)

/**
 * A container or view album.
 *
 * The first four fields mirror `AlbumRecord` exactly; the rest are the product concepts the
 * catalog row does not carry.
 */
data class Album(
    /** `AlbumRecord.id`. */
    val id: AlbumId,
    /**
     * `AlbumRecord.name`.
     *
     * Empty for the default album, which is nameless by design — a client surfaces it as the
     * library's primary view rather than as a row called "Default".
     */
    val name: String,
    /** `AlbumRecord.created_at`. */
    val createdAt: Instant,
    /** `AlbumRecord.modified_at`. */
    val modifiedAt: Instant,
    /** `AlbumRecord.cover_asset_id`. */
    val coverAssetId: AssetId? = null,
    /** Container or view. */
    val kind: Wire<AlbumKind> = AlbumKind.Container.asWire(),
    /**
     * Whether this is the owner's **default album** — the de facto, nameless container that
     * receives any import the user does not file elsewhere.
     *
     * It is designated by a non-secret server-side owner pointer, not by anything in the
     * album itself, which is why this is a projection rather than an identity: re-designating
     * the default just moves the pointer.
     */
    val isDefault: Boolean = false,
    /** The viewer's own capability, when this is a container they are a member of. */
    val viewerCapability: Wire<AlbumCapability>? = null,
    /** Everyone the album is shared with. Empty for a view, which has no membership. */
    val members: List<AlbumMember> = emptyList(),
    /** How many assets the album holds, as last counted. */
    val assetCount: Int = 0,
) {
    /**
     * Whether the album can be deleted right now.
     *
     * The currently-designated default cannot be, and the reason is not politeness: import
     * always needs a home, so the user must repoint the default first. A client that deleted
     * it anyway would leave the next import with nowhere to land.
     */
    val isDeletable: Boolean get() = !isDefault
}

/**
 * Which resolution rule produced an import's destination album.
 *
 * The planner records this so a surprising destination is explainable after the fact —
 * "why did my screenshot end up there?" is a question the user can only answer if the rule
 * that fired was written down at the time.
 *
 * The order below is the resolution order, first match wins.
 */
enum class DefaultAlbumRule(
    override val wire: String,
) : WireEnum {
    /** The user picked an album explicitly at import time. */
    ExplicitPick("explicit_pick"),

    /** A `scope_id` override row matched this exact source. Deferred post-v1. */
    ScopeOverride("scope_override"),

    /** A per-[SourceKind] default row matched. Deferred post-v1. */
    SourceKindDefault("source_kind_default"),

    /** The owner's `default_album_id` pointer. */
    OwnerPointer("owner_pointer"),

    /** The de facto album whose id is derived from the account master key. */
    DerivedDefault("derived_default"),
    ;

    companion object : WireCodec<DefaultAlbumRule>({ entries })
}

/**
 * The outcome of `resolve_default_album(context)` — always a container, never a view.
 *
 * A view can never be an import destination, so this type cannot express one; that
 * constraint lives in the type rather than in a runtime check because the failure mode
 * (an import into something that owns no assets) has no sensible recovery.
 */
data class ResolvedDefaultAlbum(
    /** The container the import will land in. */
    val albumId: AlbumId,
    /** Which rule produced it. */
    val rule: DefaultAlbumRule,
)
