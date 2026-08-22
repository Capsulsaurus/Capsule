package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.data.PeopleRepository
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.model.Asset
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.FaceCluster
import com.justin13888.capsule.core.model.FaceClusterId
import com.justin13888.capsule.core.model.Person
import com.justin13888.capsule.core.model.PersonId
import kotlinx.coroutines.flow.Flow

/**
 * Face clusters, and the people the user attaches to them.
 *
 * Nothing here creates or edits a cluster, and the omission is the design: a cluster is a
 * computed view — a pure function of the asset set and the model slot behind the embeddings —
 * so a write path would be a second source of truth that the next re-clustering would silently
 * overwrite. What the user contributes is a name and an attachment, and only those survive a
 * model swap.
 */
class FakePeopleRepository(
    private val backend: MockBackend,
) : PeopleRepository {
    override fun people(): Flow<List<Person>> =
        backend.observe { state, scenario ->
            if (!scenario.populated) {
                emptyList()
            } else {
                state.people.filterNot { it.id in state.hiddenPeople }
            }
        }

    override fun person(id: PersonId): Flow<Person?> =
        backend.observe { state, _ ->
            // Hidden people are still returned by id: hiding removes them from the People view,
            // not from the library, and a deep link to one must still resolve.
            state.people.firstOrNull { it.id == id }
        }

    override fun clusters(): Flow<List<FaceCluster>> =
        backend.observe { state, scenario ->
            // Stale clusters are surfaced rather than hidden: they are the regeneration queue,
            // and a user who watches a person vanish after an app update is owed the reason.
            if (scenario.populated) state.clusters.toList() else emptyList()
        }

    override suspend fun assignCluster(
        clusterId: FaceClusterId,
        personId: PersonId,
    ) {
        backend.gate(MockOperation.PeopleWrite)
        backend.mutate { state ->
            val at = state.clusters.indexOfFirst { it.id == clusterId }
            if (at < 0) throw LocalError.NotFound("cluster ${clusterId.value}")
            state.clusters[at] = state.clusters[at].copy(personId = personId)
            state.detachedClusters -= clusterId
            val cluster = state.clusters[at]
            val personAt = state.people.indexOfFirst { it.id == personId }
            if (personAt >= 0) {
                val person = state.people[personAt]
                state.people[personAt] =
                    person.copy(
                        clusterIds = (person.clusterIds + clusterId).distinct(),
                        assetCount = person.assetCount + cluster.assetIds.size,
                    )
            } else {
                // Naming an unnamed grouping is how a person first comes into existence —
                // there is no separate create call on the contract. Without this the id
                // would be attached to the cluster while no person row existed, and the
                // very next rename of that id would fail as not-found.
                state.people +=
                    Person(
                        id = personId,
                        displayName = "",
                        clusterIds = listOf(clusterId),
                        coverAssetId = cluster.assetIds.firstOrNull(),
                        assetCount = cluster.assetIds.size,
                    )
            }
        }
    }

    override suspend fun detachCluster(clusterId: FaceClusterId) {
        backend.gate(MockOperation.PeopleWrite)
        backend.mutate { state ->
            val at = state.clusters.indexOfFirst { it.id == clusterId }
            if (at < 0) throw LocalError.NotFound("cluster ${clusterId.value}")
            state.clusters[at] = state.clusters[at].copy(personId = null)
            state.detachedClusters += clusterId
            state.people.replaceAll { person ->
                person.copy(clusterIds = person.clusterIds.filterNot { it == clusterId })
            }
        }
    }

    override suspend fun renamePerson(
        id: PersonId,
        displayName: String,
    ) {
        backend.gate(MockOperation.PeopleWrite)
        updatePerson(id) { it.copy(displayName = displayName) }
    }

    override suspend fun mergePeople(
        from: PersonId,
        into: PersonId,
    ) {
        backend.gate(MockOperation.PeopleWrite)
        backend.mutate { state ->
            val source = state.people.firstOrNull { it.id == from } ?: throw LocalError.NotFound("person ${from.value}")
            val targetAt = state.people.indexOfFirst { it.id == into }
            if (targetAt < 0) throw LocalError.NotFound("person ${into.value}")
            val target = state.people[targetAt]
            state.people[targetAt] =
                target.copy(
                    clusterIds = (target.clusterIds + source.clusterIds).distinct(),
                    assetCount = target.assetCount + source.assetCount,
                )
            state.people.removeAll { it.id == from }
            state.clusters.replaceAll { cluster ->
                if (cluster.personId == from) cluster.copy(personId = into) else cluster
            }
        }
    }

    override suspend fun setPersonCover(
        id: PersonId,
        assetId: AssetId,
    ) {
        backend.gate(MockOperation.PeopleWrite)
        updatePerson(id) { it.copy(coverAssetId = assetId) }
    }

    override suspend fun personAssets(
        id: PersonId,
        offset: Int,
        limit: Int,
    ): List<Asset> {
        backend.gate(MockOperation.PeopleRead)
        val members =
            backend.read { state ->
                val person = state.people.firstOrNull { it.id == id } ?: return@read emptyList<Asset>()
                val ids =
                    state.clusters
                        .filter { it.id in person.clusterIds }
                        .flatMap(FaceCluster::assetIds)
                        .toSet()
                backend.visibleAssets(state).filter { it.id in ids }
            }
        if (offset >= members.size || limit <= 0) return emptyList()
        return members.subList(offset.coerceAtLeast(0), (offset + limit).coerceAtMost(members.size))
    }

    override suspend fun hidePerson(
        id: PersonId,
        hidden: Boolean,
    ) {
        backend.gate(MockOperation.PeopleWrite)
        // Removes them from the People view without touching the clusters behind them, so the
        // model's work is not thrown away by a presentation decision.
        backend.mutate { state -> if (hidden) state.hiddenPeople += id else state.hiddenPeople -= id }
    }

    private fun updatePerson(
        id: PersonId,
        transform: (Person) -> Person,
    ) {
        backend.mutate { state ->
            val at = state.people.indexOfFirst { it.id == id }
            if (at < 0) throw LocalError.NotFound("person ${id.value}")
            state.people[at] = transform(state.people[at])
        }
    }
}
