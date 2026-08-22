package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.data.SearchRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockState
import com.justin13888.capsule.core.model.AiTag
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetTags
import com.justin13888.capsule.core.model.SmartAlbumDefinition
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.SmartAlbumValidation
import com.justin13888.capsule.core.model.SmartAlbumValidator
import com.justin13888.capsule.core.model.UserTag
import kotlinx.coroutines.flow.Flow
import java.util.Locale

/**
 * Local search, and the smart albums that are saved searches.
 *
 * No network operation appears anywhere in this class, and that is a design requirement
 * rather than a shortcut: a gallery read path performs no network I/O at all, so a mock that
 * gated search behind connectivity would let a UI depend on behaviour the real client will
 * never have.
 */
class FakeSearchRepository(
    private val backend: MockBackend,
) : SearchRepository {
    override suspend fun search(
        query: String,
        offset: Int,
        limit: Int,
    ): List<Asset> {
        backend.gate(MockOperation.Search)
        val needle = query.trim().lowercase(Locale.ROOT)
        if (needle.isEmpty()) return emptyList()
        val hits =
            backend.read { state ->
                backend.visibleAssets(state).filter { asset -> matches(state, asset, needle) }
            }
        if (offset >= hits.size || limit <= 0) return emptyList()
        return hits.subList(offset.coerceAtLeast(0), (offset + limit).coerceAtMost(hits.size))
    }

    override fun suggestions(prefix: String): Flow<List<String>> =
        backend.observe { state, _ ->
            val needle = prefix.trim().lowercase(Locale.ROOT)
            if (needle.isEmpty()) {
                emptyList()
            } else {
                vocabulary(state)
                    .filter { it.startsWith(needle) }
                    .distinct()
                    .sorted()
                    .take(MAX_SUGGESTIONS)
            }
        }

    override fun smartAlbums(): Flow<List<SmartAlbumDefinition>> =
        backend.observe { state, _ ->
            // Definitions from a later grammar are present on purpose: they have to be shown as
            // "created by a newer app version" and synced onward intact, never stripped.
            state.smartAlbums.values.toList()
        }

    override suspend fun saveSmartAlbum(definition: SmartAlbumDefinition) {
        backend.gate(MockOperation.SmartAlbumWrite)
        // Every violation, not just the first: a definition arriving over sync cannot be fixed
        // interactively, so the user needs the whole list to understand what the other device
        // wrote.
        val validation = SmartAlbumValidator.validate(definition)
        if (validation is SmartAlbumValidation.Rejected) {
            throw LocalError.Storage("smart album rejected: ${validation.violations}")
        }
        backend.mutate { it.smartAlbums[definition.id] = definition }
    }

    override suspend fun deleteSmartAlbum(id: SmartAlbumId) {
        backend.gate(MockOperation.SmartAlbumWrite)
        backend.mutate { it.smartAlbums.remove(id) }
    }

    override suspend fun evaluate(
        id: SmartAlbumId,
        offset: Int,
        limit: Int,
    ): List<Asset> {
        backend.gate(MockOperation.Search)
        val definition =
            backend.read { it.smartAlbums[id] } ?: throw LocalError.NotFound("smart album ${id.value}")
        // A definition from a later grammar is preserved and reported, never evaluated:
        // guessing at terms this build does not understand would compute a different
        // membership than the device that wrote it.
        if (SmartAlbumValidator.validate(definition) is SmartAlbumValidation.PreservedUnevaluated) {
            throw ProtocolError.VersionUnsupported
        }
        val matches =
            backend
                .read { state ->
                    backend.visibleAssets(state).filter { asset ->
                        PredicateEvaluator.matches(definition.predicate, asset, state.tags[asset.id] ?: AssetTags())
                    }
                }
                // Sorted asset-id order, so the result is byte-identical across devices and runs.
                .sortedBy { it.id.value }
        if (offset >= matches.size || limit <= 0) return emptyList()
        return matches.subList(offset.coerceAtLeast(0), (offset + limit).coerceAtMost(matches.size))
    }

    /** Whether an asset answers to [needle] across the fields a user would expect to search. */
    private fun matches(
        state: MockState,
        asset: Asset,
        needle: String,
    ): Boolean {
        val tags = state.tags[asset.id]
        val fields =
            sequenceOf(
                state.captions[asset.id]?.current?.value,
                state.albums
                    .firstOrNull { it.id == asset.albumId }
                    ?.name
                    ?.takeIf(String::isNotEmpty),
                asset.camera?.model,
            )
        val tagWords =
            tags
                ?.user
                .orEmpty()
                .asSequence()
                .map(UserTag::tag) +
                tags
                    ?.freshAi
                    .orEmpty()
                    .asSequence()
                    .map(AiTag::tag)
        return (fields + tagWords).filterNotNull().any { it.lowercase(Locale.ROOT).contains(needle) }
    }

    /** Every word a completion could offer, drawn from what the library actually holds. */
    private fun vocabulary(state: MockState): List<String> =
        state.tags.values.flatMap { entry ->
            entry.user.map(UserTag::tag) + entry.freshAi.map { it.tag }
        } + state.albums.map { it.name.lowercase(Locale.ROOT) }.filter(String::isNotEmpty)

    private companion object {
        const val MAX_SUGGESTIONS = 8
    }
}
