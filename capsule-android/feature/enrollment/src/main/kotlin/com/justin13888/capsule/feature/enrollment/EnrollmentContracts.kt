package com.justin13888.capsule.feature.enrollment

import androidx.compose.runtime.Immutable
import kotlinx.collections.immutable.ImmutableList

/**
 * The five things that have to happen before an account exists.
 *
 * Ordered, and each one independent: the doc's ceremony is a sequence, but a failure in one
 * step is not a failure of the whole — the directory upload can be deferred and the default
 * album can be created later — so the type carries the step rather than a single "setup
 * failed" flag.
 */
enum class EnrollmentStep {
    /** Draw the account master key and escrow it under the recovery passphrase. */
    MasterKey,

    /** Generate the user identity key that every device key cross-signs to. */
    IdentityKey,

    /** Generate this device's non-exportable keys inside the secure element. */
    DeviceKeys,

    /** Publish the identity-signed device directory. */
    DeviceDirectory,

    /** Establish the writable album the first import lands in. */
    DefaultAlbum,
}

/** Where one [EnrollmentStep] has got to. */
enum class StepStatus {
    /** Not started. */
    Pending,

    /** Running now. */
    Running,

    /** Finished. */
    Done,

    /** Failed, and retryable on its own. */
    Failed,

    /**
     * Failed in a way that does not block setup.
     *
     * The directory upload and the default album are both recoverable later — the keys are
     * already valid, and the album's identifier is derivable from the master key — so
     * treating them as hard failures would stop a ceremony that has, in fact, succeeded.
     */
    Deferred,
}

/**
 * A one-time enrollment code, in the two forms the doc requires it to be offered in.
 *
 * Two fields rather than one rendered twice, because they are deliberately different: the
 * square code carries the full payload, and the text fallback trades entropy for something a
 * human can read down a phone line. The shorter one is safe only because it never stands
 * alone — single-use, ten minutes, rate-limited, and never the thing that protects the
 * channel — which the screen says out loud.
 */
@Immutable
data class EnrollmentCode(
    /** The full-entropy payload the square code carries. */
    val payload: String,
    /** The deliberately shorter, transcribable fallback. */
    val friendlyDigits: String,
)

/**
 * What one end of the ceremony is, as the other end must be able to check it.
 *
 * The safety code alone would not catch a relay that swapped in a different device, so the
 * comparison screen shows the model and a short key fingerprint beside it and asks the user
 * to confirm both.
 */
@Immutable
data class DeviceIdentity(
    /** The hardware, as the device reports itself. */
    val model: String,
    /** A short fingerprint of the device signing key. */
    val keyFingerprint: String,
)

/**
 * The channel-transcript digest both devices display.
 *
 * Held as chunks rather than as one string so that both ends are laid out identically by
 * construction. A human comparing two differently-grouped renderings of the same digits is a
 * human who will approve a mismatch.
 */
@Immutable
data class SafetyCode(
    /** The fixed-length groups, in order. */
    val chunks: ImmutableList<String>,
) {
    /** The whole code, for announcing and for comparing against what a peer reports. */
    val text: String get() = chunks.joinToString(SEPARATOR)

    companion object {
        /** What the groups are joined with, in both the display and the spoken form. */
        const val SEPARATOR: String = "-"

        /** Splits a joined code back into its groups. */
        fun parse(text: String): List<String> = text.split(SEPARATOR)
    }
}

/**
 * The hardware that holds this device's signing key.
 *
 * A port because its refusal is a state the UI has to render and a JVM test has to be able to
 * cause. Shipping secure elements do refuse, rarely; a ceremony that assumed success would
 * fail somewhere less explicable.
 */
interface SecureElement {
    /**
     * Generates the device key pair and returns its short fingerprint.
     *
     * Throws when the hardware refuses — the classical halves are non-exportable, so there is
     * no software fallback that would produce the same guarantee, and pretending otherwise
     * would be worse than failing.
     */
    suspend fun generateDeviceKeys(): String
}

/**
 * Fresh proof of physical control of this device.
 *
 * Separate from the session, deliberately: a stolen session token must not be enough to
 * enroll a rogue device, so issuing a code is gated on something an attacker holding only a
 * token cannot produce.
 */
interface EnrollmentAuthorization {
    /** Raises the platform prompt. `false` means the user declined or it failed. */
    suspend fun confirm(): Boolean
}

/** The account-setup ceremony the first device on an account runs. */
interface FirstDeviceCeremony {
    /** Runs one step. Throws whatever that step can fail with. */
    suspend fun perform(step: EnrollmentStep)

    /**
     * The passphrase the master key is escrowed under.
     *
     * Returned rather than stored: this is the only copy the user will ever see, it is never
     * persisted, and the ceremony holds it only for as long as the screen showing it lives.
     */
    fun recoveryPassphrase(): RecoveryPassphrase
}

/** Issues the single-use code the other device reads. */
interface EnrollmentCodeIssuer {
    /** Issues a code, which the server deletes on redemption or expiry. */
    suspend fun issue(): EnrollmentCode
}

/**
 * Reading a code off the camera.
 *
 * A port so the decode is substitutable: the preview underneath it is a real CameraX one, but
 * what it "reads" is a fixture, and no barcode library is pulled into the app for it.
 */
interface EnrollmentCodeReader {
    /** Suspends until a code is in frame. */
    suspend fun awaitCode(): String
}
