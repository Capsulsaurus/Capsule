package com.justin13888.capsule.security

import android.app.Activity
import android.app.KeyguardManager
import android.content.Context
import android.content.Intent
import android.hardware.biometrics.BiometricPrompt
import android.os.Build
import android.os.CancellationSignal
import androidx.activity.compose.LocalActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.RequiresApi
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.res.stringResource
import androidx.core.content.getSystemService
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.GatedView
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlin.coroutines.resume

/**
 * The real prompt: platform biometrics where they exist, the device credential otherwise.
 *
 * Uses the framework `android.hardware.biometrics.BiometricPrompt` rather than the AndroidX
 * compat wrapper. The wrapper would drag `androidx.fragment` and `appcompat` onto an
 * otherwise pure-Compose application, and pins them at versions from 2020; the framework
 * class covers API 28 and up, which is all but the two oldest releases this app supports, and
 * those fall through to the system credential screen.
 *
 * The biometric-to-credential fallback is entirely this adapter's concern. [LocalAuthGate]
 * sees a boolean, exactly as the platform-agnostic contract intends.
 */
@Composable
fun rememberPlatformLocalAuthPrompt(): LocalAuthPrompt {
    val activity = LocalActivity.current
    val title = stringResource(R.string.android_shell_gate_prompt_title)
    val cancel = stringResource(R.string.android_shell_gate_prompt_cancel)
    val body = stringResource(R.string.android_shell_gate_body)

    val pending = remember { PendingCredentialResult() }
    val credential =
        rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {
            pending.complete(it.resultCode == Activity.RESULT_OK)
        }

    return remember(activity, title, cancel, body) {
        object : LocalAuthPrompt {
            override suspend fun authenticate(view: GatedView): Boolean =
                when {
                    activity == null -> false
                    Build.VERSION.SDK_INT >= Build.VERSION_CODES.P ->
                        authenticateWithBiometrics(activity, title, body, cancel)

                    else -> {
                        val intent = confirmCredentialIntent(activity, title, body)
                        if (intent == null) false else pending.awaiting { credential.launch(intent) }
                    }
                }
        }
    }
}

/**
 * The single in-flight confirm-credential result.
 *
 * One slot rather than a queue because the gate covers the whole window while it is prompting,
 * so a second request cannot be started until this one has resolved.
 */
private class PendingCredentialResult {
    private var deferred: CompletableDeferred<Boolean>? = null

    /** Runs [launch] and suspends until the activity result arrives. */
    suspend fun awaiting(launch: () -> Unit): Boolean {
        val slot = CompletableDeferred<Boolean>()
        deferred = slot
        launch()
        return slot.await()
    }

    /** Hands the result to whoever is waiting, if anyone still is. */
    fun complete(authenticated: Boolean) {
        deferred?.complete(authenticated)
        deferred = null
    }
}

/**
 * Bridges the framework prompt's callback API onto a suspend function.
 *
 * Every terminal outcome resumes with a boolean, including the error and negative-button
 * paths: refusing to distinguish "cancelled" from "failed" here is deliberate, since the gate
 * must leave the view locked in both cases and a caller that branched would be re-deciding
 * policy the platform already settled.
 */
@RequiresApi(Build.VERSION_CODES.P)
private suspend fun authenticateWithBiometrics(
    context: Context,
    title: String,
    subtitle: String,
    negative: String,
): Boolean =
    suspendCancellableCoroutine { continuation ->
        val signal = CancellationSignal()
        continuation.invokeOnCancellation { signal.cancel() }
        val executor = context.mainExecutor
        val prompt =
            BiometricPrompt
                .Builder(context)
                .setTitle(title)
                .setSubtitle(subtitle)
                .setNegativeButton(negative, executor) { _, _ ->
                    if (continuation.isActive) continuation.resume(false)
                }.build()
        prompt.authenticate(
            signal,
            executor,
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    if (continuation.isActive) continuation.resume(true)
                }

                override fun onAuthenticationError(
                    errorCode: Int,
                    errString: CharSequence,
                ) {
                    if (continuation.isActive) continuation.resume(false)
                }
            },
        )
    }

/**
 * The pre-API-28 path: the system's own confirm-credential screen.
 *
 * Returns null when the device has no secure lock screen at all, which the gate reads as "did
 * not authenticate". Leaving the view locked on an unsecured device is the honest outcome —
 * there is no credential to be fresh about, and silently opening it would make SR1 mean
 * nothing exactly where it is least protected.
 */
@Suppress("DEPRECATION") // The replacement is BiometricPrompt, which this branch cannot use.
private fun confirmCredentialIntent(
    context: Context,
    title: String,
    description: String,
): Intent? =
    context
        .getSystemService<KeyguardManager>()
        ?.takeIf { it.isDeviceSecure }
        ?.createConfirmDeviceCredentialIntent(title, description)
