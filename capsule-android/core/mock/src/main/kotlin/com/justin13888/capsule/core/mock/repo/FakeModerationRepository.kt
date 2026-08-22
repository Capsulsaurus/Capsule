package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.ModerationRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.QuotaScenario
import com.justin13888.capsule.core.model.BlobHash
import com.justin13888.capsule.core.model.ModerationEvent
import com.justin13888.capsule.core.model.SuspensionEffects
import com.justin13888.capsule.core.model.UserBlock
import com.justin13888.capsule.core.model.UserId
import kotlinx.coroutines.flow.Flow

/**
 * Operator actions taken against the account, and the blocks this user placed themselves.
 *
 * [report] names a content hash and nothing else. That is the whole shape of federated
 * moderation: an operator can act on a hash they were told about without anyone handing them
 * the ability to read the content, so reporting cannot become a decryption side channel — and
 * a mock that accepted plaintext here would let a UI be built that eventually sends it.
 */
class FakeModerationRepository(
    private val backend: MockBackend,
) : ModerationRepository {
    override fun events(): Flow<List<ModerationEvent>> =
        backend.observe { _, scenario ->
            // Surfaced, never swallowed: the log is append-only and a lift is its own entry
            // rather than a mutation of the takedown it reverses.
            if (!scenario.populated) {
                emptyList()
            } else {
                backend.library.moderationEvents.sortedByDescending { it.createdAt }
            }
        }

    override fun suspensionEffects(): Flow<SuspensionEffects?> =
        backend.observe { _, scenario ->
            // Suspension is an access-level action, not a data-level one: reads survive, and
            // master-key-gated session revocation stays available so a user whose account was
            // suspended after a compromise can still lock the attacker out.
            if (scenario.quota == QuotaScenario.Suspended) SuspensionEffects() else null
        }

    override fun blocks(): Flow<List<UserBlock>> = backend.observe { state, _ -> state.blocks.toList() }

    override suspend fun blockUser(id: UserId) {
        backend.gate(MockOperation.ModerationWrite)
        // Idempotent, and personal: the block rides a group removal and a key-epoch bump and
        // never becomes a server-wide federation block.
        backend.mutate { state ->
            if (state.blocks.none { it.blockedUserId == id }) {
                state.blocks += UserBlock(blockedUserId = id, createdAt = backend.now())
            }
        }
    }

    override suspend fun unblockUser(id: UserId) {
        backend.gate(MockOperation.ModerationWrite)
        backend.mutate { state -> state.blocks.removeAll { it.blockedUserId == id } }
    }

    override suspend fun report(
        contentHash: BlobHash,
        albumPointer: String?,
        reason: String?,
    ) {
        backend.gate(MockOperation.ModerationWrite)
        // Nothing is stored locally: a report leaves for the content's home server, and the
        // pointer locates the asset for an operator who already has album access rather than
        // granting any.
        check(contentHash.value.isNotEmpty()) { "a report must name a content hash" }
    }
}
