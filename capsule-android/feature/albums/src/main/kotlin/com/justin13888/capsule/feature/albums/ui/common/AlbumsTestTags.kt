package com.justin13888.capsule.feature.albums.ui.common

/**
 * The handles the screen tests drive this feature by.
 *
 * Named constants rather than literals at the assertion site so a renamed surface breaks the
 * compile rather than a test three files away, and so the tags themselves stay unique — two
 * screens that accidentally shared one would make a passing test meaningless.
 */
object AlbumsTestTags {
    /** The collections hub's scrolling body. */
    const val HUB = "albums.hub"

    /** One album row in the hub; suffixed with the album id. */
    const val HUB_ALBUM = "albums.hub.album"

    /** One section header in the hub; suffixed with the section's own key. */
    const val HUB_SECTION = "albums.hub.section"

    /** The album detail body. */
    const val DETAIL = "albums.detail"

    /** The member list on album detail. */
    const val MEMBERS = "albums.detail.members"

    /** One member row; suffixed with the member's account id. */
    const val MEMBER = "albums.detail.member"

    /** The read-only policy block on album detail. */
    const val POLICY = "albums.detail.policy"

    /** The upgrade ceremony block on album detail. */
    const val UPGRADE = "albums.detail.upgrade"

    /** The per-constituent banner list on an aggregated federated album. */
    const val FEDERATED = "albums.detail.federated"

    /** The smart album list. */
    const val SMART_LIST = "albums.smart.list"

    /** The smart album builder body. */
    const val BUILDER = "albums.builder"

    /** One node of the rule tree; suffixed with the draft node id. */
    const val BUILDER_NODE = "albums.builder.node"

    /** The builder's live match count. */
    const val BUILDER_COUNT = "albums.builder.count"

    /** The builder's list of structural refusals. */
    const val BUILDER_VIOLATIONS = "albums.builder.violations"

    /** The stacks list. */
    const val STACKS = "albums.stacks"

    /** The stack detail body. */
    const val STACK_DETAIL = "albums.stack"

    /** Recently Deleted. */
    const val TRASH = "albums.trash"

    /** One trashed item; suffixed with the asset id. */
    const val TRASH_ITEM = "albums.trash.item"

    /** The Hidden view. */
    const val HIDDEN = "albums.hidden"

    /** The duplicates view. */
    const val DUPLICATES = "albums.duplicates"

    /** The places view. */
    const val PLACES = "albums.places"

    /** The procedural map canvas inside the places view. */
    const val PLACES_MAP = "albums.places.map"
}
