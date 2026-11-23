package com.justin13888.capsule.feature.importer

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.ImportScope
import com.justin13888.capsule.core.model.PlatformTag
import com.justin13888.capsule.core.model.SourceKind
import com.justin13888.capsule.core.model.asWire
import com.justin13888.capsule.core.model.knownOrNull

/**
 * One place the user can point an import at, as the picker draws it.
 *
 * Wraps [ImportScope] rather than replacing it so the identity the rest of the system uses —
 * platform, source kind, and the platform's stable locator — is the identity the picker
 * shows. In particular the locator is always the bucket's **relative path** (`DCIM/Camera`),
 * never a `MediaStore` bucket id: the id is a hash of the display name and differs across
 * devices and OS versions, so two phones pointed at the same folder would otherwise map to
 * two different scopes and be asked the destination question twice.
 */
data class ImportSource(
    /** The canonical scope this row stands for. */
    val scope: ImportScope,
    /** How many candidate files the last scan counted, or `null` before any scan. */
    val itemCount: Int? = null,
    /** The album this source's imports have been mapped to, or `null` while unmapped. */
    val destination: AlbumId? = null,
) {
    /** The platform's stable spelling of the source — the relative path on Android. */
    val locator: String get() = scope.locator

    /** What kind of place this is, or `null` for a token this build does not know. */
    val kind: SourceKind? get() = scope.sourceKind.knownOrNull

    /** Whether the destination question has already been asked and answered for this source. */
    val isMapped: Boolean get() = destination != null

    companion object {
        /**
         * An ad-hoc source the user pointed at once — a picked folder, a Takeout archive.
         *
         * Minted here rather than waiting for the repository to list it because the picker has
         * to be able to show a folder the moment the system picker returns one, and a source
         * that only appears after a round trip reads as a dropped tap.
         */
        fun adHoc(
            locator: String,
            kind: SourceKind,
        ): ImportSource =
            ImportSource(
                scope =
                    ImportScope(
                        platform = PlatformTag.Android.asWire(),
                        sourceKind = kind.asWire(),
                        locator = locator,
                    ),
            )
    }
}

/**
 * The library volume's free space, as the plan's confirmation step needs it.
 *
 * A port of its own rather than a call on [com.justin13888.capsule.core.data.StorageRepository]
 * because it is a **local filesystem** question, not a server one: the design puts
 * `available_bytes()` outside the pure planner precisely so the planner stays reproducible,
 * and putting it behind an interface here is what lets a test drive the constrained case
 * without a full disk.
 */
fun interface FreeSpaceProbe {
    /** Bytes currently available on the volume the library is stored on. */
    suspend fun availableBytes(): Long
}
