import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Favourite derivation

public extension Asset {
    /// The reserved user tag that carries the favourite flag.
    ///
    /// ``LibraryAsset`` has no favourite field, so the flag has to be derived
    /// from something the sidecar already stores. The two candidates are the
    /// star rating and a reserved entry in the user-tag OR-set, and the tag is
    /// the only non-lossy one:
    ///
    /// - **Rating would destroy data.** ``LibraryAsset/rating`` is a 0–5 scale
    ///   the domain deliberately keeps orthogonal to every other flag — the
    ///   ``OrganizePort`` docs call out that conflating rating with a second
    ///   meaning "forces a lossy workflow". Treating "rating ≥ 1" as favourite
    ///   means un-favouriting a five-star photograph silently zeroes its
    ///   rating, and favouriting an unrated one invents a star the user never
    ///   gave it.
    /// - **A reserved tag is additive.** An OR-set add and its matching remove
    ///   touch nothing else on the sidecar, converge across devices with no
    ///   conflict dialog, and round-trip exactly. The dotted, namespaced
    ///   spelling keeps it out of the mock's own token vocabulary, so it cannot
    ///   collide with a tag the library derived.
    ///
    /// It is a **tag, not a hidden field**: it shows up in the asset's tag list
    /// like any other, which is honest — the app really is storing it there.
    static let favoriteTag = "capsule.favorite"
}

public extension LibraryAsset {
    /// Whether the user has favourited this asset, per ``Asset/favoriteTag``.
    var isFavorite: Bool {
        tagsUser.contains(Asset.favoriteTag)
    }
}

// MARK: - Projection

public extension Asset {
    /// Project a ``LibraryAsset`` onto the value type the existing screens
    /// render.
    ///
    /// Mostly a narrowing: both sides already share ``AssetID`` and
    /// ``MediaType`` from `CapsuleFoundation`, so nothing is re-keyed and no
    /// identifier is minted. The three conversions that are not pure copies:
    ///
    /// - **The timeline axis.** ``captureDate`` comes from
    ///   ``LibraryAsset/effectiveCaptureTimestamp`` — the UTC capture instant
    ///   when the zone was resolved, else the device wall clock — because that
    ///   is the single field the domain says every sort, section boundary, and
    ///   date filter reads. Taking ``CaptureTime/captureTimestamp`` directly
    ///   would reorder the library the moment a photo taken abroad appears.
    /// - **Duration.** Milliseconds to `TimeInterval` seconds; a still photo
    ///   carries no duration at all, which becomes `0` rather than a sentinel.
    /// - **Dimensions.** `nil` becomes `0 × 0`, which ``Asset/aspectRatio``
    ///   already reads as "unknown, lay it out square".
    ///
    /// Lifecycle state (trashed, stack-hidden, user-hidden) is deliberately
    /// **not** projected: `Asset` has nowhere to put it, and the caller selects
    /// a slice through ``TimelineQuery`` instead — which is the one place those
    /// three flags cannot be confused for one another.
    init(libraryAsset: LibraryAsset) {
        self.init(
            id: libraryAsset.id,
            mediaType: libraryAsset.mediaType,
            captureDate: libraryAsset.effectiveCaptureTimestamp.date,
            pixelWidth: Int(libraryAsset.dimensions?.width ?? 0),
            pixelHeight: Int(libraryAsset.dimensions?.height ?? 0),
            duration: libraryAsset.durationMilliseconds.map { Double($0) / 1000 } ?? 0,
            isFavorite: libraryAsset.isFavorite
        )
    }
}
