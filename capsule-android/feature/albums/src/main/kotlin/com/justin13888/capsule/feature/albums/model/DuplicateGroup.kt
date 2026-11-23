package com.justin13888.capsule.feature.albums.model

import com.justin13888.capsule.core.model.Asset

/** How confident the finder is that a group really is the same photograph twice. */
enum class DuplicateKind {
    /**
     * The plaintext content hashes agree, so the files are byte-identical.
     *
     * The only case where discarding a copy is provably lossless, which is why it is
     * separated from the other one rather than folded into a single "duplicates" list.
     */
    Identical,

    /**
     * Same frame, different bytes — a re-export, a second copy at another quality.
     *
     * Presented as a *candidate*: the user decides, and nothing is deleted for them.
     */
    Similar,
}

/**
 * A set of assets the finder believes are the same photograph.
 *
 * [keeper] is a suggestion the user can move, not a decision already taken. Every
 * destructive step from here goes through the ordinary soft delete, so a wrong suggestion
 * costs a restore rather than a photograph.
 */
data class DuplicateGroup(
    /** A stable identity for the group, derived from its members so it survives a rescan. */
    val id: String,
    /** How sure the finder is. */
    val kind: DuplicateKind,
    /** The members, best-first: the suggested keeper leads. */
    val members: List<Asset>,
) {
    /** The asset the finder suggests keeping. */
    val keeper: Asset get() = members.first()

    /** Everything the user would be discarding if they took the suggestion. */
    val others: List<Asset> get() = members.drop(1)
}
