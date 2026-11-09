package com.justin13888.capsule.screens

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.data.MuseumObject
import com.justin13888.capsule.data.MuseumRepository
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn

/** Holds the object [DetailScreen] renders, selected by [setId]. */
class DetailViewModel(
    private val museumRepository: MuseumRepository,
) : ViewModel() {
    private val objectId = MutableStateFlow<Int?>(null)

    @OptIn(ExperimentalCoroutinesApi::class)
    val museumObject: StateFlow<MuseumObject?> =
        objectId
            .flatMapLatest { id -> if (id == null) flowOf(null) else museumRepository.getObjectById(id) }
            .stateIn(viewModelScope, SharingStarted.WhileSubscribed(STOP_TIMEOUT_MILLIS), null)

    /** Selects the object to show. */
    fun setId(objectId: Int) {
        this.objectId.value = objectId
    }

    private companion object {
        const val STOP_TIMEOUT_MILLIS = 5_000L
    }
}
