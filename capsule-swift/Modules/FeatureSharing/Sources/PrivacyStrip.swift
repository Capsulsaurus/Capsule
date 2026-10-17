import Foundation

// MARK: - PrivacyStripField

/// One row of the boundary-crossing metadata strip
/// (*Metadata — Privacy on Export*).
///
/// The table is normative and closed: these five fields, with these two
/// dispositions, are what the strip does. Modelling it as a value type rather
/// than as copy inside a view means the share composer and the export sheet
/// enumerate the *same* list — a user cannot be told two different stories
/// about what leaves their library, and a test can assert the list is complete.
///
/// A "boundary crossing" is a share link served to a non-member, an export to
/// media the user will hand off, or a federated peer outside the owner's home
/// server. Capsule's own devices syncing the same library are **not** a
/// crossing; that is intra-trust, and nothing is stripped.
public enum PrivacyStripField: String, Sendable, Equatable, Hashable, Identifiable, CaseIterable {
    /// Uniquely links every photograph to one physical camera body.
    case cameraSerial
    /// The importing device (UUIDv4).
    case deviceIdentifier
    /// The session the import happened in (UUIDv7).
    case sessionIdentifier
    /// Capture coordinates.
    case location
    /// Faces matched to a known person.
    case contactTags

    public var id: String { rawValue }

    /// What the default export does to this field.
    public var disposition: Disposition {
        self == .location ? .reduced : .removed
    }

    /// How a field is treated when it crosses a trust boundary.
    public enum Disposition: Sendable, Equatable, Hashable {
        /// Removed outright.
        case removed
        /// Kept but coarsened — GPS is rounded to 2 decimal places, roughly a
        /// kilometre.
        case reduced
    }
}

// MARK: - PrivacyStripPolicy

/// Whether the strip may be waived on this surface, and whether it currently
/// is.
///
/// Two surfaces, two answers, and the difference is not cosmetic:
///
/// - **Share links have no opt-out at all.** A public share *is* a boundary
///   crossing by definition, so there is no toggle to render — and the UI says
///   so plainly rather than implying a setting exists somewhere else
///   (*Share Links — Metadata Stripping*).
/// - **Export offers a per-export opt-in to retain**, which resets to off every
///   single time. Deliberately not a sticky account setting: the foot-gun the
///   design names by name is a user who opts in once and forgets
///   (*Metadata — Privacy on Export*).
public struct PrivacyStripPolicy: Sendable, Equatable, Hashable {
    /// Whether this surface has a retain control at all.
    public let allowsRetention: Bool
    /// Whether the user has opted in **for this one operation**.
    public private(set) var retainsIdentifyingMetadata: Bool

    private init(allowsRetention: Bool) {
        self.allowsRetention = allowsRetention
        retainsIdentifyingMetadata = false
    }

    /// The share-link policy: mandatory, no control, never waivable.
    public static let mandatory = PrivacyStripPolicy(allowsRetention: false)

    /// The export policy: opt-in available, and off at the start of every
    /// export.
    public static let perExportOptIn = PrivacyStripPolicy(allowsRetention: true)

    /// Set the opt-in. A no-op where retention is not offered, so a caller
    /// cannot waive the share-link strip by reaching past the UI.
    public mutating func setRetention(_ retain: Bool) {
        guard allowsRetention else { return }
        retainsIdentifyingMetadata = retain
    }

    /// Return to the default. Called at the *start* of every export rather than
    /// at the end, so a crash, a cancel, or a dismissed sheet cannot leave the
    /// opt-in armed for the next one.
    public mutating func reset() {
        retainsIdentifyingMetadata = false
    }

    /// What actually happens to each field under this policy.
    public func effectiveDisposition(for field: PrivacyStripField) -> PrivacyStripField.Disposition? {
        retainsIdentifyingMetadata ? nil : field.disposition
    }
}
