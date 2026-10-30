package com.justin13888.capsule.screens.culling

import com.justin13888.capsule.screens.domain.AssetId
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.util.logging.Level
import java.util.logging.Logger

/** Where the pass is in reading its assets. */
enum class CullingPhase {
    LOADING,
    READY,
    FAILED,
}

/** Something the surface tells the user once, then clears with [CullingModel.consumeNotice]. */
sealed interface CullNotice {
    /** A flag could not be saved, so the asset keeps the flag it had. */
    data object FlagFailed : CullNotice

    /** The rejects could not all be moved to the trash; any that were have left the pass. */
    data object SweepFailed : CullNotice

    /** [count] rejects were moved to the trash. */
    data class Swept(
        val count: Int,
    ) : CullNotice
}

/** Everything the culling surface draws. */
data class CullingState(
    val phase: CullingPhase = CullingPhase.LOADING,
    /** The asset the current step shows, or `null` for an empty pass. */
    val asset: AssetId? = null,
    /** The current step's flag state. */
    val flag: CullStepFlag? = null,
    val position: CullPosition = CullPosition(0, 0),
    /** The verbs that would do anything from here. */
    val accepted: Set<CullVerb> = emptySet(),
    /** How many rejects the sweep would move. */
    val sweepCount: Int = 0,
    /** The sweep's confirmation is showing. */
    val isConfirmingSweep: Boolean = false,
    val notice: CullNotice? = null,
)

/**
 * Drives the culling surface: a [CullingPass] over the assets [port] lists, every verb from
 * every door (a key, a button, a TalkBack action) arriving through [perform].
 *
 * A verdict shows at once and advances; its write follows through [write], and a failed
 * write puts the flag back and says so. A stack verdict writes through
 * [CullPort.flagStack], which reaches members the pass never read. The sweep only ever runs
 * from [confirmSweep], after [perform] asked for its confirmation, and moves exactly the
 * pass's rejects.
 *
 * Main-confined: call every member from one thread (the UI's), as a ViewModel does.
 */
class CullingModel(
    private val port: CullPort,
) {
    private val pass = CullingPass()
    private val mutableState = MutableStateFlow(CullingState())

    /** What the surface draws. */
    val state: StateFlow<CullingState> = mutableState.asStateFlow()

    /** Read the pass from the port; `true` when it was read. */
    suspend fun load(): Boolean =
        try {
            pass.append(port.pass())
            log.info("culling: pass loaded with ${pass.steps.size} steps")
            publish { it.copy(phase = CullingPhase.READY) }
            true
        } catch (error: CancellationException) {
            throw error
        } catch (
            @Suppress("TooGenericExceptionCaught") error: Exception,
        ) {
            log.log(Level.WARNING, "culling: loading the pass failed", error)
            publish { it.copy(phase = if (pass.isEmpty) CullingPhase.FAILED else CullingPhase.READY) }
            false
        }

    /**
     * Run [verb] from any of its doors, if the pass accepts it from here.
     *
     * @return the verdict to [write], for a pick, unflag or reject; `null` otherwise. A
     *   sweep only asks for its confirmation.
     */
    fun perform(verb: CullVerb): CullVerdict? {
        if (!pass.accepts(verb)) return null
        log.fine("culling: $verb")
        if (verb == CullVerb.SWEEP) {
            publish { it.copy(isConfirmingSweep = true) }
            return null
        }
        var verdict: CullVerdict? = null
        when (verb) {
            CullVerb.PICK, CullVerb.NEUTRAL, CullVerb.REJECT -> verdict = verb.flag?.let(pass::verdict)
            CullVerb.NEXT -> pass.next()
            CullVerb.PREVIOUS -> pass.previous()
            CullVerb.EXPAND_STACK -> pass.expandStack()
            CullVerb.COLLAPSE_STACK -> pass.collapseStack()
            CullVerb.SWEEP -> Unit
        }
        publish { it }
        return verdict
    }

    /** Write [verdict], then settle it in the pass, or put the flag back when it failed. */
    suspend fun write(verdict: CullVerdict) {
        try {
            val written =
                verdict.stack?.let { stack ->
                    val members = port.flagStack(stack, verdict.flag)
                    val unlisted = verdict.writeTargets(members).drop(members.size)
                    unlisted.forEach { port.setCull(it, verdict.flag) }
                    members + unlisted
                } ?: verdict.targets.onEach { port.setCull(it, verdict.flag) }
            pass.settle(verdict, written)
            log.info("culling: flagged ${written.size} as ${verdict.flag}")
            publish { it }
        } catch (error: CancellationException) {
            throw error
        } catch (
            @Suppress("TooGenericExceptionCaught") error: Exception,
        ) {
            log.log(Level.WARNING, "culling: a flag write failed", error)
            pass.revert(verdict)
            publish { it.copy(notice = CullNotice.FlagFailed) }
        }
    }

    /** Leave the sweep's confirmation without sweeping. */
    fun dismissSweep() {
        publish { it.copy(isConfirmingSweep = false) }
    }

    /** The user confirmed: move the pass's rejects to the trash, and drop them from the pass. */
    suspend fun confirmSweep() {
        publish { it.copy(isConfirmingSweep = false) }
        val candidates = pass.sweepCandidates
        if (candidates.isEmpty()) return
        val notice =
            try {
                val moved = port.moveToTrash(candidates)
                pass.remove(moved.toSet())
                log.info("culling: swept ${moved.size} rejects to the trash")
                CullNotice.Swept(moved.size)
            } catch (error: SweepIncomplete) {
                log.log(Level.WARNING, "culling: the sweep stopped after ${error.moved.size} assets", error)
                pass.remove(error.moved.toSet())
                CullNotice.SweepFailed
            } catch (error: CancellationException) {
                throw error
            } catch (
                @Suppress("TooGenericExceptionCaught") error: Exception,
            ) {
                log.log(Level.WARNING, "culling: the sweep failed", error)
                CullNotice.SweepFailed
            }
        publish { it.copy(notice = notice) }
    }

    /** The surface has told the user about the current notice. */
    fun consumeNotice() {
        publish { it.copy(notice = null) }
    }

    /** Apply [change] to the state, then redraw everything the pass decides. */
    private fun publish(change: (CullingState) -> CullingState) {
        mutableState.update { previous ->
            change(previous).copy(
                asset = pass.currentAsset?.id,
                flag = pass.currentFlag,
                position = pass.position,
                accepted = CullVerb.entries.filterTo(mutableSetOf(), pass::accepts),
                sweepCount = pass.sweepCandidates.size,
            )
        }
    }

    private companion object {
        val log: Logger = Logger.getLogger("capsule.culling")
    }
}
