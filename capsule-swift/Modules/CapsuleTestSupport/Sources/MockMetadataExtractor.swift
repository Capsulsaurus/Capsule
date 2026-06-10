import Foundation
import ManagedStore

/// A ``MediaMetadataExtracting`` that returns fixed metadata, filling in the
/// file size from the actual data — for deterministic import tests.
public struct MockMetadataExtractor: MediaMetadataExtracting {
    private let template: MediaMetadata

    public init(template: MediaMetadata = MediaMetadata(
        assetType: "photo",
        pixelWidth: 4032,
        pixelHeight: 3024,
        captureTimestamp: Fixtures.referenceTimestamp
    )) {
        self.template = template
    }

    public func extractMetadata(from data: Data, filename _: String) -> MediaMetadata {
        var metadata = template
        metadata.fileSize = Int64(data.count)
        return metadata
    }
}
