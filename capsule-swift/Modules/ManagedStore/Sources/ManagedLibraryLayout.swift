import Foundation

/// The on-disk directory layout of a Capsule-managed library.
///
/// A pure value type: every path is *computed* from ``root``, so the layout is
/// trivially testable and carries no I/O. It is the mobile adaptation of the
/// `capsule-docs` filesystem design — media partitioned by capture date, a
/// rebuildable SQLite index, a cache of thumbnails, and a trash:
///
/// ```
/// <root>/
///   media/{YYYY}/{YYYY-MM}/{uuid}.{ext}   — imported media files
///   media/{YYYY}/{YYYY-MM}/{uuid}.cbor    — the paired sidecar
///   index/catalog.sqlite                  — the rebuildable catalog
///   index/thumbnails/                     — the thumbnail cache
///   .library/trash/                       — soft-deleted files
/// ```
///
/// Date partitioning uses the **UTC** calendar components of the capture date,
/// so a library stays portable across devices in different timezones.
public struct ManagedLibraryLayout: Sendable, Equatable {
    /// The library's root directory.
    public let root: URL

    public init(root: URL) {
        self.root = root
    }

    /// The default library root: `Application Support/CapsuleLibrary`.
    public static func defaultRoot() throws -> URL {
        let appSupport = try FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false
        )
        return appSupport.appending(path: "CapsuleLibrary", directoryHint: .isDirectory)
    }

    /// The root of date-partitioned media storage.
    public var mediaDirectory: URL {
        root.appending(path: "media", directoryHint: .isDirectory)
    }

    /// The directory holding the rebuildable index.
    public var indexDirectory: URL {
        root.appending(path: "index", directoryHint: .isDirectory)
    }

    /// The SQLite catalog file.
    public var catalogFile: URL {
        indexDirectory.appending(path: "catalog.sqlite")
    }

    /// The thumbnail cache directory.
    public var thumbnailsDirectory: URL {
        indexDirectory.appending(path: "thumbnails", directoryHint: .isDirectory)
    }

    /// The trash directory for soft-deleted files.
    public var trashDirectory: URL {
        root
            .appending(path: ".library", directoryHint: .isDirectory)
            .appending(path: "trash", directoryHint: .isDirectory)
    }

    /// Every directory that must exist for the library to be usable — the set
    /// the store creates when initialising a fresh library.
    public var skeletonDirectories: [URL] {
        [mediaDirectory, indexDirectory, thumbnailsDirectory, trashDirectory]
    }

    /// The `media/{YYYY}/{YYYY-MM}` directory for a capture date.
    public func mediaDirectory(forCaptureDate date: Date) -> URL {
        let parts = Self.datePartition(for: date)
        return mediaDirectory
            .appending(path: parts.year, directoryHint: .isDirectory)
            .appending(path: parts.month, directoryHint: .isDirectory)
    }

    /// The media file path for an asset: `media/{YYYY}/{YYYY-MM}/{uuid}.{ext}`.
    public func mediaFile(uuid: String, fileExtension: String, captureDate: Date) -> URL {
        mediaDirectory(forCaptureDate: captureDate)
            .appending(path: uuid)
            .appendingPathExtension(fileExtension)
    }

    /// The sidecar file paired with an asset: `…/{uuid}.cbor`.
    public func sidecarFile(uuid: String, captureDate: Date) -> URL {
        mediaDirectory(forCaptureDate: captureDate)
            .appending(path: uuid)
            .appendingPathExtension("cbor")
    }

    /// The sidecar paired with a given media file — its sibling, same stem.
    public func sidecarFile(forMediaFile mediaFile: URL) -> URL {
        mediaFile.deletingPathExtension().appendingPathExtension("cbor")
    }

    // MARK: - Date partitioning

    /// `YYYY` / `YYYY-MM` strings from a date's UTC calendar components.
    static func datePartition(for date: Date) -> (year: String, month: String) {
        var calendar = Calendar(identifier: .gregorian)
        // swiftlint:disable:next force_unwrapping
        calendar.timeZone = TimeZone(identifier: "UTC")!
        let parts = calendar.dateComponents([.year, .month], from: date)
        let year = parts.year ?? 0
        let month = parts.month ?? 0
        return (
            year: String(format: "%04d", year),
            month: String(format: "%04d-%02d", year, month)
        )
    }
}
