package com.justin13888.capsule.feature.enrollment

import android.os.Build
import com.justin13888.capsule.core.data.LocalError
import com.justin13888.capsule.core.mock.MockBackend
import com.justin13888.capsule.core.mock.MockOperation
import com.justin13888.capsule.core.mock.MockRepositories
import kotlinx.coroutines.delay
import kotlin.random.Random

/**
 * The account ceremony, run against the seeded backend.
 *
 * The two steps that talk to a server go through [MockBackend.gate], which is what makes the
 * doc's deferred outcomes reachable by choosing a scenario rather than by editing code: the
 * offline scenario turns the directory upload into "will complete when reachable", and the
 * grace-expired quota scenario turns the default album into something a later import
 * recreates. The three key-generation steps are local and never touch the gate, because a
 * setup that needed the network to make a key would be a setup that failed on a plane.
 */
class MockFirstDeviceCeremony(
    private val repositories: MockRepositories,
    private val secureElement: SecureElement,
) : FirstDeviceCeremony {
    private val passphrase by lazy {
        RecoveryPassphrase.generate(Random(repositories.backend.library.spec.seed))
    }

    override suspend fun perform(step: EnrollmentStep) {
        when (step) {
            EnrollmentStep.MasterKey, EnrollmentStep.IdentityKey -> delay(LOCAL_WORK_MILLIS)
            EnrollmentStep.DeviceKeys -> secureElement.generateDeviceKeys()
            EnrollmentStep.DeviceDirectory -> repositories.backend.gate(MockOperation.Enrollment)
            EnrollmentStep.DefaultAlbum -> repositories.backend.gate(MockOperation.AlbumWrite)
        }
    }

    override fun recoveryPassphrase(): RecoveryPassphrase = passphrase

    private companion object {
        /** Long enough that the running state is visible, short enough not to be a wait. */
        const val LOCAL_WORK_MILLIS = 120L
    }
}

/**
 * The device's key hardware, with a documented way to make it refuse.
 *
 * Hardware refusal is rare and real, and the only failure in the ceremony with no server
 * behind it. The lever is the scenario's own escape hatch — a forced *local* failure on
 * [MockOperation.Enrollment] — so the state is reachable from a test or a debug drawer without
 * a preset having to exist for it, and without colliding with the server-side codes, which
 * that operation's gate throws on the later directory step instead.
 */
class MockSecureElement(
    private val backend: MockBackend,
) : SecureElement {
    override suspend fun generateDeviceKeys(): String {
        delay(GENERATION_MILLIS)
        val forced = backend.scenario.value.failures[MockOperation.Enrollment]
        if (forced is LocalError) throw forced
        return fingerprintOf(backend.library.spec.seed)
    }

    private companion object {
        const val GENERATION_MILLIS = 180L
    }
}

/** Issues the single-use code, refusing exactly where the server would. */
class MockEnrollmentCodeIssuer(
    private val backend: MockBackend,
) : EnrollmentCodeIssuer {
    override suspend fun issue(): EnrollmentCode {
        backend.gate(MockOperation.Enrollment)
        return codeFor(backend.library.spec.seed)
    }
}

/**
 * The mocked decode over a real camera preview.
 *
 * It returns the code the *other* end of this same fixture would have issued, which is what
 * makes the two halves of the ceremony agree without either of them needing a network. The
 * dwell is there because a scanner that succeeded on the first frame would never show the
 * looking state, which is most of what this screen is.
 */
class MockEnrollmentCodeReader(
    private val backend: MockBackend,
) : EnrollmentCodeReader {
    override suspend fun awaitCode(): String {
        delay(DWELL_MILLIS)
        return codeFor(backend.library.spec.seed).payload
    }

    private companion object {
        const val DWELL_MILLIS = 1_500L
    }
}

/**
 * The stand-in for the platform's fresh-authorization prompt.
 *
 * Always confirms, and is a binding rather than a hard-coded `true` precisely so that the app
 * can point it at the real biometric prompt without this module changing: the gate in front of
 * issuing a code is a security property of the flow, not a screen decoration.
 */
class AlwaysConfirmingAuthorization : EnrollmentAuthorization {
    override suspend fun confirm(): Boolean = true
}

/** What this device says it is, for the other end of the comparison to check. */
internal fun thisDeviceIdentity(fingerprint: String): DeviceIdentity =
    DeviceIdentity(model = "${Build.MANUFACTURER} ${Build.MODEL}".trim(), keyFingerprint = fingerprint)

/** A short, stable fingerprint over [seed], in the grouped form the screens compare. */
internal fun fingerprintOf(seed: Long): String =
    seed
        .toULong()
        .toString(radix = HEX)
        .padStart(FINGERPRINT_DIGITS, '0')
        .takeLast(FINGERPRINT_DIGITS)
        .uppercase()
        .chunked(FINGERPRINT_GROUP)
        .joinToString(SafetyCode.SEPARATOR)

/** The code the issuing end of this fixture produces for [seed]. */
internal fun codeFor(seed: Long): EnrollmentCode {
    val random = Random(seed)
    val payload = List(PAYLOAD_CHARS) { PAYLOAD_ALPHABET[random.nextInt(PAYLOAD_ALPHABET.length)] }.joinToString("")
    val digits = List(FRIENDLY_DIGITS) { random.nextInt(DECIMAL) }.joinToString("")
    return EnrollmentCode(payload = payload, friendlyDigits = digits)
}

private const val HEX = 16
private const val DECIMAL = 10
private const val FINGERPRINT_DIGITS = 8
private const val FINGERPRINT_GROUP = 4

/** Sixteen characters of a 32-symbol alphabet: eighty bits, comfortably over the doc's floor. */
private const val PAYLOAD_CHARS = 16

/** Deliberately shorter than [PAYLOAD_CHARS]: transcribable, and never load-bearing alone. */
private const val FRIENDLY_DIGITS = 9

/** Crockford-style: no I, L, O or U, so nothing is misread down a phone line. */
private const val PAYLOAD_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
