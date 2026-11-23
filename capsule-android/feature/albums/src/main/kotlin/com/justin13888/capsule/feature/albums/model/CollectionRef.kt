package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.AlbumId

/**
 * What the shell's single album destination is currently pointing at.
 *
 * The routing table offers one parameterised destination for "an album", carrying a bare
 * `String`. Collections holds four different things that all *present* as an album, though:
 * a container the user owns, a system view computed on the fly, the stacks listing, and an
 * aggregated federated album. Rather than ask the shell for three more destinations — which
 * would put feature vocabulary in the app module's sealed hierarchy — the id itself is given
 * a tiny, total grammar and parsed back at the feature boundary.
 *
 * A prefix scheme rather than JSON because the value travels through `rememberNavBackStack`'s
 * reflective serialization and through saved instance state; the shorter and more opaque it
 * is, the less there is to go wrong. Container ids are carried verbatim so the common case
 * costs nothing, and anything unrecognised parses back to a container — an id minted by a
 * newer build must still open *something* rather than crash the back stack.
 */
sealed interface CollectionRef {
    /** The `albumId` this ref travels as. */
    fun encode(): String

    /** One real container album: an MLS group with keys, membership, and assets of its own. */
    data class Container(
        val albumId: AlbumId,
    ) : CollectionRef {
        override fun encode(): String = albumId.value
    }

    /** A built-in view — key-free, owning nothing, recomputed from the local index. */
    data class SystemView(
        val kind: SystemViewKind,
    ) : CollectionRef {
        override fun encode(): String = VIEW_PREFIX + kind.token
    }

    /** The listing of every stack, which is a utility rather than an album. */
    data object Stacks : CollectionRef {
        override fun encode(): String = STACKS_TOKEN
    }

    /**
     * One aggregated federated album, addressed by its client-minted album-group id.
     *
     * Not an album id: the group has no server-side existence at all, so there is nothing
     * else it could be addressed by.
     */
    data class Federated(
        val groupId: String,
    ) : CollectionRef {
        override fun encode(): String = GROUP_PREFIX + groupId
    }

    companion object {
        private const val VIEW_PREFIX = "view:"
        private const val GROUP_PREFIX = "group:"
        private const val STACKS_TOKEN = "view:stacks"

        /**
         * Reads a ref back off the wire.
         *
         * Total by construction. An unknown `view:` token, or anything else the grammar does
         * not cover, falls back to a container: a destination that renders "album not found"
         * is recoverable, and one that throws inside the entry provider takes the whole back
         * stack with it.
         */
        fun parse(raw: String): CollectionRef =
            when {
                raw == STACKS_TOKEN -> Stacks
                raw.startsWith(GROUP_PREFIX) -> Federated(raw.removePrefix(GROUP_PREFIX))
                raw.startsWith(VIEW_PREFIX) ->
                    SystemViewKind
                        .fromToken(raw.removePrefix(VIEW_PREFIX))
                        ?.let(::SystemView)
                        ?: Container(AlbumId(raw))

                else -> Container(AlbumId(raw))
            }
    }
}
