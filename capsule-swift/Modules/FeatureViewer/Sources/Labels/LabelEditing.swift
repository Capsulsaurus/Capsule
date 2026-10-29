import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import SwiftUI

// MARK: - LabelEditing

/// What the label and region editor needs from the composition root: the port
/// it writes through, and how to find the stack an asset belongs to.
///
/// Carried in the SwiftUI environment rather than threaded through every
/// screen that presents the viewer: the viewer is opened from the timeline,
/// albums, search and places, and each would otherwise take two more
/// arguments it never reads. Absent, the editor's entry points are not shown.
public struct LabelEditing: Sendable {
    /// Labels on regions, stacks and albums, and the regions themselves.
    public let port: any LabelPort
    /// The stack `asset` belongs to, if any; the editor offers the stack's
    /// labels beside the asset's regions.
    public let stackOf: @Sendable (AssetID) async -> StackID?

    public init(
        port: any LabelPort,
        stackOf: @escaping @Sendable (AssetID) async -> StackID? = { _ in nil }
    ) {
        self.port = port
        self.stackOf = stackOf
    }
}

private struct LabelEditingKey: EnvironmentKey {
    static let defaultValue: LabelEditing? = nil
}

public extension EnvironmentValues {
    /// The label editor's ports, or `nil` where labels cannot be edited.
    var labelEditing: LabelEditing? {
        get { self[LabelEditingKey.self] }
        set { self[LabelEditingKey.self] = newValue }
    }
}
