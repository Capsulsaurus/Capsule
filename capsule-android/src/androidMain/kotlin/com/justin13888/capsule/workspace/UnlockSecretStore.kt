package com.justin13888.capsule.workspace

import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.nio.channels.FileChannel
import java.nio.file.StandardOpenOption
import java.security.SecureRandom
import java.util.logging.Logger

/**
 * Seals and opens the library's unlock secret. On a device it is an Android Keystore key
 * that never leaves the secure hardware ([KeystoreSecretSealer]); tests pass their own.
 */
interface SecretSealer {
    /** [plaintext] sealed so only this sealer opens it again. */
    fun seal(plaintext: ByteArray): ByteArray

    /** The plaintext [sealed] holds; throws when it was not sealed by this sealer's key. */
    fun open(sealed: ByteArray): ByteArray
}

/** The sealed unlock secret exists but this device's key cannot open it. */
class UnlockSecretUnavailableException(
    message: String,
    cause: Throwable? = null,
) : Exception(message, cause)

/**
 * The passphrase the library at rest is wrapped under, kept sealed in [file].
 *
 * The person never types it: it is [SECRET_BYTES] random bytes drawn on first launch,
 * sealed by [sealer] and written before any library is created under it, so a library
 * never exists without the secret that opens it. The recovery phrase that rebuilds an
 * account on another device is a separate secret, made by the enrollment ceremony.
 *
 * A secret the sealer cannot open is reported, never replaced: replacing it would leave
 * the library it wraps unopenable for good.
 */
class UnlockSecretStore(
    private val file: File,
    private val sealer: SecretSealer,
    private val random: (Int) -> ByteArray = ::secureRandomBytes,
) {
    /** Whether a sealed secret has been written. */
    fun exists(): Boolean = file.exists()

    /** The stored secret, or a new one sealed and written first when none exists. */
    fun loadOrCreate(): ByteArray = if (file.exists()) load() else create()

    /** The stored secret. Throws [UnlockSecretUnavailableException] when it does not open. */
    fun load(): ByteArray {
        val sealed =
            try {
                file.readBytes()
            } catch (error: IOException) {
                throw UnlockSecretUnavailableException("the sealed unlock secret is unreadable", error)
            }
        return try {
            sealer.open(sealed)
        } catch (error: UnlockSecretUnavailableException) {
            throw error
        } catch (
            @Suppress("TooGenericExceptionCaught") error: Exception,
        ) {
            // A keystore refusal surfaces as whichever provider exception it raised; all of
            // them mean the same thing here: this device's key does not open this secret.
            log.warning("unlock secret: the sealed secret did not open (${error.javaClass.simpleName})")
            throw UnlockSecretUnavailableException("the sealed unlock secret did not open", error)
        }
    }

    private fun create(): ByteArray {
        val secret = random(SECRET_BYTES)
        val sealed = sealer.seal(secret)
        file.parentFile?.mkdirs()
        // Written beside, synced, and renamed over, then the rename synced: the library is
        // created under this secret next, so it has to be durable before the library is, or a
        // power loss could keep the library and lose the only secret that opens it.
        val partial = File(file.path + ".partial")
        FileOutputStream(partial).use { out ->
            out.write(sealed)
            out.fd.sync()
        }
        if (!partial.renameTo(file)) {
            partial.delete()
            throw IOException("could not store the unlock secret at ${file.path}")
        }
        file.parentFile?.let(::syncDirectory)
        log.info("unlock secret: a new secret was sealed and stored")
        return secret
    }

    companion object {
        /** 256 bits: the secret is a key, not something a person remembers. */
        const val SECRET_BYTES = 32

        private val log: Logger = Logger.getLogger("capsule.workspace")

        private fun secureRandomBytes(count: Int): ByteArray = ByteArray(count).also { SecureRandom().nextBytes(it) }

        /** Makes a rename into [dir] durable: on Linux, and so Android, a directory opens for reading and syncs. */
        private fun syncDirectory(dir: File) {
            FileChannel.open(dir.toPath(), StandardOpenOption.READ).use { it.force(true) }
        }
    }
}
