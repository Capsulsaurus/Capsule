package com.justin13888.capsule.di

import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.shell.CapsuleShellState
import org.koin.dsl.module

/**
 * What the shell itself owns.
 *
 * Both singletons are process-wide by nature rather than by convenience. [CapsuleShellState]
 * is where a transport that receives a `426`, or a connectivity monitor that drops to
 * offline, reports a condition that is true of the whole app; scoping it to a screen would
 * mean the discovery is lost the moment that screen leaves. [LocalAuthGate] holds the SR1
 * grace window, and a per-screen gate would hand out a fresh five minutes every time the
 * user navigated away and back — which is the same as having no window at all.
 */
val shellModule =
    module {
        single { CapsuleShellState() }
        single { LocalAuthGate() }
    }
