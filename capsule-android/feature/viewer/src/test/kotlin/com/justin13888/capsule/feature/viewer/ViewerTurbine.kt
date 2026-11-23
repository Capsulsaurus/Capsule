package com.justin13888.capsule.feature.viewer

import app.cash.turbine.ReceiveTurbine

/**
 * Waits for the first emission of a given shape, skipping the ones on the way there.
 *
 * The view models publish a genuine sequence — `Loading`, then `Ready` with a resolving
 * fetch, then `Ready` with its outcome — and a test that asserted on emission *counts* would
 * break every time an intermediate state was added or removed for a reason unrelated to what
 * it was checking. Waiting for the shape under test keeps the assertion about the behaviour.
 *
 * Bounded so a state that never arrives fails with a readable message instead of hanging
 * until the suite's own timeout, which reports nothing useful.
 */
internal suspend inline fun <reified T> ReceiveTurbine<*>.awaitFirst(): T {
    repeat(MAX_EMISSIONS) {
        val event = awaitItem()
        if (event is T) return event
    }
    error("no ${T::class.simpleName} emission within $MAX_EMISSIONS states")
}

/** Waits for the viewer to be ready, whatever its fetch is doing. */
internal suspend fun ReceiveTurbine<ViewerUiState>.awaitReady(): ViewerUiState.Ready = awaitFirst()

/**
 * Waits for the viewer's representation fetch to reach a given outcome.
 *
 * Separate from [awaitReady] because the pager becomes ready *before* the fetch resolves — it
 * has the placeholder to draw and does not wait — so a test about the outcome has to keep
 * reading past the first ready state.
 */
internal suspend inline fun <reified T : RepresentationStatus> ReceiveTurbine<ViewerUiState>.awaitStatus(): T {
    repeat(MAX_EMISSIONS) {
        val status = (awaitItem() as? ViewerUiState.Ready)?.representation
        if (status is T) return status
    }
    error("no ${T::class.simpleName} fetch outcome within $MAX_EMISSIONS states")
}

/** How many emissions a helper reads before giving up. Generous, but finite. */
const val MAX_EMISSIONS: Int = 24
