package com.justin13888.capsule.screens.culling

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

/**
 * Keeps a [CullingModel] across configuration changes and runs it on the main thread, which
 * is the confinement the model is written for. Reads the pass once when created.
 */
class CullingViewModel(
    port: CullPort,
) : ViewModel() {
    private val model = CullingModel(port)

    /** What the surface draws. */
    val state: StateFlow<CullingState> = model.state

    /** Every store write, in the order the user made it. */
    private val writes = WriteQueue()
    private val handOff = CullingHandOff(model, writes::submit)

    init {
        viewModelScope.launch { model.load() }
        viewModelScope.launch { writes.run() }
    }

    /** Run [verb]; a verdict shows at once and its write follows. */
    fun perform(verb: CullVerb) {
        handOff.perform(verb)
    }

    fun confirmSweep() {
        handOff.confirmSweep()
    }

    override fun onCleared() {
        writes.close()
    }

    fun dismissSweep() = model.dismissSweep()

    fun consumeNotice() = model.consumeNotice()
}

/**
 * What the surface hands the store: [perform] shows a verdict at once and queues its write,
 * [confirmSweep] queues the sweep, both through [submit], which is [WriteQueue.submit] in the app. Split from the
 * ViewModel so the hand-off runs on the JVM without Android's main dispatcher.
 */
class CullingHandOff(
    private val model: CullingModel,
    private val submit: (suspend () -> Unit) -> Unit,
) {
    /** Run [verb]; a verdict shows at once and its write follows. */
    fun perform(verb: CullVerb) {
        val verdict = model.perform(verb) ?: return
        submit { model.write(verdict) }
    }

    /** Queue the sweep of the pass's rejects. */
    fun confirmSweep() {
        submit { model.confirmSweep() }
    }
}
