package com.justin13888.capsule.feature.importer

import android.content.Context
import android.os.StatFs
import com.justin13888.capsule.core.data.ImportRepository
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ImportScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * The answers to "where should photos from *X* go?", readable again after they are given.
 *
 * [ImportRepository.setScopeDestination] records the answer but the contract exposes no way
 * to read one back, and the picker cannot ask *once* without knowing which sources it has
 * already asked about. This is that read side, kept deliberately narrow: writes still go
 * through the repository, and this only mirrors them so the prompt is not repeated.
 */
interface ScopeMappingStore {
    /** Every remembered answer, keyed by the scope's stable locator. */
    fun mappings(): Flow<Map<String, AlbumId>>

    /** Remembers that [scope]'s imports land in [albumId]. */
    fun remember(
        scope: ImportScope,
        albumId: AlbumId,
    )
}

/**
 * A process-lifetime [ScopeMappingStore].
 *
 * In memory on purpose: the durable record is the repository's, and a second persisted copy
 * would be a second source of truth that could disagree with it. Losing the mirror at process
 * death costs at most one repeated prompt, where a stale persisted mirror would silently file
 * photos in the wrong album.
 */
class InMemoryScopeMappingStore : ScopeMappingStore {
    private val state = MutableStateFlow<Map<String, AlbumId>>(emptyMap())

    override fun mappings(): Flow<Map<String, AlbumId>> = state.asStateFlow()

    override fun remember(
        scope: ImportScope,
        albumId: AlbumId,
    ) {
        state.update { it + (scope.locator to albumId) }
    }
}

/**
 * The real free-space probe, over the volume the app's own files live on.
 *
 * `StatFs` reports the *available* blocks rather than the free ones, which is the number that
 * matters: the difference is space reserved for the system that an import cannot have, and
 * planning against it would promise room the import then fails to find.
 */
class AndroidFreeSpaceProbe(
    private val context: Context,
) : FreeSpaceProbe {
    override suspend fun availableBytes(): Long {
        val stats = StatFs(context.filesDir.absolutePath)
        return stats.availableBytes
    }
}
