package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.SmartAlbumId
import kotlinx.coroutines.flow.Flow

/**
 * Search, and the smart albums that are saved searches.
 *
 * Search runs entirely locally, over the assets the viewer can already decrypt — a gallery
 * read path performs no network I/O at all, which is a design requirement rather than a
 * performance preference.
 *
 * Smart-album membership is **computed, never stored**. [evaluate] is therefore a query
 * rather than a lookup, and editing a definition moves and re-encrypts nothing.
 */
interface SearchRepository {
    /**
     * A window of assets matching a free-text query, in the same order on every run.
     *
     * Hidden and trashed assets are excluded, like every other default view.
     */
    suspend fun search(
        query: String,
        offset: Int,
        limit: Int,
    ): List<Asset>

    /** Completion candidates for a partially typed query. */
    fun suggestions(prefix: String): Flow<List<String>>

    /**
     * Every smart-album definition the owner holds, including ones this build cannot evaluate.
     *
     * Definitions from a later grammar are present here on purpose: they must be shown as
     * "created by a newer app version" and synced onward intact, never stripped.
     */
    fun smartAlbums(): Flow<List<SmartAlbumDefinition>>

    /**
     * Saves a definition — one stamped write of the whole definition.
     *
     * Whole-definition rather than field-wise, so there is never a partial-predicate merge:
     * two devices editing the same smart album cannot produce a tree neither of them wrote.
     * Throws if the definition does not validate.
     */
    suspend fun saveSmartAlbum(definition: SmartAlbumDefinition)

    /** Deletes a definition — a stamped absence, so the deletion converges like any edit. */
    suspend fun deleteSmartAlbum(id: SmartAlbumId)

    /**
     * A window of the assets a definition currently matches.
     *
     * Evaluated in sorted asset-id order so the result is byte-identical across devices and
     * runs. A definition from a later grammar is never evaluated — the call reports it rather
     * than guessing at terms it does not understand.
     */
    suspend fun evaluate(
        id: SmartAlbumId,
        offset: Int,
        limit: Int,
    ): List<Asset>
}
