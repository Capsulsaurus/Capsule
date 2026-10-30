package com.justin13888.capsule.workspace

import com.justin13888.capsule.screens.domain.AssetId
import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiDeviceTier
import uniffi.capsule_sdk.FfiHardwareSigner
import uniffi.capsule_sdk.FfiWorkspace
import uniffi.capsule_sdk.FfiWorkspaceInterface
import java.io.File
import java.io.IOException
import java.security.SecureRandom
import java.util.logging.Logger

/** The library this device holds, opened, with the subjects the home menu opens editors on. */
class OpenedLibrary(
    val workspace: FfiWorkspaceInterface,
    /** The library's first asset, or `null` while it holds none. */
    val sampleAsset: AssetId?,
)

/** The library on disk exists, but the secret that unlocks it is gone or does not open. */
class LibraryLockedException(
    cause: Throwable?,
) : Exception("the library exists but its unlock secret is unavailable", cause)

/**
 * The first launch could not create the library because the keystore refused to enroll the
 * device signing key. Nothing was laid out, so the launch can be retried, or the library
 * created on software keys instead (`app.enrollment.failure.hardware.*`).
 */
class HardwareEnrollmentRefusedException(
    cause: Throwable,
) : Exception("the keystore refused to enroll the device signing key", cause)

/**
 * Passes every call to [element], keeping what [enroll] threw: the core reports an element's
 * own failure as an internal error, so this is the only place a refused enrollment can still
 * be told apart from any other reason creation failed.
 */
private class EnrollmentWatch(
    private val element: FfiHardwareSigner,
) : FfiHardwareSigner by element {
    @Volatile
    var refusal: Throwable? = null
        private set

    override fun enroll(keyAlias: String): ByteArray =
        try {
            element.enroll(keyAlias)
        } catch (
            @Suppress("TooGenericExceptionCaught") error: Exception,
        ) {
            refusal = error
            throw error
        }
}

/**
 * Creates the library on first launch and opens it on every launch after.
 *
 * The library lives at [root] and is wrapped under the secret [secrets] holds. Which of
 * `create` and `open` runs is decided by what is on disk, never by a flag that could
 * disagree with it: a root with no `.library` is new and is created; one with a `.library`
 * is opened, which also finishes a library whose first launch stopped after the library was
 * laid out and before its account was written. A first launch that stopped earlier, while
 * the core was still laying it out, left a skeleton with no account and no media: that is
 * cleared and created again, since neither open nor create can get past it.
 *
 * A launch never closes its library (the system kills the process instead), so the lock it
 * holds outlives it. The core treats a lock whose holding process is gone as stale.
 *
 * An existing library whose secret is missing or does not open is refused with
 * [LibraryLockedException] and left as it is. Nothing here ever deletes or recreates a
 * library that holds an account or any media, because that would discard the assets in it.
 *
 * The device signing key is held by [signer] (StrongBox on a device, a software stand-in in
 * tests) under [DEVICE_SIGNING_KEY_ALIAS]: a new library is created on it with
 * `createWithP256HardwareSigner`, and every launch after reopens it with
 * `openWithP256HardwareSigner`, so the key the device publishes is the same at every launch.
 * The library records which key it signs with, so a library created on the software key (any
 * library from before the key moved into the keystore) keeps opening on that key. When the
 * keystore refuses the key the launch fails with [HardwareEnrollmentRefusedException] and can be
 * retried; nothing here falls back to software keys on its own. A `null` [signer] is the documented deviation of software keys
 * (`app.enrollment.failure.hardware.*`), and only a caller that offered that choice passes it.
 */
class LibraryLifecycle(
    private val root: File,
    private val secrets: UnlockSecretStore,
    private val tier: FfiDeviceTier,
    private val client: FfiClientBuild,
    /** The name the default album is created under, from the catalog. */
    private val defaultAlbumName: String,
    /** The element holding the device signing key, or `null` for software keys. */
    private val signer: FfiHardwareSigner?,
    /** Draws the 32-byte seed of the signing key's post-quantum half, sealed by the library at creation. */
    private val signingSeed: () -> ByteArray = { ByteArray(SIGNING_SEED_BYTES).also { SecureRandom().nextBytes(it) } },
) {
    /** Open the library, creating it first when this device has none. Blocks: run it off the main thread. */
    fun open(): OpenedLibrary {
        clearInterruptedCreate()
        val exists = File(root, LIBRARY_DIR).isDirectory
        val workspace =
            if (exists) {
                val passphrase = existingPassphrase()
                log.info("library: opening ${root.path} (signingElement=${signer != null})")
                if (signer != null) {
                    FfiWorkspace.openWithP256HardwareSigner(root.path, passphrase, tier, signer, DEVICE_SIGNING_KEY_ALIAS, client)
                } else {
                    FfiWorkspace.open(root.path, passphrase, tier, client)
                }
            } else {
                // The secret is written before the library, so no library ever exists without one.
                val passphrase = secrets.loadOrCreate()
                // Only the parent: a root the core makes itself is one it removes again when
                // creation fails, so a failed first launch leaves nothing half-made to open.
                root.parentFile?.mkdirs()
                log.info("library: creating ${root.path} (signingElement=${signer != null})")
                if (signer != null) {
                    createOnElement(signer, passphrase)
                } else {
                    FfiWorkspace.create(root.path, passphrase, tier, client)
                }
            }
        // Idempotent: the default album is there from the first launch, so the album editor
        // and the first import always have somewhere to go.
        val sampleAsset =
            try {
                workspace.ensureAlbum(workspace.defaultAlbumId(), defaultAlbumName)
                workspace.assetIds().firstOrNull()?.let(::AssetId)
            } catch (
                @Suppress("TooGenericExceptionCaught") error: Exception,
            ) {
                // Released rather than leaked: the caller only ever sees the failure.
                (workspace as? AutoCloseable)?.close()
                throw error
            }
        log.info("library: open (assets=${if (sampleAsset == null) 0 else "1+"})")
        return OpenedLibrary(workspace, sampleAsset)
    }

    /**
     * Creates the library on [element]'s key. A refused enrollment fails before the core writes
     * anything under the root, and is reported as [HardwareEnrollmentRefusedException] so the
     * person can be offered software keys; any other failure is passed on as it came.
     */
    private fun createOnElement(
        element: FfiHardwareSigner,
        passphrase: ByteArray,
    ): FfiWorkspace {
        val watch = EnrollmentWatch(element)
        return try {
            FfiWorkspace.createWithP256HardwareSigner(
                root.path,
                passphrase,
                tier,
                watch,
                DEVICE_SIGNING_KEY_ALIAS,
                signingSeed(),
                client,
            )
        } catch (
            @Suppress("TooGenericExceptionCaught") error: Exception,
        ) {
            val refusal = watch.refusal ?: throw error
            log.warning("library: the keystore refused to enroll the device signing key (${refusal.javaClass.simpleName})")
            throw HardwareEnrollmentRefusedException(refusal).apply { addSuppressed(error) }
        }
    }

    /** The unlock secret of the library already on disk. Throws [LibraryLockedException] when it is missing or does not open. */
    private fun existingPassphrase(): ByteArray {
        if (!secrets.exists()) {
            log.severe("library: ${root.path} exists but no unlock secret does; refusing to touch it")
            throw LibraryLockedException(null)
        }
        return try {
            secrets.load()
        } catch (error: UnlockSecretUnavailableException) {
            throw LibraryLockedException(error)
        }
    }

    /**
     * Removes the skeleton a first launch left when it was killed while the core was still
     * laying the library out: `.library` exists, but its version or config file does not.
     * Open refuses such a root, and create refuses it for not being empty, so nothing else
     * would ever get past it. It is not a library yet: the account is written only after
     * both files, and an asset only into an opened library, so with no account and nothing
     * under `media` there is nothing in it to lose. Any other state is left for open to judge.
     */
    private fun clearInterruptedCreate() {
        val state = File(root, LIBRARY_DIR)
        if (!state.isDirectory) return
        val laidOut = INIT_FILES.all { File(state, it).isFile }
        val holdsAccount = File(state, ACCOUNT_FILE).exists()
        val holdsMedia = File(root, MEDIA_DIR).list()?.isNotEmpty() == true
        if (laidOut || holdsAccount || holdsMedia) return
        log.warning("library: ${root.path} is a first launch that stopped mid-creation; clearing it to create again")
        if (!root.deleteRecursively()) {
            throw IOException("could not clear the interrupted library at ${root.path}")
        }
    }

    companion object {
        /** The directory a library keeps its own state in, under its root. */
        const val LIBRARY_DIR = ".library"

        /** The keystore alias of the device signing key. The library records it; renaming it here never moves a library. */
        const val DEVICE_SIGNING_KEY_ALIAS = "capsule-device-dsk"

        private const val SIGNING_SEED_BYTES = 32

        /** What the core writes under [LIBRARY_DIR] while creating, before any account exists. */
        private val INIT_FILES = listOf("version.cbor", "config.cbor")
        private const val ACCOUNT_FILE = "account.cbor"
        private const val MEDIA_DIR = "media"

        private val log: Logger = Logger.getLogger("capsule.workspace")
    }
}
