package com.justin13888.capsule.workspace

import com.justin13888.capsule.screens.domain.AssetId
import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiDeviceTier
import uniffi.capsule_sdk.FfiWorkspace
import uniffi.capsule_sdk.FfiWorkspaceInterface
import java.io.File
import java.io.IOException
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
 * The device signing key is the account's software key: `FfiWorkspace.open` takes no
 * hardware signer, so a key enrolled in StrongBox at creation would be replaced by the
 * software one at the next launch. The library's unlock secret is what the keystore holds.
 */
class LibraryLifecycle(
    private val root: File,
    private val secrets: UnlockSecretStore,
    private val tier: FfiDeviceTier,
    private val client: FfiClientBuild,
    /** The name the default album is created under, from the catalog. */
    private val defaultAlbumName: String,
) {
    /** Open the library, creating it first when this device has none. Blocks: run it off the main thread. */
    fun open(): OpenedLibrary {
        clearInterruptedCreate()
        val exists = File(root, LIBRARY_DIR).isDirectory
        val workspace =
            if (exists) {
                val passphrase = existingPassphrase()
                log.info("library: opening ${root.path}")
                FfiWorkspace.open(root.path, passphrase, tier, client)
            } else {
                // The secret is written before the library, so no library ever exists without one.
                val passphrase = secrets.loadOrCreate()
                // Only the parent: a root the core makes itself is one it removes again when
                // creation fails, so a failed first launch leaves nothing half-made to open.
                root.parentFile?.mkdirs()
                log.info("library: creating ${root.path}")
                FfiWorkspace.create(root.path, passphrase, tier, client)
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

        /** What the core writes under [LIBRARY_DIR] while creating, before any account exists. */
        private val INIT_FILES = listOf("version.cbor", "config.cbor")
        private const val ACCOUNT_FILE = "account.cbor"
        private const val MEDIA_DIR = "media"

        private val log: Logger = Logger.getLogger("capsule.workspace")
    }
}
