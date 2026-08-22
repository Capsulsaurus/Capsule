package com.justin13888.capsule.core.mock.repo

import com.justin13888.capsule.core.data.ImportRepository
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.seed.DeterministicIds
import com.justin13888.capsule.core.mock.seed.Prng
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.DefaultAlbumRule
import com.justin13888.capsule.core.model.ImportCandidate
import com.justin13888.capsule.core.model.ImportDecision
import com.justin13888.capsule.core.model.ImportFileState
import com.justin13888.capsule.core.model.ImportId
import com.justin13888.capsule.core.model.ImportPlan
import com.justin13888.capsule.core.model.ImportProgress
import com.justin13888.capsule.core.model.ImportScope
import kotlinx.coroutines.flow.Flow

/**
 * Planning and running an import.
 *
 * [plan] is pure and takes no draws from any shared stream: every decision is a function of
 * the locator's own hash, so planning the same sources twice produces the same plan down to
 * the order of its entries. That is the property the split from [confirm] exists to protect —
 * the free-space probe that sets `streamingRecommended` is I/O, and I/O inside the planner
 * would let a user confirm one set of numbers and watch another execute.
 */
class FakeImportRepository(
    private val backend: MockBackend,
    private val albums: FakeAlbumRepository,
) : ImportRepository {
    override suspend fun plan(
        sourceLocators: List<String>,
        destination: AlbumId?,
    ): ImportPlan {
        backend.gate(MockOperation.ImportPlan)
        val resolved = destination ?: albums.resolveDefaultAlbum().albumId
        val added = mutableListOf<ImportCandidate>()
        val skipped = mutableListOf<ImportCandidate>()
        val conflicts = mutableListOf<ImportCandidate>()
        sourceLocators.forEach { locator ->
            val candidate = classify(locator, resolved)
            when (candidate.decision) {
                is ImportDecision.Import -> added += candidate
                is ImportDecision.SkipDuplicate, ImportDecision.SkipUnsupported -> skipped += candidate
                is ImportDecision.SkipError -> conflicts += candidate
            }
        }
        return ImportPlan(
            importId = ImportId(DeterministicIds.uuidV7(0, planPrng(sourceLocators))),
            added = added,
            skipped = skipped,
            conflicts = conflicts,
            totalSize = added.sumOf(ImportCandidate::sizeBytes),
            destinationRule = if (destination == null) DefaultAlbumRule.OwnerPointer else DefaultAlbumRule.ExplicitPick,
        )
    }

    override suspend fun confirm(plan: ImportPlan): ImportId {
        backend.gate(MockOperation.ImportRun)
        if (!plan.isReady) {
            throw LocalError.Storage("the plan still has ${plan.conflicts.size} conflicts to resolve")
        }
        // The mock has no bytes to move, so a confirmed run lands finished. Modelling a
        // long-running run here would mean inventing a scheduler whose timing the screenshot
        // goldens would then depend on.
        val progress =
            ImportProgress(
                importId = plan.importId,
                completed = plan.added.size,
                total = plan.added.size,
                currentState = ImportFileState.Done,
                // No outcomes: the mock moved no bytes, and inventing successes here would put
                // asset ids in front of the UI that nothing in the library answers to.
                outcomes = emptyList(),
            )
        backend.mutate { it.imports[plan.importId] = progress }
        return plan.importId
    }

    override fun progress(id: ImportId): Flow<ImportProgress> =
        backend.observe { state, _ ->
            state.imports[id] ?: ImportProgress(importId = id, completed = 0, total = 0)
        }

    override suspend fun cancel(id: ImportId) {
        backend.gate(MockOperation.ImportRun)
        // Files already imported stay imported: a cancelled run is a shorter run, not a
        // rollback, and under Move mode no source file is released for a file that did not
        // finish.
        backend.mutate { state ->
            val running = state.imports[id] ?: return@mutate
            state.imports[id] = running.copy(total = running.completed, currentState = ImportFileState.Done)
        }
    }

    override fun scopes(): Flow<List<ImportScope>> = backend.observe { state, _ -> state.scopes.toList() }

    override suspend fun setScopeDestination(
        scope: ImportScope,
        albumId: AlbumId,
    ) {
        backend.gate(MockOperation.SettingsWrite)
        backend.requireAlbum(albumId)
        backend.mutate { state ->
            state.scopeDestinations[scope.locator] = albumId
            if (state.scopes.none { it.locator == scope.locator }) state.scopes += scope
        }
    }

    /**
     * What the planner decides about one candidate.
     *
     * Driven by a hash of the locator rather than by a draw, so the decision is a property of
     * the file rather than of the order the planner happened to visit it in. The two skip
     * reasons stay distinct: "already in your library" and "nothing here can read this" are
     * different problems and a UI that showed them alike would tell a user their photos were
     * imported when they were not.
     */
    private fun classify(
        locator: String,
        destination: AlbumId,
    ): ImportCandidate {
        val prng = Prng(Prng.mixSeed(backend.library.spec.seed, locator))
        val size = prng.nextInt(MIN_SIZE_KB, MAX_SIZE_KB).toLong() * BYTES_PER_KB
        val roll = prng.nextDouble()
        val decision =
            when {
                roll < DUPLICATE_SHARE ->
                    backend
                        .read { it.assets.values.firstOrNull() }
                        ?.let { ImportDecision.SkipDuplicate(it.id) }
                        ?: ImportDecision.Import(destination)

                roll < DUPLICATE_SHARE + UNSUPPORTED_SHARE -> ImportDecision.SkipUnsupported
                roll < DUPLICATE_SHARE + UNSUPPORTED_SHARE + ERROR_SHARE ->
                    ImportDecision.SkipError("the file could not be read well enough to decide")

                else -> ImportDecision.Import(destination)
            }
        return ImportCandidate(sourceLocator = locator, sizeBytes = size, decision = decision)
    }

    /** A stream seeded from the whole source list, so the run id is a function of the plan. */
    private fun planPrng(sourceLocators: List<String>): Prng =
        Prng(Prng.mixSeed(backend.library.spec.seed, sourceLocators.joinToString("|")))

    private companion object {
        const val MIN_SIZE_KB = 900
        const val MAX_SIZE_KB = 42_000
        const val BYTES_PER_KB = 1024L
        const val DUPLICATE_SHARE = 0.12
        const val UNSUPPORTED_SHARE = 0.05
        const val ERROR_SHARE = 0.03
    }
}
