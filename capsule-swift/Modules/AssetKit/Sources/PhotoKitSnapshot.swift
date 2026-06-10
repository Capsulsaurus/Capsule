import Foundation
import Photos

/// An ``AssetSnapshot`` backed by a lazy `PHFetchResult`.
///
/// `PHFetchResult` realises its `PHAsset` objects lazily and is documented as
/// safe for concurrent read access, so wrapping it gives the timeline O(1)
/// indexed access over the whole system library without materialising it —
/// hence the `@unchecked Sendable`, which is sound for this read-only use.
struct PhotoKitSnapshot: AssetSnapshot, @unchecked Sendable {
    let fetchResult: PHFetchResult<PHAsset>

    init(_ fetchResult: PHFetchResult<PHAsset>) {
        self.fetchResult = fetchResult
    }

    var count: Int {
        fetchResult.count
    }

    func asset(at index: Int) -> Asset {
        Asset(photoKitAsset: fetchResult.object(at: index))
    }
}
