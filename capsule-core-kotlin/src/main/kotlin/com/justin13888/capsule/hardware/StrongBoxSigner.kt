package com.justin13888.capsule.hardware

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import uniffi.capsule_core.HardwareSigner
import uniffi.capsule_core.HardwareSignerException
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.Signature
import java.security.interfaces.ECPrivateKey
import java.security.interfaces.ECPublicKey

/**
 * Android StrongBox–backed [HardwareSigner]. The private key is generated inside the
 * AndroidKeyStore — StrongBox (a dedicated secure element) when [strongBoxBacked] is true, else
 * the TEE — and never leaves it.
 *
 * ## Algorithm (the same one Secure Enclave and the TPM reference produce)
 *
 * The AndroidKeyStore exposes **ECDSA over NIST P-256**, not Ed25519, so this returns P-256
 * material: a 65-byte **uncompressed SEC1** (`0x04‖x‖y`) public key — the same encoding Secure
 * Enclave's x9.63 representation uses, which `P256HybridSigningKey` normalizes on the Rust side —
 * and an ASN.1/DER ECDSA signature over `msg` that `p256::ecdsa` verifies verbatim. It therefore
 * plugs into the P-256 `createWithP256HardwareSigner` path end to end (slice S-F2); the Ed25519
 * [SoftwareSigner] drives the separate `createWithHardwareSigner` path.
 *
 * Requires API 23+ (StrongBox: API 28+ on devices that ship a secure element).
 */
class StrongBoxSigner(
    private val strongBoxBacked: Boolean = true,
) : HardwareSigner {
    private val keyStore: KeyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }

    private fun privateKey(keyAlias: String): ECPrivateKey {
        (keyStore.getKey(keyAlias, null) as? ECPrivateKey)?.let { return it }
        val generator = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, ANDROID_KEYSTORE)
        val spec =
            KeyGenParameterSpec
                .Builder(keyAlias, KeyProperties.PURPOSE_SIGN)
                .setAlgorithmParameterSpec(java.security.spec.ECGenParameterSpec("secp256r1"))
                .setDigests(KeyProperties.DIGEST_SHA256)
                .setIsStrongBoxBacked(strongBoxBacked)
                .build()
        generator.initialize(spec)
        return generator.generateKeyPair().private as ECPrivateKey
    }

    override fun enroll(keyAlias: String): ByteArray {
        privateKey(keyAlias)
        return classicalPublicKey(keyAlias)
    }

    override fun classicalPublicKey(keyAlias: String): ByteArray {
        val pub = keyStore.getCertificate(keyAlias)?.publicKey as? ECPublicKey
            ?: throw HardwareSignerException.NotFound("no StrongBox key for alias $keyAlias")
        // Emit uncompressed SEC1 (0x04‖x‖y, 65 bytes) — the shape `P256HybridSigningKey` ingests —
        // rather than the JCA default X.509 SubjectPublicKeyInfo, which the Rust side cannot parse.
        val w = pub.w
        val out = ByteArray(65)
        out[0] = 0x04
        fixedField(w.affineX, out, 1)
        fixedField(w.affineY, out, 33)
        return out
    }

    override fun signClassical(
        keyAlias: String,
        msg: ByteArray,
    ): ByteArray =
        Signature.getInstance("SHA256withECDSA").run {
            initSign(privateKey(keyAlias))
            update(msg)
            sign()
        }

    override fun assertNonExportable(keyAlias: String) {
        val key =
            keyStore.getKey(keyAlias, null)
                ?: throw HardwareSignerException.NotFound("no StrongBox key for alias $keyAlias")
        val factory = KeyFactory.getInstance(key.algorithm, ANDROID_KEYSTORE)
        val info = factory.getKeySpec(key, KeyInfo::class.java) as KeyInfo
        // The private bytes are unreadable by construction; confirm the key is in secure hardware.
        if (!info.isInsideSecureHardware) {
            throw HardwareSignerException.Exportable("key for $keyAlias is not in secure hardware")
        }
    }

    private companion object {
        const val ANDROID_KEYSTORE = "AndroidKeyStore"

        /// Write `value` as a fixed 32-byte big-endian field into `out` at `offset`, left-padding
        /// with zeros and dropping any BigInteger sign byte — the SEC1 coordinate encoding.
        private fun fixedField(
            value: java.math.BigInteger,
            out: ByteArray,
            offset: Int,
        ) {
            val bytes = value.toByteArray() // big-endian, possibly with a leading sign byte
            val src = if (bytes.size > 32) bytes.copyOfRange(bytes.size - 32, bytes.size) else bytes
            System.arraycopy(src, 0, out, offset + (32 - src.size), src.size)
        }
    }
}
