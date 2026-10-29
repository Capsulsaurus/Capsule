import CapsuleFoundation

/// Whether a backing store holds an asset.
///
/// The question a composite asks before routing a per-asset write, because the
/// identifier alone cannot answer it: the mock library and the managed store
/// both mint `.managed(uuid:)` identifiers, so two sources can hand out ids of
/// the same shape. A store that conforms answers from its own index, exactly,
/// and a composite asks it before falling back to the source that owns
/// everything else.
public protocol AssetOwning: Sendable {
    /// Whether this store indexes `id`, including an asset in its trash.
    func owns(_ id: AssetID) async -> Bool
}
