package com.justin13888.capsule.hardware

import org.bouncycastle.crypto.digests.SHA512Digest
import org.bouncycastle.crypto.generators.HKDFBytesGenerator
import org.bouncycastle.crypto.params.Ed25519PrivateKeyParameters
import org.bouncycastle.crypto.params.HKDFParameters
import org.bouncycastle.crypto.signers.Ed25519Signer
import uniffi.capsule_core.HardwareSigner
import uniffi.capsule_core.HardwareSignerException

/**
 * Software [HardwareSigner] — the no-secure-element fallback, mirroring the Rust and Swift
 * `SoftwareSigner`. The classical Ed25519 key lives in process memory, derived deterministically
 * (HKDF-SHA512, per `keyAlias`) from a 32-byte seed the caller is responsible for sealing.
 *
 * Because it produces a genuine 32-byte Ed25519 public key and 64-byte signature, this backend
 * plugs **directly** into `FfiWorkspace.createWithHardwareSigner` — it is the one the JVM smoke
 * test drives end to end. It offers no hardware non-exportability, which [assertNonExportable]
 * reports truthfully by throwing [HardwareSignerException.Exportable].
 *
 * The derivation matches the Rust/Swift references (salt = `keyAlias`, info =
 * `capsule/software-signer/ed25519/v1`), so the same seed yields the same device key in every
 * language. BouncyCastle is used so deterministic-from-seed Ed25519 works identically on the JVM
 * (unit tests) and on Android without relying on a particular JCA provider's Ed25519 support.
 */
class SoftwareSigner(private val seed: ByteArray) : HardwareSigner {
    init {
        require(seed.size == 32) { "software signer seed must be 32 bytes" }
    }

    private fun privateKey(keyAlias: String): Ed25519PrivateKeyParameters {
        val out = ByteArray(32)
        HKDFBytesGenerator(SHA512Digest()).apply {
            init(HKDFParameters(seed, keyAlias.toByteArray(Charsets.UTF_8), INFO))
            generateBytes(out, 0, out.size)
        }
        return Ed25519PrivateKeyParameters(out, 0)
    }

    override fun enroll(keyAlias: String): ByteArray = classicalPublicKey(keyAlias)

    override fun classicalPublicKey(keyAlias: String): ByteArray =
        privateKey(keyAlias).generatePublicKey().encoded

    override fun signClassical(keyAlias: String, msg: ByteArray): ByteArray =
        Ed25519Signer().run {
            init(true, privateKey(keyAlias))
            update(msg, 0, msg.size)
            generateSignature()
        }

    override fun assertNonExportable(keyAlias: String) {
        // Honest by design: a software key is readable, so it can never meet the hardware
        // non-exportability contract a Secure Enclave / StrongBox / TPM does.
        throw HardwareSignerException.Exportable("software key is exportable")
    }

    private companion object {
        val INFO = "capsule/software-signer/ed25519/v1".toByteArray(Charsets.UTF_8)
    }
}
