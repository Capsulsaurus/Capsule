package com.justin13888.capsule.core.mock.seed

import java.security.MessageDigest
import java.util.Locale

/**
 * A content digest over an entire generated library.
 *
 * Determinism is the whole premise of this module, and a spot check does not test it: two
 * runs can agree on the first hundred assets and disagree on a camera serial four thousand
 * entries in, which is exactly the kind of drift that only shows up as an unexplained
 * screenshot diff months later. Hashing every record turns "is this reproducible?" into a
 * single value a test can compare and a reviewer can quote.
 *
 * The digest is fed incrementally rather than over one giant string: a forty-thousand-asset
 * library renders to tens of megabytes of text, and materialising that to hash it would make
 * the check expensive enough to be dropped.
 */
object LibraryDigest {
    /** The lowercase hex SHA-256 of every record in [library], in its canonical order. */
    fun of(library: MockLibrary): String {
        val digest = MessageDigest.getInstance("SHA-256")
        digest.feed(library.spec)
        // Order matters as much as content: two libraries holding the same assets in a
        // different timeline order lay out differently, so the digest must separate them.
        library.assets.forEach({ record -> digest.feed(record) })
        library.assets.forEach { digest.feed(library.exif[it.id]) }
        library.albums.forEach({ record -> digest.feed(record) })
        library.stacks.forEach { stack ->
            digest.feed(stack)
            library.stackMembers[stack.id]?.forEach({ record -> digest.feed(record) })
        }
        library.captions.forEach { (id, caption) ->
            digest.feed(id)
            digest.feed(caption)
        }
        library.tags.forEach { (id, tags) ->
            digest.feed(id)
            digest.feed(tags)
        }
        library.provenance.forEach { (id, chain) ->
            digest.feed(id)
            chain.forEach({ record -> digest.feed(record) })
        }
        library.people.forEach({ record -> digest.feed(record) })
        library.clusters.forEach({ record -> digest.feed(record) })
        library.smartAlbums.forEach({ record -> digest.feed(record) })
        library.quarantine.forEach({ record -> digest.feed(record) })
        library.devices.forEach({ record -> digest.feed(record) })
        library.sessions.forEach({ record -> digest.feed(record) })
        library.cohorts.forEach({ record -> digest.feed(record) })
        library.shareLinks.forEach({ record -> digest.feed(record) })
        library.uploadLinks.forEach({ record -> digest.feed(record) })
        library.drops.forEach({ record -> digest.feed(record) })
        library.moderationEvents.forEach({ record -> digest.feed(record) })
        library.scopes.forEach({ record -> digest.feed(record) })
        digest.feed(library.quota)
        digest.feed(library.landmarks)
        return digest.digest().joinToString("") { String.format(Locale.ROOT, "%02x", it) }
    }

    /**
     * Feeds one record's canonical rendering into the digest.
     *
     * `toString()` on a `data class` is generated from every constructor property in
     * declaration order, so it is a complete and stable rendering for exactly the types this
     * library is built from — and a field added to one of them changes the digest, which is
     * the behaviour a fixture fingerprint should have.
     */
    private fun MessageDigest.feed(value: Any?) {
        this.update(value.toString().toByteArray(Charsets.UTF_8))
        this.update(SEPARATOR)
    }

    /** Keeps two adjacent renderings from concatenating into a third, different one. */
    private val SEPARATOR = byteArrayOf(0x1E)
}
