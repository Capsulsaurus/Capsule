package com.justin13888.capsule.data

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flowOf

/** The data source the scaffold screens read from. */
interface MuseumRepository {
    /** Every object available to the list screen. */
    fun getObjects(): Flow<List<MuseumObject>>

    /** The object with [objectId], or `null` when there is none. */
    fun getObjectById(objectId: Int): Flow<MuseumObject?>
}

/**
 * A data source with nothing in it.
 *
 * The app has no library behind it yet, so the screens render their empty state. It makes no
 * network or storage access.
 */
class EmptyMuseumRepository : MuseumRepository {
    override fun getObjects(): Flow<List<MuseumObject>> = flowOf(emptyList())

    override fun getObjectById(objectId: Int): Flow<MuseumObject?> = flowOf(null)
}
