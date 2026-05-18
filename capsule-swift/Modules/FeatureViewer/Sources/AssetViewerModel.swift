import AssetKit
import CapsuleFoundation
import Foundation
import Observation

/// Drives the full-screen viewer: which asset is shown, the info-panel sheet,
/// and the favourite / delete actions, which it routes through the
/// ``AssetProvider`` so they are testable against a mock.
@MainActor
@Observable
public final class AssetViewerModel {
    /// The assets being paged through.
    public private(set) var assets: [Asset]
    /// The index of the asset currently on screen.
    public var currentIndex: Int
    /// Whether the swipe-up info panel is presented.
    public var isInfoPanelPresented = false

    private let provider: any AssetProvider

    public init(assets: [Asset], startIndex: Int, provider: any AssetProvider) {
        self.assets = assets
        currentIndex = assets.isEmpty ? 0 : min(max(startIndex, 0), assets.count - 1)
        self.provider = provider
    }

    /// The asset currently on screen, if any.
    public var currentAsset: Asset? {
        assets.indices.contains(currentIndex) ? assets[currentIndex] : nil
    }

    /// Toggle the current asset's favourite flag through its provider.
    public func toggleFavorite() async {
        guard let asset = currentAsset else { return }
        let newValue = !asset.isFavorite
        do {
            try await provider.setFavorite(newValue, for: asset.id)
            if assets.indices.contains(currentIndex) {
                assets[currentIndex].isFavorite = newValue
            }
        } catch {
            CapsuleLog.interface.error("favorite toggle failed: \(String(describing: error), privacy: .public)")
        }
    }

    /// Delete the current asset.
    ///
    /// - Returns: `true` when the viewer should dismiss because no assets
    ///   remain. A cancelled system deletion prompt leaves the asset in place.
    public func deleteCurrentAsset() async -> Bool {
        guard let asset = currentAsset else { return assets.isEmpty }
        do {
            try await provider.delete([asset.id])
            assets.remove(at: currentIndex)
            if assets.isEmpty { return true }
            currentIndex = min(currentIndex, assets.count - 1)
            return false
        } catch {
            CapsuleLog.interface.error("delete failed: \(String(describing: error), privacy: .public)")
            return false
        }
    }
}
