package com.justin13888.capsule

import android.Manifest
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.runtime.LaunchedEffect
import com.justin13888.capsule.alerts.AlertDestination
import com.justin13888.capsule.alerts.AlertNotifications
import java.util.logging.Logger

class MainActivity : ComponentActivity() {
    private val alerts by lazy { (application as CapsuleApp).alerts }

    // The answer, either way, is just another state change: refresh, and the loop arms
    // the alarms or degrades every class to its badge.
    private val notificationPrompt =
        registerForActivityResult(ActivityResultContracts.RequestPermission()) { alerts.coordinator.refresh() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        handleAlertTap(intent)
        setContent {
            // Remove when https://issuetracker.google.com/issues/364713509 is fixed
            LaunchedEffect(isSystemInDarkTheme()) {
                enableEdgeToEdge()
            }
            App()
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
    }

    override fun onPause() {
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
