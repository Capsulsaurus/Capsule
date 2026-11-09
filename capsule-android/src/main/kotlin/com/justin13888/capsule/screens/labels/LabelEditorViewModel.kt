package com.justin13888.capsule.screens.labels

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.screens.domain.LabelPort
import com.justin13888.capsule.screens.domain.RegionId
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

/**
 * Keeps a [LabelEditorModel] across configuration changes and runs its writes on the
 * main thread, which is the confinement the model is written for. Loads the subject
 * once when created.
 */
class LabelEditorViewModel(
    subject: LabelSubject,
    port: LabelPort,
) : ViewModel() {
    private val model = LabelEditorModel(subject, port)

    /** What the editor draws. */
    val state: StateFlow<LabelEditorState> = model.state

    /** What the editor is open on. */
    val subject: LabelSubject get() = model.subject

    init {
        viewModelScope.launch { model.load() }
    }

    /** Add [text] to the current target, calling [onStored] only once the port stores it. */
    fun addLabel(
        text: String,
        onStored: () -> Unit = {},
    ) {
        viewModelScope.launch { if (model.addLabel(text)) onStored() }
    }

    fun removeLabel(text: String) {
        viewModelScope.launch { model.removeLabel(text) }
    }

    fun selectRegion(region: RegionId?) = model.selectRegion(region)

    fun startDrawing(kind: RegionDraft.Kind) = model.startDrawing(kind)

    fun updateDraft(draft: RegionDraft) = model.updateDraft(draft)

    fun cancelDrawing() = model.cancelDrawing()

    /**
     * Save the draft with [firstLabel] as its first label, or none when it is blank,
     * calling [onStored] only once the port stores it.
     */
    fun finishDrawing(
        firstLabel: String,
        onStored: () -> Unit = {},
    ) {
        val labels = if (firstLabel.isBlank()) emptyList() else listOf(firstLabel)
        viewModelScope.launch { if (model.finishDrawing(labels)) onStored() }
    }

    fun deleteSelectedRegion() {
        viewModelScope.launch { model.deleteSelectedRegion() }
    }
}
