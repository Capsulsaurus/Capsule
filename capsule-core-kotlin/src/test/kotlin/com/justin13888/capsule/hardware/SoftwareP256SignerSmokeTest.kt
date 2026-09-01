package com.justin13888.capsule.hardware

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import uniffi.capsule_core.DeviceTier
import uniffi.capsule_core.FfiClientBuild
import uniffi.capsule_core.FfiVerifyOutcome
import uniffi.capsule_core.FfiWorkspace
import uniffi.capsule_core.HardwareSignerException
import java.nio.file.Files

/**
 * The Kotlin analogue of the Swift `SmokeTests.softwareP256ComposesEndToEnd` — proves the software
 * **P-256** element composes through `P256HybridSigningKey` into a real workspace over uniffi
 * (slice S-F2). The [StrongBoxSigner] path is on-device only (androidInstrumentedTest); this
 * always-runnable JVM path exercises the same wire contract (65-byte uncompressed SEC1 public key,
 * DER ECDSA signature) that StrongBox emits, so a green run proves the whole P-256 FFI composition
 * independent of hardware. Run `./stage-bindings.sh` first.
 *
 * Platform-CI-owed: the root Gradle/Android toolchain does not run on the reference dev host, so
 * this is written to mirror the verified Swift shape and is exercised by the platform CI lane.
 */
class SoftwareP256SignerSmokeTest {
    private val client = FfiClientBuild("capsule-core-kotlin", "0.0.0")

    private fun freshRoot(): String = Files.createTempDirectory("capsule-kotlin-p256-smoke").toString()

    private fun freshImage(): String {
        val img = Files.createTempFile("capsule-kotlin-p256-img", ".jpg")
        Files.write(img, byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte()) + "p256 smoke bytes".toByteArray())
        return img.toString()
    }

    @Test
    fun softwareP256ContractHoldsAndIsHonest() {
        val signer = SoftwareP256Signer(ByteArray(32) { 7 })

        val pub = signer.enroll("device-dsk")
        assertEquals(65, pub.size, "P-256 uncompressed SEC1 public key is 65 bytes")
        assertEquals(0x04.toByte(), pub[0], "uncompressed-point tag")

        val sig = signer.signClassical("device-dsk", "asset manifest bytes".toByteArray())
        assertEquals(0x30.toByte(), sig[0], "DER SEQUENCE tag — not raw r‖s")

        // Honest: a software key reports itself exportable, unlike a real secure element.
        assertThrows(HardwareSignerException.Exportable::class.java) {
            signer.assertNonExportable("device-dsk")
        }
    }

    @Test
    fun softwareP256ComposesThroughFfi() {
        val signer = SoftwareP256Signer(ByteArray(32) { 7 })
        val ws =
            FfiWorkspace.createWithP256HardwareSigner(
                freshRoot(),
                "correct horse".toByteArray(),
                DeviceTier.NORMAL,
                signer,
                "device-dsk",
                ByteArray(32) { 9 },
                client,
            )
        // The manifest, signed by the P-256 hybrid, verifies through the verify_asset chokepoint.
        // Import into a session album (the default album has no session key material minted).
        val asset = ws.importAsset(ws.createAlbum("P-256 Trip"), freshImage())
        assertTrue(ws.verify(asset) is FfiVerifyOutcome.Accept)
        assertFalse(ws.userId().isEmpty())
    }
}
