package com.justin13888.capsule.workspace

import android.app.ActivityManager
import android.content.Context
import android.content.pm.PackageManager
import com.justin13888.capsule.R
import com.justin13888.capsule.hardware.StrongBoxSigner
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.capsule_sdk.FfiClientBuild
import uniffi.capsule_sdk.FfiDeviceTier
import java.io.File
import java.io.IOException
import java.util.logging.Level
import java.util.logging.Logger

/** Where the library stands for the screens. */
sealed interface LibraryState {
    data object Opening : LibraryState

    class Ready(
        val library: OpenedLibrary,
    ) : LibraryState

    /**
     * It did not open; [locked] when it exists and its unlock secret is gone, [hardwareRefused]
     * when the first launch could not enroll the device signing key in the keystore, which
     * offers software keys beside the retry.
     */
    class Failed(
        val locked: Boolean,
        val hardwareRefused: Boolean = false,
    ) : LibraryState
}

/**
 * Whether this device's library signs with software keys: chosen once, when the keystore
 * refused the device signing key and the person accepted the deviation, and kept so a later
 * launch never asks the keystore again over a library that already signs in software.
 */
interface SoftwareKeyChoice {
    fun chosen(): Boolean

    fun choose()
}

/**
 * [SoftwareKeyChoice] as a marker file beside the library, outside the root the core creates and removes.
 *
 * The marker is permanent: nothing clears it, because a library created on software keys records
 * that key and keeps opening on it, so the choice describes the library for its whole life. Moving
 * a device onto hardware later means creating a new library and enrolling it, which is a separate
 * flow that would remove this marker as part of that migration; it does not exist yet.
 */
class FileSoftwareKeyChoice(
    private val marker: File,
) : SoftwareKeyChoice {
    override fun chosen(): Boolean = marker.isFile

    override fun choose() {
        marker.parentFile?.mkdirs()
        if (!marker.isFile && !marker.createNewFile() && !marker.isFile) {
            throw IOException("could not record the software-key choice at ${marker.path}")
        }
    }
}

/**
 * Opens the library once, on [dispatcher], and holds it for the life of the process, so a
 * recreated activity finds it open rather than paying for the unlock again. Each attempt
 * builds its lifecycle through [lifecycle], on software keys once [softwareKeys] holds that
 * choice and on the device's element otherwise.
 */
class LibraryHolder(
    private val lifecycle: (softwareKeys: Boolean) -> LibraryLifecycle,
    private val softwareKeys: SoftwareKeyChoice,
    private val scope: CoroutineScope,
    private val dispatcher: CoroutineDispatcher = Dispatchers.IO,
) {
    private val mutable = MutableStateFlow<LibraryState>(LibraryState.Opening)

    val state: StateFlow<LibraryState> = mutable.asStateFlow()

    init {
        open()
    }

    /** Try again after a failure; does nothing while it is opening or open. */
    fun retry() {
        if (mutable.value is LibraryState.Failed) open()
    }

    /**
     * Accept the deviation the keystore's refusal offered: remember software keys for this
     * library and create it on them. Does nothing unless the last attempt was refused enrollment.
     */
    fun continueWithSoftwareKeys() {
        val failed = mutable.value as? LibraryState.Failed ?: return
        if (!failed.hardwareRefused) return
        mutable.value = LibraryState.Opening
        scope.launch(dispatcher) {
            try {
                softwareKeys.choose()
            } catch (error: IOException) {
                log.log(Level.SEVERE, "library: could not record the software-key choice", error)
                mutable.value = LibraryState.Failed(locked = false, hardwareRefused = true)
                return@launch
            }
            log.warning("library: continuing on software keys after the keystore refused the device signing key")
            attempt()
        }
    }

    private fun open() {
        mutable.value = LibraryState.Opening
        scope.launch(dispatcher) { attempt() }
    }

    private fun attempt() {
        val software = softwareKeys.chosen()
        log.info("library: opening (softwareKeys=$software)")
        mutable.value =
            try {
                LibraryState.Ready(lifecycle(software).open())
            } catch (error: LibraryLockedException) {
                log.log(Level.SEVERE, "library: locked", error)
                LibraryState.Failed(locked = true)
            } catch (error: HardwareEnrollmentRefusedException) {
                log.log(Level.SEVERE, "library: the keystore refused the device signing key", error)
                LibraryState.Failed(locked = false, hardwareRefused = true)
            } catch (
                @Suppress("TooGenericExceptionCaught") error: Exception,
            ) {
                // A workspace refusal (FfiException), an I/O failure, or a keystore error:
                // each leaves the library as it was, and the person can try again.
                log.log(Level.SEVERE, "library: did not open", error)
                LibraryState.Failed(locked = false)
            }
    }

    private companion object {
        val log: Logger = Logger.getLogger("capsule.workspace")
    }
}

/** The library and the server session the app's screens run on, built once per process. */
class WorkspaceGraph(
    context: Context,
) {
    private val app = context.applicationContext
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    // noBackupFilesDir: an auto-backup restored onto another device would carry the library
    // and its sealed secret but not the keystore key that opens it.
    private fun lifecycle(softwareKeys: Boolean) =
        LibraryLifecycle(
            root = File(app.noBackupFilesDir, "library"),
            secrets = UnlockSecretStore(File(app.noBackupFilesDir, "library-unlock.sealed"), KeystoreSecretSealer()),
            tier = deviceTier(app),
            client = FfiClientBuild(CLIENT_ID, versionName(app)),
            defaultAlbumName = app.getString(R.string.app_albums_default_name),
            // StrongBox where the device has a secure element, the TEE where it does not: the
            // key is in hardware either way, and requesting StrongBox where there is none fails.
            // Software keys only once the person accepted them after the keystore refused.
            signer =
                if (softwareKeys) {
                    null
                } else {
                    StrongBoxSigner(strongBoxBacked = app.packageManager.hasSystemFeature(PackageManager.FEATURE_STRONGBOX_KEYSTORE))
                },
        )

    val library: LibraryHolder by lazy {
        LibraryHolder(
            lifecycle = ::lifecycle,
            softwareKeys = FileSoftwareKeyChoice(File(app.noBackupFilesDir, "library-software-keys")),
            scope = scope,
        )
    }

    val sessions: ServerSessions by lazy { ServerSessions(SharedPreferencesServerAddressStore(app)) }

    private companion object {
        const val CLIENT_ID = "capsule-android"
        const val LOW_RAM_BYTES = 2L * 1024 * 1024 * 1024
        const val UNKNOWN_VERSION = "0.0.0"

        /** The app's own semver, which every manifest it authors reports beside the core's commit. */
        fun versionName(context: Context): String =
            context.packageManager.getPackageInfo(context.packageName, 0).versionName ?: UNKNOWN_VERSION

        fun deviceTier(context: Context): FfiDeviceTier {
            val manager = context.getSystemService(ActivityManager::class.java)
            val memory = ActivityManager.MemoryInfo().also { manager.getMemoryInfo(it) }
            return if (manager.isLowRamDevice || memory.totalMem <= LOW_RAM_BYTES) FfiDeviceTier.LOW_RAM else FfiDeviceTier.NORMAL
        }
    }
}

/** [ServerAddressStore] in the app's private preferences. */
class SharedPreferencesServerAddressStore(
    context: Context,
) : ServerAddressStore {
    private val prefs = context.applicationContext.getSharedPreferences("capsule.server", Context.MODE_PRIVATE)

    override fun load(): ServerAddress? = prefs.getString(ORIGIN, null)?.let(ServerAddress::parse)

    override fun save(address: ServerAddress) = prefs.edit().putString(ORIGIN, address.origin).apply()

    private companion object {
        const val ORIGIN = "origin"
    }
}
