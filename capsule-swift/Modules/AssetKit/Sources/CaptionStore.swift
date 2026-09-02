import CapsuleFoundation
import Foundation

/// Reads and writes an asset's caption.
///
/// A protocol of its own rather than two more methods on ``AssetProvider``,
/// because captions are the first thing the viewer *writes* that is neither a
/// flag nor a lifecycle change, and because the two backing stores answer it
/// very differently: the Capsule library keeps a CRDT register with a superseded
/// log behind it, while the system photo library has no caption concept at all.
/// A narrow protocol lets the second answer honestly instead of pretending.
///
/// Concurrent edits are **not** last-writer-wins-and-forget. A losing write is
/// preserved in the sidecar's superseded log, which is what makes an offline
/// edit safe to make; surfacing that log is `S-U8`'s restore affordance and is
/// not built here.
public protocol CaptionStore: Sendable {
    /// The current caption, or `nil` when the asset has none — or when this
    /// store does not own the asset.
    func caption(for id: AssetID) async -> String?

    /// Set or clear the caption. Clearing is `nil`, not an empty string: an
    /// empty caption and no caption are the same fact and should not be two
    /// states.
    func setCaption(_ caption: String?, for id: AssetID) async throws
}
