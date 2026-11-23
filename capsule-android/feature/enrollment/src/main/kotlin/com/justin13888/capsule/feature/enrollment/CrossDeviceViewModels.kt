package com.justin13888.capsule.feature.enrollment

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.mock.EnrollmentGateway
import com.justin13888.capsule.core.model.PlatformTag
import kotlinx.collections.immutable.toImmutableList
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import java.time.Duration

/**
 * The gate in front of issuing a code.
 *
 * The doc is explicit that a valid session token is not enough to enroll a device: an attacker
 * who exfiltrated one remotely must not be able to add a rogue device, so initiation is bound
 * to physical control of an already-trusted device. That is why this screen exists at all,
 * rather than the ceremony starting at the code.
 */
class EnrollmentStartViewModel(
    private val authorization: EnrollmentAuthorization,
    private val session: EnrollmentSession,
) : ViewModel() {
    private val mutableState =
        MutableStateFlow(EnrollmentStartUiState(isAuthorized = session.progress.value.isAuthorized))

    /** Everything the entry screen draws. */
    val state: StateFlow<EnrollmentStartUiState> = mutableState.asStateFlow()

    /** Raises the platform prompt. */
    fun confirm() {
        if (mutableState.value.isConfirming) return
        mutableState.update { it.copy(isConfirming = true, wasDeclined = false) }
        viewModelScope.launch {
            val confirmed = runCatching { authorization.confirm() }.getOrDefault(false)
            if (confirmed) session.authorized()
            mutableState.update {
                it.copy(isConfirming = false, isAuthorized = confirmed, wasDeclined = !confirmed)
            }
        }
    }
}

/**
 * Issuing the code and counting it down.
 *
 * The countdown is driven by the view model rather than by the composition because expiry is a
 * state change, not an animation: at zero the code stops being offered and the screen says so.
 * A screen-owned timer would keep counting a code that the server had already deleted.
 */
class EnrollmentCodeViewModel(
    private val issuer: EnrollmentCodeIssuer,
    private val session: EnrollmentSession,
) : ViewModel() {
    private val mutableState = MutableStateFlow(EnrollmentCodeUiState())

    /** Everything the code screen draws. */
    val state: StateFlow<EnrollmentCodeUiState> = mutableState.asStateFlow()

    private var countdown: Job? = null

    init {
        issue()
    }

    /** Issues a fresh code, replacing and expiring whatever was showing. */
    fun issue() {
        countdown?.cancel()
        mutableState.update {
            EnrollmentCodeUiState(isIssuing = true, remaining = EnrollmentCodeUiState.LIFETIME)
        }
        viewModelScope.launch {
            runCatching { issuer.issue() }
                .onSuccess { code ->
                    session.issued(code)
                    mutableState.update { it.copy(isIssuing = false, code = code) }
                    startCountdown()
                }.onFailure { throwable ->
                    mutableState.update {
                        it.copy(isIssuing = false, failure = EnrollmentFailure.of(throwable))
                    }
                }
        }
    }

    private fun startCountdown() {
        countdown =
            viewModelScope.launch {
                while (!mutableState.value.remaining.isZero && !mutableState.value.remaining.isNegative) {
                    delay(TICK.toMillis())
                    mutableState.update { it.copy(remaining = it.remaining.minus(TICK)) }
                }
            }
    }

    private companion object {
        /** One second, so the countdown reads as a countdown rather than as a jump. */
        val TICK: Duration = Duration.ofSeconds(1)
    }
}

/**
 * Reading the code on the other device.
 *
 * The decode is mocked over a real preview, and the typed fallback goes through exactly the
 * same acceptance path — a scanner whose manual entry took a different route would be a
 * second implementation of the only check that matters here.
 */
class EnrollmentScanViewModel(
    private val reader: EnrollmentCodeReader,
    private val session: EnrollmentSession,
) : ViewModel() {
    private val mutableState = MutableStateFlow(ScanUiState())

    /** Everything the scanner draws. */
    val state: StateFlow<ScanUiState> = mutableState.asStateFlow()

    private var reading: Job? = null

    /** Records whether the camera may be opened, and starts reading as soon as it may. */
    fun cameraPermissionChanged(granted: Boolean) {
        mutableState.update { it.copy(hasCameraPermission = granted) }
        if (granted) startReading()
    }

    /** Records what the user is typing into the fallback field. */
    fun manualCodeChanged(value: String) = mutableState.update { it.copy(manualCode = value, failure = null) }

    /** Submits a hand-typed code down the same path a scanned one takes. */
    fun submitManualCode() {
        val typed = mutableState.value.manualCode.trim()
        if (typed.isEmpty()) return
        accept(typed)
    }

    private fun startReading() {
        if (reading?.isActive == true) return
        mutableState.update { it.copy(isReading = true, failure = null) }
        reading =
            viewModelScope.launch {
                runCatching { reader.awaitCode() }
                    .onSuccess { accept(it) }
                    .onFailure { throwable ->
                        mutableState.update {
                            it.copy(isReading = false, failure = EnrollmentFailure.of(throwable))
                        }
                    }
            }
    }

    private fun accept(code: String) {
        session.redeemed(code)
        mutableState.update { it.copy(isReading = false, acceptedCode = code, failure = null) }
    }
}

/**
 * The safety-code comparison, and the only thing standing between this ceremony and a machine
 * in the middle.
 *
 * Confirming needs an explicit acknowledgement that both the code *and* the named device
 * match, and there is no default: [confirm] refuses and says so rather than proceeding, which
 * is the difference between a check and a dialog people click through. A mismatch is its own
 * exit, reached either by the user saying so or by the server refusing the code.
 */
class SafetyCodeViewModel(
    private val gateway: EnrollmentGateway,
    private val session: EnrollmentSession,
) : ViewModel() {
    private val mutableState = MutableStateFlow(SafetyCodeUiState())

    /** Everything the comparison screen draws. */
    val state: StateFlow<SafetyCodeUiState> = mutableState.asStateFlow()

    init {
        val code = SafetyCode(SafetyCode.parse(gateway.safetyCode()).toImmutableList())
        // The fingerprints are derived separately from the safety code rather than sliced out
        // of it. The doc binds the code to each device's *identity* precisely so that a relay
        // which reproduced the code against different hardware is still caught; a fingerprint
        // that was a substring of the code would be satisfied by the same forgery.
        mutableState.update {
            it.copy(
                safetyCode = code,
                thisDevice = thisDeviceIdentity(fingerprintOf(code.text.hashCode().toLong())),
                otherDevice =
                    DeviceIdentity(
                        model = PEER_MODEL,
                        keyFingerprint = fingerprintOf(PEER_MODEL.hashCode().toLong()),
                    ),
            )
        }
    }

    /** Ticks or unticks the match-and-identity acknowledgement. */
    fun acknowledgementChanged(acknowledged: Boolean) {
        mutableState.update { it.copy(isAcknowledged = acknowledged, acknowledgementMissing = false) }
    }

    /** Completes the ceremony, but only once the user has actually said the codes match. */
    fun confirm() {
        val current = mutableState.value
        if (!current.canConfirm) return
        if (!current.isAcknowledged) {
            mutableState.update { it.copy(acknowledgementMissing = true) }
            return
        }
        val code = current.safetyCode ?: return
        mutableState.update { it.copy(isConfirming = true, failure = null) }
        viewModelScope.launch {
            runCatching {
                gateway.enroll(
                    displayName = PEER_MODEL,
                    platform = PlatformTag.Android,
                    confirmedSafetyCode = code.text,
                )
            }.onSuccess { device ->
                val outcome = EnrollmentOutcome.Added(device.displayName ?: PEER_MODEL)
                session.finished(outcome)
                mutableState.update { it.copy(isConfirming = false, outcome = outcome) }
            }.onFailure { throwable -> settleFailure(throwable) }
        }
    }

    /** The abort path. The user says the codes differ, and nothing is handed over. */
    fun abort() {
        mutableState.update { it.copy(isAborted = true, isConfirming = false) }
    }

    /** Burns this ceremony and returns to the beginning. */
    fun startOver() {
        session.restart()
        mutableState.update { it.copy(isAborted = false, isAcknowledged = false, failure = null) }
    }

    /**
     * Turns a refusal into either the deferred outcome or the abort screen.
     *
     * An unreachable server is not a mismatch: the keys are valid and the directory upload
     * simply has not landed, so it completes with [EnrollmentOutcome.PendingDirectoryUpload].
     * Anything else the server says at this point means the channel was not what it claimed,
     * and that ends the ceremony rather than offering a retry.
     */
    private fun settleFailure(throwable: Throwable) {
        if (throwable is LocalError.Offline) {
            session.finished(EnrollmentOutcome.PendingDirectoryUpload)
            mutableState.update {
                it.copy(isConfirming = false, outcome = EnrollmentOutcome.PendingDirectoryUpload)
            }
            return
        }
        mutableState.update {
            it.copy(isConfirming = false, isAborted = true, failure = EnrollmentFailure.of(throwable))
        }
    }

    private companion object {
        /**
         * What the fixture's peer calls itself.
         *
         * A constant rather than a catalogue key: a device model is a proper noun the hardware
         * reports, and translating one would be inventing a device that does not exist.
         */
        const val PEER_MODEL = "Pixel 9 Pro"
    }
}

/** The last screen, which only reads what the ceremony already decided. */
class EnrollmentCompleteViewModel(
    session: EnrollmentSession,
) : ViewModel() {
    /** How the ceremony ended. */
    val state: StateFlow<EnrollmentCompleteUiState> =
        session.progress
            .map { EnrollmentCompleteUiState(outcome = it.outcome) }
            .stateIn(
                scope = viewModelScope,
                started = SharingStarted.Eagerly,
                initialValue = EnrollmentCompleteUiState(outcome = session.progress.value.outcome),
            )
}
