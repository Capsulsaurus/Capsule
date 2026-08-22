package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.MetadataRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockState
import com.justin13888.capsule.core.model.AddId
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.core.model.Lww
import com.justin13888.capsule.core.model.LwwStamp
import com.justin13888.capsule.core.model.SupersededCaption
import com.justin13888.capsule.core.model.UserTag
import kotlinx.coroutines.flow.Flow

/**
 * Captions and the two tag namespaces.
 *
 * The two merge differently and the fake honours both. A caption write displaces the previous
 * value into the superseded log rather than overwriting it, because a plain last-writer-wins
 * register silently drops one side of a concurrent edit and that is real data loss. A tag
 * removal is keyed by the add it targets, because two devices can add the same word
 * independently and removing "the tag `beach`" would delete an add the remover never observed.
 */
class FakeMetadataRepository(
    private val backend: MockBackend,
) : MetadataRepository {
    override fun caption(id: AssetId): Flow<Caption> =
        backend.observe { state, _ ->
            // An asset that has never been captioned reads as an empty register rather than as
            // a missing one: "no caption yet" and "caption cleared" are the same thing to draw.
            state.captions[id] ?: EMPTY_CAPTION
        }

    override suspend fun setCaption(
        id: AssetId,
        value: String,
    ) {
        require(value.toByteArray().size <= Caption.MAX_VALUE_BYTES) {
            "a caption is bounded at ${Caption.MAX_VALUE_BYTES} bytes"
        }
        writeCaption(id, value)
    }

    override suspend fun clearCaption(id: AssetId) = writeCaption(id, null)

    override suspend fun restoreSupersededCaption(
        id: AssetId,
        superseded: SupersededCaption,
    ) {
        // A fresh write rather than a rollback: the value the user picked becomes the newest
        // stamped value, so it wins on every device with no special replay rule.
        writeCaption(id, superseded.value)
    }

    override fun tags(id: AssetId): Flow<AssetTags> = backend.observe { state, _ -> state.tags[id] ?: AssetTags() }

    override suspend fun addUserTag(
        id: AssetId,
        tag: String,
    ) {
        backend.gate(MockOperation.MetadataWrite)
        backend.requireWritable(backend.requireAsset(id))
        backend.mutate { state ->
            val existing = state.tags[id] ?: AssetTags()
            state.tags[id] = existing.copy(user = existing.user + UserTag(tag, nextAddId(state)))
        }
    }

    override suspend fun removeUserTag(
        id: AssetId,
        addId: AddId,
    ) {
        backend.gate(MockOperation.MetadataWrite)
        backend.mutate { state ->
            val existing = state.tags[id] ?: throw LocalError.NotFound("tags for ${id.value}")
            // Removing an add nobody observed is a rejection, not a silent no-op: the OR-set's
            // whole point is that a remove names the add it undoes.
            if (existing.user.none { it.addId == addId }) {
                throw LocalError.NotFound("user tag add ${addId.counter} on ${id.value}")
            }
            state.tags[id] = existing.copy(user = existing.user.filterNot { it.addId == addId })
        }
    }

    override suspend fun dismissAiTag(
        id: AssetId,
        addId: AddId,
    ) {
        backend.gate(MockOperation.MetadataWrite)
        backend.mutate { state ->
            val existing = state.tags[id] ?: throw LocalError.NotFound("tags for ${id.value}")
            state.tags[id] = existing.copy(ai = existing.ai.filterNot { it.addId == addId })
        }
    }

    override suspend fun promoteAiTag(
        id: AssetId,
        addId: AddId,
        alsoDismiss: Boolean,
    ) {
        backend.gate(MockOperation.MetadataWrite)
        backend.mutate { state ->
            val existing = state.tags[id] ?: throw LocalError.NotFound("tags for ${id.value}")
            val suggestion =
                existing.ai.firstOrNull { it.addId == addId }
                    ?: throw LocalError.NotFound("ai tag add ${addId.counter} on ${id.value}")
            // A copy under a fresh user-scoped add id, never a move: promotion is a statement
            // about the user's intent and the model's entry is not the user's to reuse.
            val promoted = existing.user + UserTag(suggestion.tag, nextAddId(state))
            val remaining = if (alsoDismiss) existing.ai.filterNot { it.addId == addId } else existing.ai
            state.tags[id] = AssetTags(user = promoted, ai = remaining)
        }
    }

    private suspend fun writeCaption(
        id: AssetId,
        value: String?,
    ) {
        backend.gate(MockOperation.MetadataWrite)
        backend.requireWritable(backend.requireAsset(id))
        val stamp = LwwStamp(backend.now(), backend.library.landmarks.thisDeviceId)
        backend.mutate { state ->
            val existing = state.captions[id] ?: EMPTY_CAPTION
            val previous = existing.current
            val displaced =
                previous?.value?.let { text ->
                    SupersededCaption(text, previous.stamp.by, previous.stamp.writtenAt)
                }
            val history = (listOfNotNull(displaced) + existing.superseded).take(Caption.MAX_SUPERSEDED)
            state.captions[id] = Caption(current = Lww(value, stamp), superseded = history)
        }
    }

    /** The next add id for this device, strictly increasing so no two adds can alias. */
    private fun nextAddId(state: MockState): AddId = AddId(backend.library.landmarks.thisDeviceId, state.addCounter++)

    private companion object {
        val EMPTY_CAPTION = Caption(current = null)
    }
}
