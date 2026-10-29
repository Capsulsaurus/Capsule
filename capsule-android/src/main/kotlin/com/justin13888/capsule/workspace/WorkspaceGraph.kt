package com.justin13888.capsule.workspace

import android.app.ActivityManager
import android.content.Context
import com.justin13888.capsule.R
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
import java.util.logging.Level
import java.util.logging.Logger

/** Where the library stands for the screens. */
sealed interface LibraryState {
    data object Opening : LibraryState

    class Ready(
        val library: OpenedLibrary,
    ) : LibraryState

    /** It did not open; [locked] when it exists and its unlock secret is gone. */
    class Failed(
        val locked: Boolean,
    ) : LibraryState
}

/**
 * Opens [lifecycle]'s library once, on [dispatcher], and holds it for the life of the
 * process, so a recreated activity finds it open rather than paying for the unlock again.
 */
class LibraryHolder(
    private val lifecycle: LibraryLifecycle,
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

    private fun open() {
        mutable.value = LibraryState.Opening
        scope.launch(dispatcher) {
            mutable.value =
                try {
                    LibraryState.Ready(lifecycle.open())
                } catch (error: LibraryLockedException) {
                    log.log(Level.SEVERE, "library: locked", error)
                    LibraryState.Failed(locked = true)
                } catch (
                    @Suppress("TooGenericExceptionCaught") error: Exception,
                ) {
                    // A workspace refusal (FfiException), an I/O failure, or a keystore error:
                    // each leaves the library as it was, and the person can try again.
                    log.log(Level.SEVERE, "library: did not open", error)
                    LibraryState.Failed(locked = false)
                }
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
    private val lifecycle =
        LibraryLifecycle(
            root = File(app.noBackupFilesDir, "library"),
            secrets = UnlockSecretStore(File(app.noBackupFilesDir, "library-unlock.sealed"), KeystoreSecretSealer()),
            tier = deviceTier(app),
            client = FfiClientBuild(CLIENT_ID, versionName(app)),
            defaultAlbumName = app.getString(R.string.app_albums_default_name),
        )

    val library: LibraryHolder by lazy { LibraryHolder(lifecycle, scope) }

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
