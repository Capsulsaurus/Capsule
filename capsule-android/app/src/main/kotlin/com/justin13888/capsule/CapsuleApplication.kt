package com.justin13888.capsule

import android.app.Application
import android.content.pm.ApplicationInfo
import com.justin13888.capsule.di.capsuleModules
import org.koin.android.ext.koin.androidContext
import org.koin.android.ext.koin.androidLogger
import org.koin.core.context.startKoin
import org.koin.core.logger.Level

/**
 * Application entry point, and the one place the object graph is assembled.
 *
 * Koin is started here rather than lazily on first injection so that a missing or ambiguous
 * binding fails at launch, loudly, instead of at whatever screen happened to need it. The
 * module list is deliberately the only thing this class knows: adding a feature area means
 * adding a module to `capsuleModules`, never editing this file.
 */
class CapsuleApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        startKoin {
            // Koin's own logging goes through Android's log rather than being silenced, so a
            // resolution failure in a release build is still recoverable from a bug report.
            androidLogger(if (isDebuggable()) Level.DEBUG else Level.ERROR)
            androidContext(this@CapsuleApplication)
            modules(capsuleModules)
        }
    }

    /**
     * Whether this is a debuggable build.
     *
     * Read from the manifest flag rather than `BuildConfig.DEBUG` so the app does not have to
     * turn on `BuildConfig` generation for one boolean.
     */
    private fun isDebuggable(): Boolean = applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0
}
