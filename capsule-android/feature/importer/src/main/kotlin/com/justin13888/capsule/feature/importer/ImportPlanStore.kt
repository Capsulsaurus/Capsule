package com.justin13888.capsule.feature.importer

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ImportId
import com.justin13888.capsule.core.model.ImportPlan

/** A plan and the inputs that produced it, so the same plan can be re-derived exactly. */
data class PlannedImport(
    /** What the planner decided. */
    val plan: ImportPlan,
    /** The sources it was computed over. */
    val sourceLocators: List<String>,
    /** The album the user picked, or `null` if the default rules resolved it. */
    val destination: AlbumId?,
)

/**
 * Plans awaiting confirmation, addressable by the id the shell routes on.
 *
 * A destination key carries an [ImportId] and nothing else — ids travel as strings through
 * navigation, and a whole plan cannot. The confirm screen therefore needs a way to get from
 * that id back to the plan, and re-planning blind is not it: the planner is deterministic
 * only given the *same* sources, which the id alone does not name.
 *
 * Confirmed runs need none of this — [com.justin13888.capsule.core.data.ImportRepository.progress]
 * answers for those — so this holds only the short window between planning and confirming.
 */
interface ImportPlanStore {
    /** Records a freshly computed plan. */
    fun put(planned: PlannedImport)

    /** The plan behind [id], or `null` when this process never computed it. */
    fun get(id: ImportId): PlannedImport?
}

/**
 * A process-lifetime [ImportPlanStore].
 *
 * Unconfirmed plans are deliberately not persisted. A plan is a snapshot of the library and
 * the sources at one moment; restoring a stale one after a restart would show the user
 * numbers that no longer describe what would happen, and the planner is cheap to re-run.
 */
class InMemoryImportPlanStore : ImportPlanStore {
    private val plans = mutableMapOf<ImportId, PlannedImport>()

    override fun put(planned: PlannedImport) {
        synchronized(plans) { plans[planned.plan.importId] = planned }
    }

    override fun get(id: ImportId): PlannedImport? = synchronized(plans) { plans[id] }
}
