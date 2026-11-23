package com.justin13888.capsule.feature.enrollment

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.model.ErrorCode
import kotlinx.collections.immutable.ImmutableList
import kotlinx.collections.immutable.ImmutableMap
import kotlinx.collections.immutable.persistentMapOf
import kotlinx.collections.immutable.toImmutableList
import java.time.Duration

/** What kind of thing went wrong, where the difference changes what the user is told. */
enum class EnrollmentFailureKind {
    /** The secure element refused to generate. No software fallback exists, by design. */
    SecureElementRefused,

    /**
     * The server could not be reached.
     *
     * Explicitly not a failure of the ceremony: the keys are already generated and valid, so
     * the only consequence is that this device is invisible to the others until the upload
     * lands.
     */
    ServerUnreachable,

    /** The server refused, and said why with a catalogue code. */
    Refused,
}

/**
 * A failure reduced to what a screen may show: a kind, and a catalogue code where one exists.
 *
 * The transport status never appears. The code is nullable because a device-local failure
 * genuinely has none, and inventing one would be a catalogue key that means nothing.
 */
@Immutable
data class EnrollmentFailure(
    /** What kind of failure this is. */
    val kind: EnrollmentFailureKind,
    /** The catalogue code to localize, or `null` for a device-local failure. */
    val code: ErrorCode? = null,
) {
    internal companion object {
        /** Classifies whatever [step] threw. */
        fun of(
            step: EnrollmentStep,
            throwable: Throwable,
        ): EnrollmentFailure {
            val code = (throwable as? CapsuleError)?.code
            val kind =
                when {
                    step == EnrollmentStep.DeviceKeys && throwable is LocalError ->
                        EnrollmentFailureKind.SecureElementRefused

                    throwable is LocalError.Offline -> EnrollmentFailureKind.ServerUnreachable
                    else -> EnrollmentFailureKind.Refused
                }
            return EnrollmentFailure(kind = kind, code = code)
        }

        /** Classifies a failure that did not come from a numbered setup step. */
        fun of(throwable: Throwable): EnrollmentFailure =
            EnrollmentFailure(
                kind =
                    if (throwable is LocalError.Offline) {
                        EnrollmentFailureKind.ServerUnreachable
                    } else {
                        EnrollmentFailureKind.Refused
                    },
                code = (throwable as? CapsuleError)?.code,
            )
    }
}

/** One step of the account ceremony and where it has got to. */
@Immutable
data class StepState(
    /** Which step. */
    val step: EnrollmentStep,
    /** Its status. */
    val status: StepStatus = StepStatus.Pending,
)

/** Which part of first-device setup is on screen. */
enum class SetupPhase {
    /** The numbered steps are running. */
    Steps,

    /** The passphrase is revealed and the user is writing it down. */
    Passphrase,

    /** The type-back gate. Setup does not complete until it is passed. */
    TypeBack,

    /** Everything is done. */
    Complete,
}

/** Everything the first-device setup screens draw. */
@Immutable
data class FirstDeviceUiState(
    /** The five steps, in order. */
    val steps: ImmutableList<StepState> = INITIAL_STEPS,
    /** Which part of the ceremony is showing. */
    val phase: SetupPhase = SetupPhase.Steps,
    /** A failure that stopped the ceremony, or `null`. */
    val failure: EnrollmentFailure? = null,
    /** The passphrase, held only while the reveal and the gate need it. */
    val passphrase: RecoveryPassphrase? = null,
    /** Which words the gate is asking for. */
    val challenge: TypeBackChallenge? = null,
    /** What the user has typed back, by position. */
    val answers: ImmutableMap<Int, String> = persistentMapOf(),
    /** Whether the last type-back attempt was wrong. */
    val typeBackRejected: Boolean = false,
    /** Whether the passphrase has just been put on the clipboard. */
    val copied: Boolean = false,
) {
    /** Steps that failed in a way a later attempt recovers from. */
    val deferred: ImmutableList<StepState> =
        steps.filter { it.status == StepStatus.Deferred }.toImmutableList()

    /** Whether the ceremony stopped and cannot go on until something changes. */
    val isBlocked: Boolean get() = steps.any { it.status == StepStatus.Failed }

    /** Whether every step has reached a terminal status. */
    val stepsSettled: Boolean
        get() = steps.all { it.status == StepStatus.Done || it.status == StepStatus.Deferred }

    /** How far through the numbered steps the ceremony is, as a fraction. */
    val progress: Float
        get() {
            val settled = steps.count { it.status != StepStatus.Pending && it.status != StepStatus.Running }
            return settled / steps.size.toFloat()
        }

    /** Whether the typed answers are complete enough to be worth checking. */
    val canSubmitTypeBack: Boolean
        get() = challenge?.positions?.all { answers[it].orEmpty().isNotBlank() } == true

    private companion object {
        val INITIAL_STEPS: ImmutableList<StepState> =
            EnrollmentStep.entries.map { StepState(it) }.toImmutableList()
    }
}

/** How adding this device to an existing account ended. */
sealed interface EnrollmentOutcome {
    /** The device is in the directory and every other device can see it. */
    data class Added(
        /** What the directory now calls it. */
        val deviceName: String,
    ) : EnrollmentOutcome

    /**
     * The keys are valid but the directory has not uploaded yet.
     *
     * A separate outcome rather than a failure, because nothing is lost: the client retries
     * with backoff, and until it succeeds the only consequence is invisibility to peers.
     */
    data object PendingDirectoryUpload : EnrollmentOutcome
}

/** The entry screen of the cross-device ceremony. */
@Immutable
data class EnrollmentStartUiState(
    /** Whether fresh physical control of this device has been proven. */
    val isAuthorized: Boolean = false,
    /** Whether the prompt is open. */
    val isConfirming: Boolean = false,
    /** Whether the last prompt was declined or failed. */
    val wasDeclined: Boolean = false,
)

/** The code-issuing side. */
@Immutable
data class EnrollmentCodeUiState(
    /** The issued code, or `null` before one exists. */
    val code: EnrollmentCode? = null,
    /** Whether a code is being issued. */
    val isIssuing: Boolean = false,
    /** How long the current code has left. */
    val remaining: Duration = LIFETIME,
    /** A failure issuing the code, or `null`. */
    val failure: EnrollmentFailure? = null,
) {
    /** Whether the ten-minute window has closed on the code that is showing. */
    val isExpired: Boolean get() = code != null && (remaining.isZero || remaining.isNegative)

    companion object {
        /** The window the doc fixes. Single-use and rate-limited on top of it. */
        val LIFETIME: Duration = Duration.ofMinutes(10)
    }
}

/** The code-reading side. */
@Immutable
data class ScanUiState(
    /** Whether the camera may be opened. */
    val hasCameraPermission: Boolean = false,
    /** Whether the decode is running. */
    val isReading: Boolean = false,
    /** What the user is typing into the fallback field. */
    val manualCode: String = "",
    /** A refusal of a code that was read or typed, or `null`. */
    val failure: EnrollmentFailure? = null,
    /** The code that was accepted, or `null`. */
    val acceptedCode: String? = null,
)

/** The safety-code comparison, which is the whole of the channel's integrity. */
@Immutable
data class SafetyCodeUiState(
    /** The chunked code both devices show. */
    val safetyCode: SafetyCode? = null,
    /** What this device is. */
    val thisDevice: DeviceIdentity? = null,
    /** What the device being added is. */
    val otherDevice: DeviceIdentity? = null,
    /** Whether the user has explicitly said the codes and the identity match. */
    val isAcknowledged: Boolean = false,
    /** Whether a confirmation was attempted without the acknowledgement. */
    val acknowledgementMissing: Boolean = false,
    /** Whether the ceremony is completing. */
    val isConfirming: Boolean = false,
    /** Whether the ceremony was aborted, by the user or by a refusal. */
    val isAborted: Boolean = false,
    /** A failure completing the ceremony, or `null`. */
    val failure: EnrollmentFailure? = null,
    /** How it ended, or `null` while it has not. */
    val outcome: EnrollmentOutcome? = null,
) {
    /** Whether confirming would do anything. */
    val canConfirm: Boolean get() = safetyCode != null && !isConfirming && !isAborted
}

/** The last screen of the cross-device ceremony. */
@Immutable
data class EnrollmentCompleteUiState(
    /** How it ended, or `null` when this screen was reached without an outcome. */
    val outcome: EnrollmentOutcome? = null,
)
