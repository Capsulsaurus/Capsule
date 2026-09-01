package com.justin13888.capsule.hardware

import org.bouncycastle.asn1.x9.ECNamedCurveTable
import org.bouncycastle.crypto.digests.SHA256Digest
import org.bouncycastle.crypto.generators.HKDFBytesGenerator
import org.bouncycastle.crypto.params.ECDomainParameters
import org.bouncycastle.crypto.params.ECPrivateKeyParameters
import org.bouncycastle.crypto.params.HKDFParameters
import org.bouncycastle.crypto.signers.ECDSASigner
import org.bouncycastle.crypto.signers.StandardDSAEncoding
import uniffi.capsule_core.HardwareSigner
import uniffi.capsule_core.HardwareSignerException
import java.math.BigInteger
import java.security.MessageDigest

/**
 * Software **P-256** [HardwareSigner] — the no-secure-element reference the real StrongBox
 * replaces, and the always-runnable path for the P-256 hybrid composition (a JVM/CI host has no
 * StrongBox). It mirrors [StrongBoxSigner]'s wire contract exactly — a 65-byte **uncompressed
 * SEC1** (`0x04‖x‖y`) public key and a **DER-encoded ECDSA** signature over `SHA-256(msg)` — so it
 * plugs into `FfiWorkspace.createWithP256HardwareSigner` end to end (slice S-F2), and mirrors the
 * Swift `SoftwareP256Signer`.
 *
 * The P-256 scalar is derived deterministically (HKDF-SHA256, per `keyAlias`, reduced mod n) from a
 * 32-byte seed the caller seals. It offers no hardware non-exportability, which [assertNonExportable]
 * reports truthfully by throwing [HardwareSignerException.Exportable]. BouncyCastle is used so the
 * derivation and DER encoding are identical on the JVM (unit tests) and on Android.
 */
class SoftwareP256Signer(
    private val seed: ByteArray,
) : HardwareSigner {
    init {
        require(seed.size == 32) { "software signer seed must be 32 bytes" }
    }

    private fun privateKey(keyAlias: String): ECPrivateKeyParameters {
        val out = ByteArray(32)
        HKDFBytesGenerator(SHA256Digest()).apply {
            init(HKDFParameters(seed, keyAlias.toByteArray(Charsets.UTF_8), INFO))
            generateBytes(out, 0, out.size)
        }
        // Reduce into [1, n-1]; the vanishing-probability zero case maps to 1.
        val d = (BigInteger(1, out).mod(DOMAIN.n - BigInteger.ONE)) + BigInteger.ONE
        return ECPrivateKeyParameters(d, DOMAIN)
    }

    override fun enroll(keyAlias: String): ByteArray = classicalPublicKey(keyAlias)

    override fun classicalPublicKey(keyAlias: String): ByteArray {
        val q = DOMAIN.g.multiply(privateKey(keyAlias).d).normalize()
        return q.getEncoded(false) // uncompressed SEC1: 0x04‖x‖y (65 bytes)
    }

    override fun signClassical(
        keyAlias: String,
        msg: ByteArray,
    ): ByteArray {
        val hash = MessageDigest.getInstance("SHA-256").digest(msg)
        val (r, s) =
            ECDSASigner().run {
                init(true, privateKey(keyAlias))
                generateSignature(hash)
            }
        return StandardDSAEncoding.INSTANCE.encode(DOMAIN.n, r, s)
    }

    override fun assertNonExportable(keyAlias: String) {
        // Honest by design: a software key is readable, so it can never meet the hardware
        // non-exportability contract a Secure Enclave / StrongBox / TPM does.
        throw HardwareSignerException.Exportable("software P-256 key is exportable")
    }

    private companion object {
        val INFO = "capsule/software-signer/p256/v1".toByteArray(Charsets.UTF_8)
        val DOMAIN: ECDomainParameters =
            ECNamedCurveTable.getByName("secp256r1").let {
                ECDomainParameters(it.curve, it.g, it.n, it.h)
            }
    }
}
