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

    init {
        viewModelScope.launch { model.load() }
    }

    /** Run [verb]; a verdict shows at once and its write follows. */
    fun perform(verb: CullVerb) {
        val verdict = model.perform(verb) ?: return
        viewModelScope.launch { model.write(verdict) }
    }

    fun confirmSweep() {
        viewModelScope.launch { model.confirmSweep() }
    }

    fun dismissSweep() = model.dismissSweep()

    fun consumeNotice() = model.consumeNotice()
}
