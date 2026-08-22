package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.AuthError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.SessionRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceCohort
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.GatedView
import com.justin13888.capsule.core.model.LocalAuthGrant
import com.justin13888.capsule.core.model.Session
import com.justin13888.capsule.core.model.SessionId
import kotlinx.coroutines.flow.Flow

/**
 * Sessions, the device directory, and the local-auth gate in front of the sensitive views.
 *
 * [revokeAllSessions] is authenticated by a proof over a server-issued challenge rather than
 * by the caller's own session, and the fake enforces that: a user whose session token was
 * stolen has to be able to lock the thief out, which a session-authenticated revoke cannot do.
 * There is no partial success either — an invalid proof revokes nothing at all, so there is
 * never a half-cleared local state to reconcile.
 */
class FakeSessionRepository(
    private val backend: MockBackend,
) : SessionRepository {
    override fun currentSession(): Flow<Session?> =
        backend.observe { state, scenario ->
            if (!scenario.signedIn) null else state.sessions.values.firstOrNull(Session::isCurrent)
        }

    override fun sessions(): Flow<List<Session>> =
        backend.observe { state, scenario ->
            if (scenario.signedIn) state.sessions.values.toList() else emptyList()
        }

    override fun cohorts(): Flow<List<DeviceCohort>> =
        backend.observe { _, scenario ->
            // Client-asserted and unverifiable, and therefore never an input to any
            // authorization decision. It exists to make a session list comprehensible.
            if (scenario.signedIn) backend.library.cohorts else emptyList()
        }

    override suspend fun revokeSession(id: SessionId) {
        backend.gate(MockOperation.SessionWrite)
        backend.mutate { state ->
            val session = state.sessions[id] ?: throw LocalError.NotFound("session ${id.value}")
            state.sessions[id] = session.copy(revokedAt = backend.now())
        }
    }

    override suspend fun revokeAllSessions(masterKeyProof: ByteArray) {
        backend.gate(MockOperation.SessionWrite)
        if (masterKeyProof.isEmpty()) throw AuthError.RevokeProofRequired
        if (!masterKeyProof.contentEquals(VALID_MASTER_KEY_PROOF)) throw AuthError.RevokeProofInvalid
        val at = backend.now()
        backend.mutate { state ->
            state.sessions.keys.toList().forEach { key ->
                state.sessions[key] = state.sessions.getValue(key).copy(revokedAt = at)
            }
        }
    }

    override fun devices(): Flow<List<Device>> =
        backend.observe { state, _ ->
            // Revoked devices are listed too. Their directory rows are never pruned: every
            // provenance record they signed is bound by their key, so removing the public half
            // would make the user's own history unverifiable.
            state.devices.values.toList()
        }

    override suspend fun revokeDevice(id: DeviceId) {
        backend.gate(MockOperation.SessionWrite)
        backend.mutate { state ->
            val device = state.devices[id] ?: throw LocalError.NotFound("device ${id.value}")
            state.devices[id] = device.copy(revokedAt = backend.now())
        }
    }

    override suspend fun requireLocalAuth(view: GatedView): LocalAuthGrant {
        backend.gate(MockOperation.LocalAuth)
        // Per view, never global: unlocking Recently Deleted must not also unlock Hidden,
        // because the two are different statements about what the user meant to keep out of
        // sight. This is UX protection against a borrowed unlocked phone, not a cryptographic
        // boundary, and the mock is written so nobody can mistake it for one.
        val grant =
            LocalAuthGrant(
                view = view,
                grantedAt = backend.now(),
                window = backend.read { it.settings.gatedViewGrace },
            )
        backend.mutate { it.grants[view] = grant }
        return grant
    }

    override fun localAuthGrant(view: GatedView): Flow<LocalAuthGrant?> =
        backend.observe { state, _ -> state.grants[view]?.takeIf { it.isFreshAt(backend.now()) } }

    private companion object {
        /**
         * The signature the fake accepts over the server's challenge.
         *
         * A fixed value rather than a real signature: the point of the parameter is that the
         * call *has* one, so the UI is built against a flow that asks for it. Verifying it for
         * real would need a key the mock deliberately does not hold.
         */
        val VALID_MASTER_KEY_PROOF = "capsule-master-key-proof".toByteArray()
    }
}
