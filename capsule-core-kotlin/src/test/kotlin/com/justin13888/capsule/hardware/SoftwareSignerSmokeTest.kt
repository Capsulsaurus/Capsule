package com.justin13888.capsule.hardware

import org.bouncycastle.crypto.params.Ed25519PublicKeyParameters
import org.bouncycastle.crypto.signers.Ed25519Signer
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import uniffi.capsule_core.DeviceTier
import uniffi.capsule_core.FfiWorkspace
import uniffi.capsule_core.HardwareSignerException
import java.nio.file.Files

/**
 * Proves the compiled `capsule-core` works when consumed from Kotlin over uniffi — the Android
 * analogue of the Rust Linux software smoke. A JVM unit test (no device): JNA loads the host
 * `libcapsule_core` from `jna.library.path` (set in build.gradle.kts). Run `./stage-bindings.sh`
 * first. The StrongBox path is on-device only (see androidInstrumentedTest).
 */
class SoftwareSignerSmokeTest {
    private fun freshRoot(): String = Files.createTempDirectory("capsule-kotlin-smoke").toString()

    @Test
    fun softwareSignerContractHoldsAndIsHonest() {
        val signer = SoftwareSigner(ByteArray(32) { 7 })
        val pub = signer.enroll("device-dsk")
        assertEquals(32, pub.size, "Ed25519 public key is 32 bytes")

        val msg = "asset manifest bytes".toByteArray()
        val sig = signer.signClassical("device-dsk", msg)
        assertEquals(64, sig.size, "Ed25519 signature is 64 bytes")

        val verifier =
            Ed25519Signer().apply {
                init(false, Ed25519PublicKeyParameters(pub, 0))
                update(msg, 0, msg.size)
            }
        assertTrue(verifier.verifySignature(sig), "signature verifies against the published key")

        // Honest: a software key reports itself exportable, unlike a real secure element.
        assertThrows(HardwareSignerException.Exportable::class.java) {
            signer.assertNonExportable("device-dsk")
        }
    }

    @Test
    fun softwarePathCreatesWorkspace() {
        val ws = FfiWorkspace.create(freshRoot(), "correct horse".toByteArray(), DeviceTier.NORMAL)
        assertFalse(ws.userId().isEmpty())
        assertFalse(ws.defaultAlbumId().isEmpty())
    }

    @Test
    fun softwareHardwareSignerRoundTripsThroughFfi() {
        // The software signer produces genuine Ed25519, so it drives the full
        // createWithHardwareSigner foreign-trait path end to end.
        val signer = SoftwareSigner(ByteArray(32) { 7 })
        val ws =
            FfiWorkspace.createWithHardwareSigner(
                freshRoot(),
                "correct horse".toByteArray(),
                DeviceTier.NORMAL,
                signer,
                "device-dsk",
                ByteArray(32) { 9 },
            )
        assertFalse(ws.userId().isEmpty())
    }
}
