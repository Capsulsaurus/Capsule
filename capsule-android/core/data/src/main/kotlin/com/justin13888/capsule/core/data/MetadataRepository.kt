package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.AddId
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.Caption
import com.justin13888.capsule.core.model.SupersededCaption
import kotlinx.coroutines.flow.Flow

/**
 * The collaborative metadata on an asset — captions and the two tag namespaces.
 *
 * Split from [AssetRepository] because these fields merge differently: captions are
 * last-writer-wins with a displaced-value log, tags are observed-remove sets keyed by add
 * id. A caller working with tags needs the add ids in hand, and a caller working with flags
 * never does.
 *
 * The tag operations are deliberately four rather than two. Adding a user tag, removing one,
 * dismissing an AI suggestion, and promoting one are different operations with different
 * provenance — promotion in particular is an explicit signed action that mints a fresh
 * user-scoped add id — and a single `setTags` would erase all four distinctions.
 */
interface MetadataRepository {
    /** The asset's caption and the values concurrent edits displaced. */
    fun caption(id: AssetId): Flow<Caption>

    /** Writes a caption. Bounded at [Caption.MAX_VALUE_BYTES]. */
    suspend fun setCaption(
        id: AssetId,
        value: String,
    )

    /** Clears the caption — a stamped absence, converging like any other write. */
    suspend fun clearCaption(id: AssetId)

    /**
     * Restores a displaced caption as the current one.
     *
     * A fresh write rather than a rollback: the value the user picked becomes the newest
     * stamped value, so it wins on every device without any special replay rule.
     */
    suspend fun restoreSupersededCaption(
        id: AssetId,
        superseded: SupersededCaption,
    )

    /** Both tag sets, re-emitted as either changes. */
    fun tags(id: AssetId): Flow<AssetTags>

    /** Adds a user tag, minting a fresh add id for it. */
    suspend fun addUserTag(
        id: AssetId,
        tag: String,
    )

    /**
     * Removes a user tag by the add it targets.
     *
     * Keyed by [AddId] rather than by text: two devices can add the same word independently,
     * and removing "the tag `beach`" would delete an add the remover never observed.
     */
    suspend fun removeUserTag(
        id: AssetId,
        addId: AddId,
    )

    /** Dismisses a model suggestion — an observed-remove on the AI set. */
    suspend fun dismissAiTag(
        id: AssetId,
        addId: AddId,
    )

    /**
     * Copies a model suggestion into the user set under a fresh user-scoped add id.
     *
     * Never automatic. An AI tag becoming a user tag is a statement about the user's intent,
     * and only the user can make it.
     */
    suspend fun promoteAiTag(
        id: AssetId,
        addId: AddId,
        alsoDismiss: Boolean = false,
    )
}
