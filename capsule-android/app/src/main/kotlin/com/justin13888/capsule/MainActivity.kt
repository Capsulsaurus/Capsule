package com.justin13888.capsule

import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.core.net.toUri
import com.justin13888.capsule.security.LocalAuthGate
import org.koin.android.ext.android.inject

/**
 * The single activity; every surface above it is Compose.
 *
 * It stays a `ComponentActivity` rather than a `FragmentActivity`: nothing in this app is a
 * fragment, and the one library that would have demanded one — the AndroidX biometric
 * wrapper — is deliberately not a dependency (see `PlatformLocalAuthPrompt`).
 *
 * `configChanges` in the manifest keeps the activity alive across rotation, fold and window
 * resize. That is not an optimisation: the adaptive shell reads the window size on every
 * frame, so a recreate would throw away the back stack and the pane state on every fold, and
 * the layout would still be correct — just after a flicker and a lost scroll position.
 */
class MainActivity : ComponentActivity() {
    private val localAuthGate: LocalAuthGate by inject()

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        setContent {
            CapsuleApp(onUpdateApp = ::openStoreListing)
        }
    }

    /**
     * Drops every fresh-local-auth grant when the app leaves the foreground.
     *
     * SR1's grace window is about the user who is still holding the phone. Backgrounding is
     * the clearest signal available that they are not, and the window is short enough that
     * re-authenticating on return is a smaller cost than leaving Hidden open on a device the
     * user just handed to somebody.
     */
    override fun onStop() {
        super.onStop()
        localAuthGate.revokeAll()
    }

    /**
     * Sends the user to this app's store listing.
     *
     * Reached only from the protocol-mismatch hard block, where the app has been told by the
     * server that it is too old to write anything. Falls back to the web listing when no store
     * app can handle the market URI — a sideloaded or de-Googled install is exactly the case
     * where a stranded user most needs the link to work.
     */
    private fun openStoreListing() {
        val market = "market://details?id=$packageName".toUri()
        val web = "https://play.google.com/store/apps/details?id=$packageName".toUri()
        if (!startViewing(market)) startViewing(web)
    }

    private fun startViewing(uri: Uri): Boolean =
        try {
            startActivity(Intent(Intent.ACTION_VIEW, uri))
            true
        } catch (_: ActivityNotFoundException) {
            false
        }
}
