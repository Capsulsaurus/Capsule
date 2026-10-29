package com.justin13888.capsule

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.MotionEvent
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import com.justin13888.capsule.alerts.AlertDestination
import com.justin13888.capsule.alerts.AlertNotifications
import com.justin13888.capsule.i18n.AppCompatLanguageStore
import com.justin13888.capsule.i18n.AppLanguagePicker
import com.justin13888.capsule.i18n.LanguagePickerButton
import com.justin13888.capsule.i18n.LocalesConfig
import com.justin13888.capsule.perf.FrameMonitor
import com.justin13888.capsule.perf.InputToFrameTracer
import com.justin13888.capsule.workspace.LibraryHost
import java.util.logging.Logger

// AppCompatActivity, because below Android 13 AppCompat applies the in-app language only to
// its own activities.
class MainActivity : AppCompatActivity() {
    private val alerts by lazy { (application as CapsuleApp).alerts }

    private val workspace by lazy { (application as CapsuleApp).workspace }

    // A language change renames the alerts channel at once, rather than at the next alert.
    private val languageStore by lazy { AppCompatLanguageStore(this, onApplied = AlertNotifications::ensureChannel) }

    private val languagePicker by lazy { AppLanguagePicker(languageStore, LocalesConfig.tags(this)) }

    // The answer, either way, is just another state change: refresh, and the loop arms
    // the alarms or degrades every class to its badge.
    private val notificationPrompt =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { alerts.coordinator.refresh() }

    // Field jank telemetry, and the trace section the parity benchmark reads input latency from.
    private lateinit var frameMonitor: FrameMonitor
    private val inputToFrame = InputToFrameTracer()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        handleAlertTap(intent)
        if (AppLanguagePicker.isNeeded(Build.VERSION.SDK_INT)) {
            // AppCompat has loaded its stored choice by now; reading it keeps the mirror that
            // alert notifications read from outside an activity level with it.
            languageStore.applied()
        }
        setContent {
            // Remove when https://issuetracker.google.com/issues/364713509 is fixed
            LaunchedEffect(isSystemInDarkTheme()) {
                enableEdgeToEdge()
            }
            // The screens run on this device's library, opened (or created on first launch)
            // by the workspace graph, and on the server session it signs in to.
            LibraryHost(workspace)
            if (AppLanguagePicker.isNeeded(Build.VERSION.SDK_INT)) {
                LanguageEntry()
            }
        }
        frameMonitor = FrameMonitor(window)
    }

    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        // A tap lands on release; that is the input whose answering frame the contract bounds.
        if (event.actionMasked == MotionEvent.ACTION_UP) inputToFrame.onTap()
        return super.dispatchTouchEvent(event)
    }

    /**
     * The in-app language picker, which Android 12 and below need because they have no
     * per-app language setting. The app has no settings screen yet, so it sits over the top
     * corner until one exists.
     */
    @Composable
    private fun LanguageEntry() {
        MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
            Box(
                modifier = Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing),
                contentAlignment = Alignment.TopEnd,
            ) {
                // The activity handles locale changes itself (configChanges), so AppCompat does
                // not recreate it; recreating re-renders every string in the new language.
                LanguagePickerButton(picker = languagePicker, onChosen = ::recreate)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleAlertTap(intent)
    }

    override fun onResume() {
        super.onResume()
        // The prompt needs a visible activity, and is only started when a class has
        // something to arm - never at launch for its own sake.
        alerts.permission.launcher = { notificationPrompt.launch(Manifest.permission.POST_NOTIFICATIONS) }
        alerts.coordinator.refresh()
        frameMonitor.resume()
    }

    override fun onPause() {
        frameMonitor.pause()
        alerts.permission.launcher = null
        super.onPause()
    }

    /**
     * A tapped alert's destination. The scaffold app has no screen for any of them yet,
     * so it is only recorded; the re-wrap is already distinct from the routine check.
     */
    private fun handleAlertTap(intent: Intent?) {
        val name = intent?.getStringExtra(AlertNotifications.EXTRA_DESTINATION) ?: return
        val destination = AlertDestination.entries.firstOrNull { it.name == name } ?: return
        Logger.getLogger("capsule.alerts").info("alert tapped: $destination")
    }
}
