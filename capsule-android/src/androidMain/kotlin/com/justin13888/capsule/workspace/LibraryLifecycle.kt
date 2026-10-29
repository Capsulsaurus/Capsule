package com.justin13888.capsule.workspace

import com.justin13888.capsule.screens.domain.AssetId
import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiDeviceTier
import uniffi.capsule_sdk.FfiWorkspace
import uniffi.capsule_sdk.FfiWorkspaceInterface
import java.io.File
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
 * is opened, which also finishes a library whose first launch stopped after the directory
 * was made and before its account was written.
 *
 * An existing library whose secret is missing or does not open is refused with
 * [LibraryLockedException] and left as it is. Nothing here ever deletes or recreates a
 * library, because that would discard the assets in it.
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
        val exists = File(root, LIBRARY_DIR).isDirectory
        val workspace =
            if (exists) {
                val passphrase =
                    if (secrets.exists()) {
                        try {
                            secrets.load()
                        } catch (error: UnlockSecretUnavailableException) {
                            throw LibraryLockedException(error)
                        }
                    } else {
                        log.severe("library: ${root.path} exists but no unlock secret does; refusing to touch it")
                        throw LibraryLockedException(null)
                    }
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

    companion object {
        /** The directory a library keeps its own state in, under its root. */
        const val LIBRARY_DIR = ".library"

        private val log: Logger = Logger.getLogger("capsule.workspace")
    }
}
