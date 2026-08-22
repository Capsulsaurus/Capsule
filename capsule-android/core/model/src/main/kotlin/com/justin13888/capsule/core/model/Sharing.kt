package com.justin13888.capsule.core.model

import java.time.Instant

/** What a share link grants read access to. */
sealed interface ShareScope {
    /** View-only access to one asset. */
    data class SingleAsset(
        val assetId: AssetId,
    ) : ShareScope

    /** View-only access to a whole album. */
    data class WholeAlbum(
        val albumId: AlbumId,
    ) : ShareScope
}

/**
 * A link handed to someone who is not a member of the album.
 *
 * There is deliberately **no state enum** here. Not-found, revoked, and expired are
 * indistinguishable to a visitor by design — all three answer the same way — so a client
 * that modelled them as three states would be inventing a distinction the server refuses to
 * make, and one that leaks whether a link ever existed.
 *
 * The read secret never reaches the server: it lives in the URL fragment, which browsers do
 * not transmit. That is why [fragmentSecret] is present on an issued link and absent from
 * anything the server can hand back.
 */
data class ShareLink(
    /** The issuer-side handle. Never appears in the URL. */
    val id: ShareLinkId,
    /**
     * The 128-bit random identifier that does appear in the URL.
     *
     * Fully opaque and carrying no scope information: a shorter or sequential identifier
     * would let an attacker enumerate other people's links.
     */
    val opaqueId: String,
    /** What the link grants. */
    val scope: ShareScope,
    /** When it stops working; `null` for no expiry. */
    val expiresAt: Instant? = null,
    /** When it was issued. */
    val createdAt: Instant,
    /** Set once revoked. Revocation and expiry look identical to a visitor. */
    val revokedAt: Instant? = null,
    /** Whether opening it requires a passphrase, unwrapped entirely client-side. */
    val requiresPassphrase: Boolean = false,
    /** The fragment half of the URL. Present only on a link this device just issued. */
    val fragmentSecret: String? = null,
) {
    /**
     * Whether the link would serve at [now].
     *
     * Fail-closed: revoked wins, and a missing expiry means "no expiry", never "expired".
     */
    fun isLiveAt(now: Instant): Boolean = revokedAt == null && (expiresAt == null || now.isBefore(expiresAt))
}

/**
 * The bounds an upload link is issued under.
 *
 * Every bound is optional except [singleUse], and an absent bound means unbounded — the
 * link owner is the one paying for the bytes, so the design leaves the ceiling to them
 * rather than imposing one. Note there is no allowed-content-type list here: content type is
 * a closed enum enforced per deposited file, not a per-link cap.
 */
data class LinkCaps(
    /** When the link stops accepting deposits. */
    val expiresAt: Instant? = null,
    /** Ceiling on the sum of all deposits. */
    val maxTotalBytes: Long? = null,
    /** Ceiling on the number of deposited files. */
    val maxFileCount: Int? = null,
    /** Ceiling on any one file. */
    val maxFileSize: Long? = null,
    /** Whether the link closes after one successful deposit session. */
    val singleUse: Boolean = false,
)

/**
 * A link that lets someone without an account deposit files into the issuer's inbox.
 *
 * The passphrase on an upload link is an **abuse gate, not confidentiality** — the opposite
 * of a share link's. The server holds a verifier and the guest submits a proof; there is no
 * secret being unwrapped, because there is nothing to read. Modelling the two with one type
 * would invite exactly the confusion that leads to a passphrase being sent to a server that
 * should never see it.
 */
data class UploadLink(
    /** The issuer-side handle. */
    val id: UploadLinkId,
    /** The opaque 128-bit identifier that appears in the URL. */
    val opaqueId: String,
    /** The bounds the link was issued under. */
    val caps: LinkCaps,
    /** When it was issued. */
    val createdAt: Instant,
    /** Set once revoked. */
    val revokedAt: Instant? = null,
    /** Whether a guest must present a passphrase proof to open a deposit session. */
    val requiresPassphrase: Boolean = false,
    /** Bytes deposited so far, against [LinkCaps.maxTotalBytes]. */
    val bytesReceived: Long = 0,
    /** Files deposited so far, against [LinkCaps.maxFileCount]. */
    val filesReceived: Int = 0,
) {
    /** Whether the link would accept a deposit at [now]. */
    fun acceptsDepositsAt(now: Instant): Boolean {
        if (revokedAt != null) return false
        if (caps.expiresAt?.let { !now.isBefore(it) } == true) return false
        if (caps.maxFileCount?.let { filesReceived >= it } == true) return false
        return caps.maxTotalBytes?.let { bytesReceived < it } ?: true
    }
}

/**
 * What a guest declared about a file they deposited.
 *
 * Every field is guest-supplied and therefore untrusted; [suggestedFilename] is explicitly
 * advisory. The one field with teeth is [ciphertextHash], which the server recomputes at
 * finalization — the guest cannot lie about the bytes they actually sent.
 */
data class DropDescriptor(
    /** The declared content type, checked against the closed set for the link's protocol version. */
    val contentType: Wire<ContentType>,
    /** The declared plaintext size. */
    val plaintextSize: Long,
    /** The content address of the deposited ciphertext, recomputed server-side. */
    val ciphertextHash: BlobHash,
    /** Guest-supplied and unverified. Never used as a path. */
    val suggestedFilename: String? = null,
)

/**
 * One deposited file awaiting the provisioning user's review.
 *
 * A drop is not an asset and carries no manifest: no signatures, no album, no provenance.
 * The guest is never a signer. Adoption is what mints the asset, and the adopter — never the
 * guest — is recorded as its creator.
 *
 * There is no adopted or discarded state on this type because there is no adopted or
 * discarded row: adoption and discard both remove the inbox entry, atomically, in the same
 * transaction that creates the asset or releases the bytes.
 */
data class PendingDrop(
    /** The inbox entry's handle. */
    val id: DropId,
    /** What the guest declared. */
    val descriptor: DropDescriptor,
    /** Which link it arrived through. */
    val viaLink: UploadLinkId,
    /** Server-attested arrival time. */
    val receivedAt: Instant,
)
