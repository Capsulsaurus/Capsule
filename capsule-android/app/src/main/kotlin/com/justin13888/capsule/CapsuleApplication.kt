package com.justin13888.capsule

import android.app.Application

/**
 * Application entry point.
 *
 * The dependency graph is wired here in a later slice; today the app runs entirely on
 * mock repositories, so there is nothing process-wide to initialise yet.
 */
class CapsuleApplication : Application()
