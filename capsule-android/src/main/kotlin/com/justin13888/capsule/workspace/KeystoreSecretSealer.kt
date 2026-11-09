package com.justin13888.capsule.workspace

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import java.nio.ByteBuffer
import java.security.KeyStore
import java.util.logging.Logger
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * [SecretSealer] over an AES-256-GCM key in the Android Keystore under [keyAlias]. The key
 * is generated in StrongBox where the device has a secure element (API 28+) and in the TEE
 * where it does not, and is never exportable either way.
 *
 * A sealed value is `ivLength(1) ‖ iv ‖ ciphertext‖tag`, so the IV the keystore chose
 * travels with the ciphertext it opens.
 */
class KeystoreSecretSealer(
    private val keyAlias: String = DEFAULT_ALIAS,
) : SecretSealer {
    private val keyStore: KeyStore by lazy { KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) } }

    override fun seal(plaintext: ByteArray): ByteArray {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val iv = cipher.iv
        val ciphertext = cipher.doFinal(plaintext)
        return ByteBuffer
            .allocate(1 + iv.size + ciphertext.size)
            .put(iv.size.toByte())
            .put(iv)
            .put(ciphertext)
            .array()
    }

    override fun open(sealed: ByteArray): ByteArray {
        val key =
            keyStore.getKey(keyAlias, null) as? SecretKey
                ?: throw UnlockSecretUnavailableException("no keystore key under $keyAlias")
        val ivLength = sealed.firstOrNull()?.toInt() ?: throw UnlockSecretUnavailableException("the sealed secret is empty")
        if (ivLength <= 0 || sealed.size <= 1 + ivLength) throw UnlockSecretUnavailableException("the sealed secret is truncated")
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BITS, sealed, 1, ivLength))
        return cipher.doFinal(sealed, 1 + ivLength, sealed.size - 1 - ivLength)
    }

    private fun key(): SecretKey =
        (keyStore.getKey(keyAlias, null) as? SecretKey)
            ?: if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                try {
                    generate(strongBox = true)
                } catch (_: StrongBoxUnavailableException) {
                    log.info("unlock secret: no StrongBox on this device; the key is kept in the TEE")
                    generate(strongBox = false)
                }
            } else {
                generate(strongBox = false)
            }

    private fun generate(strongBox: Boolean): SecretKey {
        val spec =
            KeyGenParameterSpec
                .Builder(keyAlias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(KEY_BITS)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .apply { if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) setIsStrongBoxBacked(true) }
                .build()
        return KeyGenerator
            .getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
            .apply { init(spec) }
            .generateKey()
            .also { log.info("unlock secret: keystore key generated (strongBox=$strongBox)") }
    }

    private companion object {
        const val ANDROID_KEYSTORE = "AndroidKeyStore"
        const val DEFAULT_ALIAS = "capsule.library.unlock"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val KEY_BITS = 256
        const val TAG_BITS = 128

        val log: Logger = Logger.getLogger("capsule.workspace")
    }
}
