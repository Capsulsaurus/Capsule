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

/**
 * Android StrongBox–backed [HardwareSigner]. The private key is generated inside the
 * AndroidKeyStore — StrongBox (a dedicated secure element) when [strongBoxBacked] is true, else
 * the TEE — and never leaves it.
 *
 * ## Algorithm caveat (the same one Secure Enclave and the TPM have)
 *
 * The AndroidKeyStore exposes **ECDSA over NIST P-256**, not Ed25519, so this returns P-256
 * material: an X.509 `SubjectPublicKeyInfo` public key and an ASN.1/DER ECDSA signature over
 * `msg`. It therefore does **not** yet plug into the Ed25519 `createWithHardwareSigner` path —
 * wiring a StrongBox device in needs the P-256 hybrid-DSK variant tracked in `DEFERRED.md`. It is
 * the real hardware adapter + the on-device non-exportability check; for an end-to-end FFI round
 * trip use [SoftwareSigner] (genuine Ed25519).
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

    override fun classicalPublicKey(keyAlias: String): ByteArray =
        keyStore.getCertificate(keyAlias)?.publicKey?.encoded
            ?: throw HardwareSignerException.NotFound("no StrongBox key for alias $keyAlias")

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
    }
}
