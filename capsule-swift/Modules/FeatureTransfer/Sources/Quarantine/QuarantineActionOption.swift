import CapsuleDomain
import Foundation

// MARK: - QuarantineActionOption

/// One of the three explicit resolutions, with nothing implied.
///
/// **There is no default.** Automatic resolution of a quarantine is the same
/// thing as silently applying or silently dropping, which is the behaviour the
/// whole surface exists to prevent (*Threat Model — Quarantine Surfaces*). So
/// ``isDefault`` is a stored `false` on every option rather than a computed
/// property some future edit could flip for one case: no keyboard default, no
/// prominent styling, no pre-selection.
public struct QuarantineActionOption: Sendable, Equatable, Identifiable {
    public var resolution: QuarantineResolution
    /// Whether the item's holding area preserves enough state for the
    /// resolution to mean anything.
    public var isEnabled: Bool
    /// Why it is unavailable, when it is.
    public var unavailableReasonKey: String?

    public var id: String { "\(resolution)" }

    /// Destroys the preserved bytes. Irreversible.
    public var isDestructive: Bool { resolution.isDestructive }

    /// Every destructive resolution confirms, and only destructive ones do.
    public var requiresConfirmation: Bool { resolution.isDestructive }

    /// Always `false`. See the type's documentation.
    public var isDefault: Bool { false }

    public init(resolution: QuarantineResolution, isEnabled: Bool, unavailableReasonKey: String? = nil) {
        self.resolution = resolution
        self.isEnabled = isEnabled
        self.unavailableReasonKey = unavailableReasonKey
    }

    /// The three options for an item, always all three, always in this order.
    ///
    /// Repair is *shown but disabled* where it is meaningless rather than
    /// hidden: a user who cannot see the option cannot learn why it does not
    /// apply, and an audit-log entry genuinely has nothing to repair.
    public static func options(for item: QuarantineItem) -> [QuarantineActionOption] {
        [
            QuarantineActionOption(
                resolution: .inspect,
                isEnabled: item.surface.storage.preservesOriginalBytes,
                unavailableReasonKey: item.surface.storage.preservesOriginalBytes
                    ? nil
                    : "ios.quarantine.action.inspect.unavailable"
            ),
            QuarantineActionOption(
                resolution: .repair,
                isEnabled: item.isRecoverable,
                unavailableReasonKey: item.isRecoverable ? nil : "ios.quarantine.action.repair.unavailable"
            ),
            QuarantineActionOption(resolution: .discard, isEnabled: true),
        ]
    }
}
