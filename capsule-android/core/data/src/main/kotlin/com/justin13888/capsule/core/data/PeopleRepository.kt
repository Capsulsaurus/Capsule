package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.FaceClusterId
import com.justin13888.capsule.core.model.Person
import com.justin13888.capsule.core.model.PersonId
import kotlinx.coroutines.flow.Flow

/**
 * Face clusters and the people the user attaches to them.
 *
 * The asymmetry here is the whole design. A [FaceCluster] is a **computed view** — a pure
 * function of the asset set and the model slot that produced the embeddings — so nothing on
 * this interface creates or edits one; re-running the clustering replaces them wholesale. A
 * [Person] is the user's contribution: a name, and which clusters they said were that person.
 * Only the second kind survives a model swap.
 */
interface PeopleRepository {
    /** Everyone the user has named. */
    fun people(): Flow<List<Person>>

    /** One person, re-emitted as their clusters or cover change. */
    fun person(id: PersonId): Flow<Person?>

    /**
     * The current clusters, including ones whose model slot has been superseded.
     *
     * Stale clusters are surfaced rather than hidden: they are the regeneration queue, and a
     * user who sees a person vanish after an app update deserves the explanation.
     */
    fun clusters(): Flow<List<FaceCluster>>

    /** Attaches a cluster to a person, creating the person if [personId] is new to it. */
    suspend fun assignCluster(
        clusterId: FaceClusterId,
        personId: PersonId,
    )

    /** Detaches a cluster — the user saying "that is not them". */
    suspend fun detachCluster(clusterId: FaceClusterId)

    /** Renames a person. The name is the user's, never the model's. */
    suspend fun renamePerson(
        id: PersonId,
        displayName: String,
    )

    /** Merges two people into one, moving every cluster onto [into]. */
    suspend fun mergePeople(
        from: PersonId,
        into: PersonId,
    )

    /** Chooses which asset shows as a person's face. */
    suspend fun setPersonCover(
        id: PersonId,
        assetId: AssetId,
    )

    /** A window of the assets a person appears in. */
    suspend fun personAssets(
        id: PersonId,
        offset: Int,
        limit: Int,
    ): List<Asset>

    /** Removes a person from the People view without deleting the clusters behind them. */
    suspend fun hidePerson(
        id: PersonId,
        hidden: Boolean,
    )
}
