import CapsuleFoundation
import Foundation

// MARK: - RegionID

/// A labelled region of an image (*Asset Organization — Labels*). UUIDv7,
/// minted fresh per create, so a deleted region's id is never reused.
public struct RegionID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

// MARK: - UserLabel

/// The one label rule, at every level a label attaches to — asset, image
/// region, stack and album (*Asset Organization — Labels*).
///
/// A label is NFC-normalized, trimmed of surrounding whitespace, 1 to
/// ``maxBytes`` UTF-8 bytes, and case-preserving. It carries no hierarchy and
/// no type. Mirrors `capsule_core::sidecar::normalize_label`, which is what the
/// core refuses on write; an editor checks here first so a refusal is shown
/// before anything is sent.
public enum UserLabel {
    /// The largest label, in UTF-8 bytes after normalization.
    public static let maxBytes = 256

    /// Why a label was refused.
    public enum Refusal: Error, Sendable, Equatable {
        /// Nothing is left once the label is trimmed.
        case empty
        /// The normalized label is this many bytes, past ``UserLabel/maxBytes``.
        case tooLong(bytes: Int)
    }

    /// Normalize `raw` into a storable label: NFC, then trimmed, then checked
    /// against the byte bound.
    public static func normalize(_ raw: String) throws(Refusal) -> String {
        let trimmed = raw.precomposedStringWithCanonicalMapping
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { throw .empty }
        let bytes = trimmed.utf8.count
        guard bytes <= maxBytes else { throw .tooLong(bytes: bytes) }
        return trimmed
    }
}

// MARK: - RegionShape

/// A region's outline, in coordinates normalized to `[0, 1]` over the
/// original's **display orientation** (after EXIF orientation), so it survives
/// every derivative size.
///
/// Mirrors `capsule_core::sidecar::RegionShape`, including its single-precision
/// coordinates: a shape that validates here validates in the core.
public enum RegionShape: Sendable, Equatable, Hashable {
    /// A circle centred at `(centerX, centerY)` (fractions of width and height)
    /// with a radius that is a fraction of the image's **shorter** side, so it
    /// is round on any aspect ratio. The core's `cx`, `cy` and `r`.
    case circle(centerX: Float, centerY: Float, radius: Float)
    /// A rectangle with its top-left corner at `(left, top)`. The core's `x`,
    /// `y`, `w` and `h`.
    case rect(left: Float, top: Float, width: Float, height: Float)

    /// Why a shape was refused.
    public enum Refusal: Error, Sendable, Equatable {
        /// A coordinate is not a finite number in `[0, 1]`.
        case outOfRange
        /// A radius, width or height is zero.
        case empty
        /// A rectangle extends past the right or bottom edge, or a circle past
        /// any edge.
        case pastEdge
    }

    /// Check the normalized-coordinate contract: every value finite and in
    /// `[0, 1]`, a positive extent, and the shape wholly inside the image.
    ///
    /// A circle spans exactly its radius of the shorter axis and the shape does
    /// not record which axis that is, so it must fit the radius from its centre
    /// on both.
    public func validate() throws(Refusal) {
        let unit = { (value: Float) in value.isFinite && (0 ... 1).contains(value) }
        switch self {
        case let .circle(centerX, centerY, radius):
            guard [centerX, centerY, radius].allSatisfy(unit) else { throw .outOfRange }
            guard radius > 0 else { throw .empty }
            guard centerX >= radius, centerY >= radius, centerX + radius <= 1, centerY + radius <= 1
            else { throw .pastEdge }
        case let .rect(left, top, width, height):
            guard [left, top, width, height].allSatisfy(unit) else { throw .outOfRange }
            guard width > 0, height > 0 else { throw .empty }
            guard left + width <= 1, top + height <= 1 else { throw .pastEdge }
        }
    }
}

// MARK: - AssetRegion

/// One live region of an asset and its labels.
///
/// A region's labels are an OR-set of their own, so two devices labelling one
/// region concurrently both keep their label; a delete is final and absorbs
/// every concurrent edit.
public struct AssetRegion: Sendable, Equatable, Hashable, Identifiable {
    public var id: RegionID
    /// The outline.
    public var shape: RegionShape
    /// The region's labels, sorted.
    public var labels: [String]

    public init(id: RegionID, shape: RegionShape, labels: [String]) {
        self.id = id
        self.shape = shape
        self.labels = labels
    }
}

// MARK: - LabelTarget

/// What a label is attached to, beyond the asset itself. The label rule is the
/// same at every level.
///
/// An asset's own labels are its user tags, an add-id OR-set edited through
/// `OrganizePort`; they are not a target here, so there is one write path for
/// them and not two.
public enum LabelTarget: Sendable, Equatable, Hashable {
    /// A region of an asset.
    case region(asset: AssetID, region: RegionID)
    /// A stack.
    case stack(StackID)
    /// An album.
    case album(AlbumID)
}

// MARK: - LabelTargetRefusal

/// Why a label or region edit named nothing to edit.
public enum LabelTargetRefusal: Error, Sendable, Equatable {
    /// The asset, region, stack or album does not exist, or no longer does: a
    /// deleted region's id is never live again.
    case notFound
}
