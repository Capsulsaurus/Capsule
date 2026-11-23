package com.justin13888.capsule.feature.sharing

import com.justin13888.capsule.core.model.ShareLink

/**
 * The origin a share URL is built against — scheme and host, no trailing slash.
 *
 * Injected rather than derived from the link, because a link record carries no host: a share
 * is served only by its album's home server, and which server that is belongs to the session,
 * not to the link.
 */
@JvmInline
value class LinkOrigin(
    val value: String,
)

/** Where the share serve path lives under an origin. */
private const val SHARE_PATH = "/s/"

/**
 * The complete, working URL for [link], or `null` when this device cannot build one.
 *
 * **The secret after `#` is not decoration.** The fragment is the half a browser never
 * transmits, which is the whole reason the server can serve a share it cannot read — so a
 * URL without it is not a shorter link, it is a link that decrypts to nothing. Anything that
 * copies, renders or encodes a share URL must carry this string end to end; trimming it to
 * "tidy up" silently ships recipients a dead link, and nothing in the UI would look wrong.
 *
 * `null` is returned rather than a fragment-less URL for the same reason: the secret exists
 * exactly once, on the device that minted the link, and a link read back from the server can
 * never be completed here. Offering the visitor-facing prefix as if it were shareable would
 * be the same failure wearing a copy button.
 */
fun ShareLink.shareUrl(origin: LinkOrigin): String? {
    val secret = fragmentSecret ?: return null
    return origin.value + SHARE_PATH + opaqueId + "#" + secret
}

/**
 * A class of metadata the serve path always removes from a share.
 *
 * Modelled as a closed list the UI *reads*, never as settings the UI writes. A public share
 * is a boundary crossing by definition and the strip has no per-share opt-out, so a toggle
 * here would be a control that either did nothing or described the system incorrectly —
 * both worse than the plain statement of fact this enum exists to render.
 */
enum class StrippedField {
    /** The camera body's serial number, which links every frame it ever took. */
    CameraSerial,

    /** The device and session identifiers an import wrote. */
    DeviceAndSessionIds,

    /** The exact fix. A share serves a city-level position at best. */
    PreciseLocation,

    /** Names attached to faces, which identify people who never agreed to the share. */
    ContactTags,
    ;

    companion object {
        /** Everything the strip removes, in the order the disclosure lists it. */
        val ALL: List<StrippedField> = entries
    }
}
