package com.justin13888.capsule.feature.settings

import androidx.compose.runtime.Immutable
import com.justin13888.capsule.core.data.CapsuleError
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ConnectionClass
import com.justin13888.capsule.core.model.Device
import com.justin13888.capsule.core.model.ErrorCode
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.LibrarySettings
import com.justin13888.capsule.core.model.SyncScope
import com.justin13888.capsule.core.model.UploadPolicy

/**
 * A refused read or write, reduced to what the user can act on.
 *
 * Carries the stable `error.*` code and nothing else — no status, no English detail — because
 * the code is the contract a client localizes and a status is a transport artefact two
 * deployments may spell differently for the same condition. [isWrite] decides the remediation:
 * a refused write can be retried, a refused read reloads.
 */
@Immutable
data class SettingsFailure(
    /** The catalogue code to localize, or `null` for a device-local failure. */
    val code: ErrorCode?,
    /** Whether the refusal came from saving rather than from reading. */
    val isWrite: Boolean,
) {
    internal companion object {
        /** Wraps whatever a repository threw. */
        fun of(
            throwable: Throwable,
            isWrite: Boolean,
        ): SettingsFailure = SettingsFailure(code = (throwable as? CapsuleError)?.code, isWrite = isWrite)
    }
}

/**
 * One container album, as a destination the user can pick.
 *
 * [name] is empty for the default album, which is nameless by design — the screen substitutes
 * catalogue copy rather than the data inventing a name that would then need translating.
 */
@Immutable
data class AlbumChoice(
    /** The album. */
    val id: AlbumId,
    /** Its user-assigned name; empty for the default album. */
    val name: String,
    /** Whether it is the owner's currently designated default. */
    val isDefault: Boolean,
)

/**
 * One import source and where its photos land.
 *
 * The source is identified by its locator — on Android the bucket's *relative path*
 * (`DCIM/Camera`), never its bucket id, which is a hash of the display name and differs
 * across devices and OS versions. Two phones pointed at the same folder have to compute the
 * same scope, and a bucket id would give them different ones.
 */
@Immutable
data class ScopeMapping(
    /** The canonical identity of the source. */
    val scope: ImportScope,
    /** Where its imports currently land, or `null` when it has never been asked. */
    val destination: AlbumChoice?,
)

/**
 * Everything the settings screens draw, as one value.
 *
 * One state for five pages, because on a wide window two of them are on screen at once and a
 * per-page state would let the list and the open pane disagree about what was just saved.
 */
@Immutable
data class SettingsUiState(
    /** Whether the first read is still in flight. */
    val isLoading: Boolean = true,
    /** A refused read or write, or `null`. */
    val failure: SettingsFailure? = null,
    /** The preferences that belong to this device. */
    val device: DeviceSettings = DeviceSettings(),
    /** The preferences that belong to the library and sync between devices. */
    val library: LibrarySettings = LibrarySettings(),
    /** How much of each new asset this library fetches. */
    val syncScope: SyncScope = SyncScope.MetadataAndThumbnails,
    /** Whether upload sessions open eagerly or in tier order. */
    val uploadPolicy: UploadPolicy = UploadPolicy.Full,
    /** The device's current path quality. */
    val connection: ConnectionClass = ConnectionClass.Unmetered,
    /** Every import source this device has seen, with where it lands. */
    val scopes: List<ScopeMapping> = emptyList(),
    /** The container albums an import can be pointed at. */
    val albums: List<AlbumChoice> = emptyList(),
    /** The import source whose destination the user is currently choosing. */
    val editingScope: ImportScope? = null,
    /** Whether a session against a server exists at all. Local-only is a valid mode. */
    val isSignedIn: Boolean = false,
    /** This device, as the signed directory carries it. */
    val thisDevice: Device? = null,
    /** What this build is. */
    val build: AppBuildInfo = AppBuildInfo.UNKNOWN,
    /** Whether this platform has a per-app language setting to apply a choice to. */
    val perAppLanguageSupported: Boolean = true,
) {
    /** Whether the screens have something real to render. */
    val isReady: Boolean get() = !isLoading

    /** The mapping for [scope], if this device has seen it. */
    fun mappingFor(scope: ImportScope?): ScopeMapping? =
        scope?.let { wanted ->
            scopes.firstOrNull { it.scope.locator == wanted.locator }
        }
}
