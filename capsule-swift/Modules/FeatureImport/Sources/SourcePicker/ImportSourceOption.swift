import CapsuleDomain
import Foundation

// MARK: - ImportSourceOption

/// One row on the source picker.
///
/// A *presentation* catalog rather than a domain type: ``SourceKind`` says what
/// a source is, this says how it is offered. The two differ because one kind can
/// be offered two ways — a folder is both "Files…" on a handheld and a watched
/// directory on a Mac — and because the sentence under each row is copy, which
/// has no business inside a wire enum.
public struct ImportSourceOption: Sendable, Equatable, Identifiable {
    /// A stable id, distinct from the kind: two options may share a kind.
    public var id: String
    public var kind: SourceKind
    /// What the row is called.
    public var titleKey: String
    /// What tapping it will *do*. Every row states this — an import source whose
    /// consequences are not on the row is a source a user cannot consent to.
    public var detailKey: String
    public var symbol: String
    /// Whether the user must point at a location before this can be scanned.
    ///
    /// The auto-discovered sources come back from `import.list_scopes` already
    /// identified; a folder or an archive does not exist until somebody picks
    /// one.
    public var requiresLocatorPick: Bool
    /// Whether the option needs a user-visible file system, and so is absent on
    /// a handheld.
    public var requiresDesktopFileSystem: Bool

    public init(
        id: String,
        kind: SourceKind,
        titleKey: String,
        detailKey: String,
        symbol: String,
        requiresLocatorPick: Bool,
        requiresDesktopFileSystem: Bool = false
    ) {
        self.id = id
        self.kind = kind
        self.titleKey = titleKey
        self.detailKey = detailKey
        self.symbol = symbol
        self.requiresLocatorPick = requiresLocatorPick
        self.requiresDesktopFileSystem = requiresDesktopFileSystem
    }

    /// Every source the product offers, in the order the picker lists them.
    ///
    /// Ordered by how many people will use it, not by how interesting it is:
    /// the photo library is what almost every import is, and a Takeout archive
    /// is a migration people do once.
    public static let catalog: [ImportSourceOption] = [
        ImportSourceOption(
            id: "photos",
            kind: .cameraRoll,
            titleKey: "app.import.source.photos.title",
            detailKey: "app.import.source.photos.detail",
            symbol: "photo.on.rectangle.angled",
            requiresLocatorPick: false
        ),
        ImportSourceOption(
            id: "files",
            kind: .folder,
            titleKey: "app.import.source.files.title",
            detailKey: "app.import.source.files.detail",
            symbol: "folder",
            requiresLocatorPick: true
        ),
        ImportSourceOption(
            id: "watched-folder",
            kind: .watchedDirectory,
            titleKey: "app.import.source.watched_folder.title",
            detailKey: "app.import.source.watched_folder.detail",
            symbol: "folder.badge.gearshape",
            requiresLocatorPick: true,
            requiresDesktopFileSystem: true
        ),
        ImportSourceOption(
            id: "removable-volume",
            kind: .removableVolume,
            titleKey: "app.import.source.removable_volume.title",
            detailKey: "app.import.source.removable_volume.detail",
            symbol: "sdcard",
            requiresLocatorPick: false,
            requiresDesktopFileSystem: true
        ),
        ImportSourceOption(
            id: "takeout",
            kind: .takeoutArchive,
            titleKey: "app.import.source.takeout.title",
            detailKey: "app.import.source.takeout.detail",
            symbol: "shippingbox",
            requiresLocatorPick: true
        ),
    ]

    /// The catalog filtered to what a platform can actually do.
    ///
    /// Filtered rather than disabled: a greyed-out "Watched folder" on an iPhone
    /// implies a permission the user could grant, and there is none — the OS has
    /// no persistent folder watch to grant.
    public static func available(on platform: ImportPlatform) -> [ImportSourceOption] {
        catalog.filter { option in
            switch option.kind {
            case .watchedDirectory: platform.watchesFolders
            case .removableVolume: platform.mountsRemovableVolumes
            default: !option.requiresDesktopFileSystem
            }
        }
    }
}

// MARK: - ImportSourceRow

/// An option paired with whatever the device has already discovered for it.
///
/// The pairing is what decides the row's affordance: a discovered scope can be
/// scanned on the tap, while everything else needs a location first. Computing
/// it once here keeps that decision out of the view body, where it would be
/// re-derived per redraw and be untestable.
public struct ImportSourceRow: Sendable, Equatable, Identifiable {
    public var option: ImportSourceOption
    /// The scope `import.list_scopes` already found for this kind, if any.
    public var discovered: ImportScope?

    public var id: String { option.id }

    public init(option: ImportSourceOption, discovered: ImportScope? = nil) {
        self.option = option
        self.discovered = discovered
    }

    /// Whether tapping starts a scan directly.
    public var scansImmediately: Bool {
        discovered != nil && !option.requiresLocatorPick
    }

    /// Whether tapping opens a location picker.
    public var needsLocation: Bool {
        !scansImmediately
    }
}
