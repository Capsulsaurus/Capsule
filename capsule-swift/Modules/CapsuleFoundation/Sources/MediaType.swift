import Foundation

/// The kind of media an asset represents.
///
/// Raw values match the catalog's `asset_type` column and the CBOR sidecar's
/// `asset_type` field, so they round-trip across the Rust FFI boundary unchanged.
public enum MediaType: String, Sendable, Codable, CaseIterable {
    case photo
    case video
    case motionPhoto = "motion_photo"
}
