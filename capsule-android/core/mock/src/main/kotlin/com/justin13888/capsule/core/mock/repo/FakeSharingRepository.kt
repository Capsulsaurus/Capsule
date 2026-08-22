package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.SharingRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.seed.DeterministicIds
import com.justin13888.capsule.core.mock.seed.Prng
import com.justin13888.capsule.core.model.LinkCaps
import com.justin13888.capsule.core.model.ShareLink
import com.justin13888.capsule.core.model.ShareLinkId
import com.justin13888.capsule.core.model.ShareScope
import com.justin13888.capsule.core.model.UploadLink
import com.justin13888.capsule.core.model.UploadLinkId
import kotlinx.coroutines.flow.Flow
import java.time.Instant

/**
 * Read links out and upload links in.
 *
 * Two creation methods rather than one parameterised call, because the two passphrases mean
 * opposite things: a share-link passphrase wraps a read secret and is unwrapped entirely on
 * the client, while an upload-link passphrase is an abuse gate whose verifier the server
 * holds. The fake mirrors that asymmetry — a created share link carries its fragment secret
 * back exactly once and an upload link never has one to carry.
 */
class FakeSharingRepository(
    private val backend: MockBackend,
) : SharingRepository {
    private val prng = Prng(Prng.mixSeed(backend.library.spec.seed, "sharing"))

    override fun shareLinks(): Flow<List<ShareLink>> =
        backend.observe { state, _ ->
            // Only what is still live. Revoked, expired and never-existed are indistinguishable
            // to a visitor by design, so there is no "revoked" row to render on the far side.
            state.shareLinks.values.filter { it.isLiveAt(backend.now()) }
        }

    override suspend fun createShareLink(
        scope: ShareScope,
        expiresAt: Instant?,
        passphrase: String?,
    ): ShareLink {
        backend.gate(MockOperation.LinkWrite)
        val now = backend.now()
        val link =
            ShareLink(
                id = ShareLinkId(DeterministicIds.uuidV7(now.toEpochMilli(), prng)),
                // 128 bits and fully opaque: a shorter or sequential identifier would let
                // somebody enumerate other people's links.
                opaqueId = DeterministicIds.hexToken(OPAQUE_ID_BYTES, prng),
                scope = scope,
                expiresAt = expiresAt,
                createdAt = now,
                requiresPassphrase = passphrase != null,
                // Returned once and never stored server-side — it lives in the URL fragment,
                // which browsers do not transmit.
                fragmentSecret = DeterministicIds.hexToken(FRAGMENT_SECRET_BYTES, prng),
            )
        backend.mutate { it.shareLinks[link.id] = link }
        return link
    }

    override suspend fun revokeShareLink(id: ShareLinkId) {
        backend.gate(MockOperation.LinkWrite)
        backend.mutate { state ->
            val link = state.shareLinks[id] ?: throw LocalError.NotFound("share link ${id.value}")
            state.shareLinks[id] = link.copy(revokedAt = backend.now())
        }
    }

    override fun uploadLinks(): Flow<List<UploadLink>> =
        backend.observe { state, _ -> state.uploadLinks.values.filter { it.revokedAt == null } }

    override suspend fun createUploadLink(
        caps: LinkCaps,
        passphrase: String?,
    ): UploadLink {
        backend.gate(MockOperation.LinkWrite)
        val now = backend.now()
        val link =
            UploadLink(
                id = UploadLinkId(DeterministicIds.uuidV7(now.toEpochMilli(), prng)),
                opaqueId = DeterministicIds.hexToken(OPAQUE_ID_BYTES, prng),
                caps = caps,
                createdAt = now,
                requiresPassphrase = passphrase != null,
            )
        backend.mutate { it.uploadLinks[link.id] = link }
        return link
    }

    override suspend fun revokeUploadLink(id: UploadLinkId) {
        backend.gate(MockOperation.LinkWrite)
        backend.mutate { state ->
            val link = state.uploadLinks[id] ?: throw LocalError.NotFound("upload link ${id.value}")
            // Deposits already in the inbox are unaffected: revoking the door does not empty
            // the room behind it.
            state.uploadLinks[id] = link.copy(revokedAt = backend.now())
        }
    }

    private companion object {
        const val OPAQUE_ID_BYTES = 16
        const val FRAGMENT_SECRET_BYTES = 16
    }
}
