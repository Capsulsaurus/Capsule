package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.BlobHash
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.SuspensionEffects
import com.justin13888.capsule.core.model.UserBlock
import com.justin13888.capsule.core.model.UserId
import kotlinx.coroutines.flow.Flow

/**
 * Operator actions taken against this account, and the blocks this user placed themselves.
 *
 * [report] names a **content hash**, never plaintext and never decryption material. That is
 * the whole shape of federated moderation: an operator can act on a hash they were told
 * about without anyone handing them the ability to read the content, so reporting cannot
 * become a decryption side channel.
 */
interface ModerationRepository {
    /** Actions taken against this account, newest first. Surfaced, never swallowed. */
    fun events(): Flow<List<ModerationEvent>>

    /** What the account can still do, when it is suspended. */
    fun suspensionEffects(): Flow<SuspensionEffects?>

    /**
     * Accounts this user has blocked.
     *
     * Personal and non-propagating: a block rides a group removal and a key-epoch bump, and
     * never becomes a server-wide federation block. One user's choice is not policy for
     * everyone on the server.
     */
    fun blocks(): Flow<List<UserBlock>>

    /** Blocks an account. Idempotent — re-blocking changes nothing. */
    suspend fun blockUser(id: UserId)

    /** Lifts a block. */
    suspend fun unblockUser(id: UserId)

    /**
     * Reports content to its home server by hash.
     *
     * [albumPointer] locates the asset for an operator who already has album access; it
     * grants no access of its own.
     */
    suspend fun report(
        contentHash: BlobHash,
        albumPointer: String?,
        reason: String?,
    )
}
