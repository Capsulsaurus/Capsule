package com.justin13888.capsule.core.data

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ImportId
import com.justin13888.capsule.core.model.ImportPlan
import com.justin13888.capsule.core.model.ImportProgress
import com.justin13888.capsule.core.model.ImportScope
import kotlinx.coroutines.flow.Flow

/**
 * Planning and running an import.
 *
 * [plan] and [confirm] are separate calls because planning is **pure and deterministic** and
 * confirmation is not: the free-space probe that sets `streamingRecommended` is I/O, and I/O
 * inside the planner would make a plan irreproducible — the user would confirm one set of
 * numbers and see another execute. Splitting them keeps the plan a thing that can be
 * recomputed and compared.
 */
interface ImportRepository {
    /**
     * Plans an import without touching anything.
     *
     * Deterministic: the same sources and destination produce the same plan, including the
     * order of its entries. [destination] left `null` resolves through the default-album
     * rules, and the plan records which rule fired.
     */
    suspend fun plan(
        sourceLocators: List<String>,
        destination: AlbumId? = null,
    ): ImportPlan

    /**
     * Confirms a plan and starts the run, attaching the free-space probe's verdict.
     *
     * Refused when the plan still has conflicts, and refused outright when it would combine
     * streaming with a staged upload policy — the two ask for opposite things and the
     * combination has no coherent behaviour.
     */
    suspend fun confirm(plan: ImportPlan): ImportId

    /** Progress for a run in flight. */
    fun progress(id: ImportId): Flow<ImportProgress>

    /**
     * Stops a run.
     *
     * Files already imported stay imported; a cancelled run is a shorter run, not a rollback.
     * Under Move mode no source file is released for an asset that did not finish.
     */
    suspend fun cancel(id: ImportId)

    /** Import sources this device has seen, so the user can map them once. */
    fun scopes(): Flow<List<ImportScope>>

    /**
     * Records where a source's imports should land from now on.
     *
     * The answer to the "where should photos from *X* go?" question a new source asks once —
     * automated imports never silently invent a destination.
     */
    suspend fun setScopeDestination(
        scope: ImportScope,
        albumId: AlbumId,
    )
}
