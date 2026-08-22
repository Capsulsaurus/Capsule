package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceCohort
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.GatedView
import com.justin13888.capsule.core.model.LocalAuthGrant
import com.justin13888.capsule.core.model.Session
import com.justin13888.capsule.core.model.SessionId
import kotlinx.coroutines.flow.Flow

/**
 * Sessions, enrolled devices, and the local-auth gate in front of the sensitive views.
 *
 * [revokeAllSessions] takes a proof rather than relying on the caller's own session, and the
 * signature says so on purpose: the global revoke is authenticated by a signature over a
 * server-issued challenge, never by a session token, so that a user whose session token was
 * stolen can still lock the thief out. A parameterless `revokeAll()` would hide that
 * requirement until it failed at runtime.
 */
interface SessionRepository {
    /** The session this process is using, or `null` when not signed in. */
    fun currentSession(): Flow<Session?>

    /** Every session on the account, so the user can see and end the ones they do not know. */
    fun sessions(): Flow<List<Session>>

    /**
     * Sessions grouped by the physical device they appear to come from.
     *
     * Client-asserted and unverifiable, and therefore never an input to any authorization or
     * capability decision. It exists to make a session list comprehensible, nothing more.
     */
    fun cohorts(): Flow<List<DeviceCohort>>

    /** Ends one session. Authenticated by any active session. */
    suspend fun revokeSession(id: SessionId)

    /**
     * Ends every session, including this one.
     *
     * [masterKeyProof] is a signature over a server-issued challenge. Without one the call
     * revokes nothing at all — there is no partial success, and so nothing for a client to
     * half-clear locally.
     */
    suspend fun revokeAllSessions(masterKeyProof: ByteArray)

    /** Every device in the signed directory, revoked ones included. */
    fun devices(): Flow<List<Device>>

    /**
     * Retires a device.
     *
     * Its directory record is marked rather than removed: every provenance record it signed
     * is bound by its key, so pruning it would make the user's own history unverifiable.
     */
    suspend fun revokeDevice(id: DeviceId)

    /**
     * Prompts for fresh local authentication for one gated view.
     *
     * Grants are per view and short-lived. Opening Recently Deleted must not also open
     * Hidden — the two are different statements about what the user meant to keep out of
     * sight. This is UX protection against a borrowed unlocked phone, explicitly not a
     * cryptographic boundary.
     */
    suspend fun requireLocalAuth(view: GatedView): LocalAuthGrant

    /** The live grant for a view, or `null` when it has lapsed. */
    fun localAuthGrant(view: GatedView): Flow<LocalAuthGrant?>
}
