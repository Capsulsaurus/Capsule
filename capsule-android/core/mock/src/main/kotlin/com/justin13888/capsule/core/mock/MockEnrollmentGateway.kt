package com.justin13888.capsule.core.mock

import com.justin13888.capsule.core.data.EnrollmentError
import com.justin13888.capsule.core.data.ProtocolError
import com.justin13888.capsule.core.mock.seed.DeterministicIds
import com.justin13888.capsule.core.mock.seed.Prng
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.DeviceId
import com.justin13888.capsule.core.model.PlatformTag
import com.justin13888.capsule.core.model.asWire

/**
 * Adding a device to the account.
 *
 * Declared here rather than in `core:data` because no repository contract owns enrollment yet
 * — it is the one flow in the app whose surface has not been pinned. Keeping it in the mock
 * makes that explicit: a feature written against this interface is written against a *mock*
 * type and will not compile once the real contract lands, which is exactly the reminder that
 * should exist.
 *
 * The safety code is compared out of band by the two humans holding the devices. That
 * comparison is the only thing standing between an enrollment and a machine in the middle,
 * which is why a mismatch is a hard refusal here and not a warning a user can dismiss past.
 */
interface EnrollmentGateway {
    /** The code shown on this device, for the user to read aloud to the other one. */
    fun safetyCode(): String

    /**
     * Completes an enrollment against the code the other device displayed.
     *
     * Refuses with [EnrollmentError.CodeRefused] when the codes differ, and with
     * [ProtocolError.VersionUnsupported] when the two ends do not speak the same protocol
     * version — a `426`, which no amount of retrying will fix.
     */
    suspend fun enroll(
        displayName: String,
        platform: PlatformTag,
        confirmedSafetyCode: String,
    ): Device
}

/** The scenario-driven implementation. */
class MockEnrollmentGateway(
    private val backend: MockBackend,
) : EnrollmentGateway {
    private val prng = Prng(Prng.mixSeed(backend.library.spec.seed, "enrollment"))

    override fun safetyCode(): String =
        DeterministicIds
            .hexToken(SAFETY_CODE_BYTES, Prng(Prng.mixSeed(backend.library.spec.seed, "safety-code")))
            .chunked(GROUP_SIZE)
            .joinToString("-")

    override suspend fun enroll(
        displayName: String,
        platform: PlatformTag,
        confirmedSafetyCode: String,
    ): Device {
        backend.gate(MockOperation.Enrollment)
        when (backend.scenario.value.enrollment) {
            EnrollmentScenario.ProtocolMismatch -> throw ProtocolError.VersionUnsupported
            EnrollmentScenario.SafetyCodeMismatch -> throw EnrollmentError.CodeRefused
            EnrollmentScenario.Succeeds -> Unit
        }
        if (confirmedSafetyCode != safetyCode()) throw EnrollmentError.CodeRefused
        // A UUIDv4, never a v7: a device id must not leak when the device was enrolled.
        val device =
            Device(
                id = DeviceId(DeterministicIds.uuidV4(prng)),
                displayName = displayName,
                platform = platform.asWire(),
                addedAt = backend.now(),
            )
        backend.mutate { it.devices[device.id] = device }
        return device
    }

    private companion object {
        const val SAFETY_CODE_BYTES = 6
        const val GROUP_SIZE = 4
    }
}
