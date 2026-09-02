import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

/// The viewer's ``CaptionStore``, over the library and organize ports.
///
/// Reads through ``LibraryPort/asset(for:)`` rather than
/// ``LibraryPort/sidecar(for:)``: the projection already resolves the caption
/// register to its current value, and reading the signed sidecar to pull one
/// string out of it would make the viewer parse a wire record it has no other
/// use for.
public struct PortBackedCaptionStore: CaptionStore {
    private let library: any LibraryPort
    private let organize: any OrganizePort

    public init(library: any LibraryPort, organize: any OrganizePort) {
        self.library = library
        self.organize = organize
    }

    public func caption(for id: AssetID) async -> String? {
        // PhotoKit assets have no caption in the Capsule library; answering
        // `nil` is the truth rather than a failure.
        guard id.isManaged else { return nil }
        return try? await library.asset(for: id)?.caption
    }

    public func setCaption(_ caption: String?, for id: AssetID) async throws {
        // An all-whitespace caption is not a caption. Normalising here rather
        // than in the view means every entry point agrees, including a paste.
        let trimmed = caption?.trimmingCharacters(in: .whitespacesAndNewlines)
        try await organize.setCaption(trimmed?.isEmpty == true ? nil : trimmed, for: id)
    }
}
