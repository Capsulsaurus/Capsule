package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.LinkCaps
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.ShareLinkId
import com.justin13888.capsule.core.model.ShareScope
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.UploadLinkId
import kotlinx.coroutines.flow.Flow
import java.time.Instant

/**
 * Links handed to people outside the album — read links out, upload links in.
 *
 * The two kinds are separate methods rather than one parameterised call because their
 * passphrases mean opposite things. A share-link passphrase wraps a read secret and is
 * unwrapped entirely client-side; an upload-link passphrase is an abuse gate whose verifier
 * the server holds. One `createLink(passphrase)` would make it far too easy to send the wrong
 * one to a server that must never see it.
 */
interface SharingRepository {
    /** Every share link this user has issued and not yet revoked. */
    fun shareLinks(): Flow<List<ShareLink>>

    /**
     * Issues a read link.
     *
     * The returned link carries its fragment secret exactly once — the server never receives
     * it, so this call is the only chance to show or copy it.
     */
    suspend fun createShareLink(
        scope: ShareScope,
        expiresAt: Instant? = null,
        passphrase: String? = null,
    ): ShareLink

    /**
     * Revokes a share link.
     *
     * To a visitor the result is indistinguishable from a link that never existed, which is
     * why there is no "revoked" state to observe on the far side.
     */
    suspend fun revokeShareLink(id: ShareLinkId)

    /** Every upload link this user has provisioned. */
    fun uploadLinks(): Flow<List<UploadLink>>

    /** Provisions an upload link under [caps]. */
    suspend fun createUploadLink(
        caps: LinkCaps,
        passphrase: String? = null,
    ): UploadLink

    /** Revokes an upload link. Deposits already in the inbox are unaffected. */
    suspend fun revokeUploadLink(id: UploadLinkId)
}
