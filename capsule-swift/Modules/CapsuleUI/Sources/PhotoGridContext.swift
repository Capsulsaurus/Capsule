import CapsuleFoundation
import CoreGraphics
import Observation

/// The mutable state every grid cell observes: how large to decode, and what is
/// selected.
///
/// Cells are hosted SwiftUI views inside recycled platform cells, which means
/// they are *configured* once and then live on. Routing the state that changes
/// after configuration through one observable object is what lets a selection
/// change or a window resize re-render the visible cells through SwiftUI's own
/// observation, with no snapshot reload, no `reconfigureItems`, and no
/// hand-written "refresh the visible cells" loop on either platform.
@MainActor
@Observable
final class PhotoGridContext {
    /// The device-pixel size a tile decodes to; `.zero` until measured.
    var decodeSize: CGSize = .zero
    /// Whether the grid is in multi-select mode.
    var isSelecting = false
    /// The selected assets, in select mode.
    var selectedIDs: Set<AssetID> = []

    /// Whether `id` should render its selected treatment.
    func isSelected(_ id: AssetID) -> Bool {
        isSelecting && selectedIDs.contains(id)
    }
}
