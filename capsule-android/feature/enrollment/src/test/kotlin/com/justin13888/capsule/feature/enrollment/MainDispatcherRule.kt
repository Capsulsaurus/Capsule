package com.justin13888.capsule.feature.enrollment

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
 * A view model launches on `Dispatchers.Main`, which does not exist on the JVM. A standard
 * dispatcher rather than an unconfined one, because the ceremony's in-flight states — a step
 * that is running, a code that is being issued, a ten-minute countdown — are most of what
 * these screens show, and an implementation that ran to completion inside the call that
 * started it would skip straight past them.
 */
class MainDispatcherRule(
    private val dispatcher: TestDispatcher = StandardTestDispatcher(),
) : TestWatcher() {
    override fun starting(description: Description) = Dispatchers.setMain(dispatcher)

    override fun finished(description: Description) = Dispatchers.resetMain()
}
