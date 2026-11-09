package com.justin13888.capsule.screens

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.data.MuseumObject
import com.justin13888.capsule.data.MuseumRepository
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.stateIn

/** Holds the objects [ListScreen] renders. */
class ListViewModel(
    museumRepository: MuseumRepository,
) : ViewModel() {
    val objects: StateFlow<List<MuseumObject>> =
        museumRepository
            .getObjects()
            .stateIn(viewModelScope, SharingStarted.WhileSubscribed(STOP_TIMEOUT_MILLIS), emptyList())

    private companion object {
        const val STOP_TIMEOUT_MILLIS = 5_000L
    }
}
