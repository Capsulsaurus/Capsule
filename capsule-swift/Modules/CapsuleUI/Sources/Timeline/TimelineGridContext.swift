import CapsuleFoundation
import CoreGraphics
import Observation

/// The state every timeline cell observes but no cell owns: how large to
/// decode, and what is selected.
///
/// The same device ``PhotoGridContext`` uses, for the same reason. A cell is
/// *configured* once and then lives on inside a recycled platform cell, so any
/// state that changes afterwards — a window resize, a selection, entering a
/// culling pass — has to reach it through observation rather than through the
/// configuration closure, which is never re-run. That is what makes a selection
/// change repaint the visible tiles with no snapshot reload, no
/// `reconfigureItems`, and no hand-written visible-cell loop on either platform.
///
/// Kept separate from ``AssetWindowStore`` deliberately: the store publishes
/// *content* arriving, this publishes *presentation* changing. Merging them
/// would make every page fetch invalidate every selection-dependent view.
@MainActor
@Observable
final class TimelineGridContext {
    /// The device-pixel size a tile decodes to; `.zero` until measured.
    var decodeSize: CGSize = .zero
    /// Whether the grid is in multi-select mode.
    var isSelecting = false
    /// The selected assets, in select mode.
    var selectedIDs: Set<AssetID> = []
    /// Whether a culling pass is running, which is the only time a cull flag is
    /// drawn in the timeline.
    var showsCullFlags = false

    /// Whether `id` should render its selected treatment.
    func isSelected(_ id: AssetID) -> Bool {
        isSelecting && selectedIDs.contains(id)
    }
}
