package com.justin13888.capsule.feature.onboarding

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.rules.TestWatcher
import org.junit.runner.Description

/**
 * Puts `viewModelScope` on a dispatcher the test controls.
 *
 * A view model launches on `Dispatchers.Main`, which does not exist on the JVM, so without
 * this every test that touches one would die on a missing main looper rather than on the
 * behaviour it meant to check. [StandardTestDispatcher] rather than the unconfined one, so
 * that the in-flight states — "probing", "working" — are actually observable instead of being
 * skipped over by an implementation that ran to completion inside the call that started it.
 */
class MainDispatcherRule(
    private val dispatcher: TestDispatcher = StandardTestDispatcher(),
) : TestWatcher() {
    override fun starting(description: Description) = Dispatchers.setMain(dispatcher)

    override fun finished(description: Description) = Dispatchers.resetMain()
}
