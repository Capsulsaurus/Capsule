package com.justin13888.capsule.workspace

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

/** Keeps a [SignInModel] across configuration changes. */
class SignInViewModel(
    sessions: ServerSessions,
) : ViewModel() {
    private val model = SignInModel(sessions)

    val state: StateFlow<SignInState> = model.state

    fun updateServer(value: String) = model.updateServer(value)

    fun updateHandle(value: String) = model.updateHandle(value)

    fun updatePassword(value: String) = model.updatePassword(value)

    fun updateCode(value: String) = model.updateCode(value)

    fun submit() {
        viewModelScope.launch { model.submit() }
    }
}
