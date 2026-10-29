import CapsuleFoundation
import Foundation

// MARK: - CompositeCaptionStore

/// One ``CaptionStore`` over several sources, routing each asset to its owner.
///
/// The viewer opens imported and library assets alike, and a caption read or
/// write has to reach the store that holds the asset. Sending an import's
/// caption to the mock library would record it against an id the library does
/// not know: the write would succeed and the next read would come back empty.
///
/// Each **secondary** is asked first, because its ``AssetOwning/owns(_:)`` is
/// exact; the **primary** owns everything no secondary claims.
public struct CompositeCaptionStore: CaptionStore {
    private let primary: any CaptionStore
    private let secondaries: [any CaptionStore & AssetOwning]

    public init(primary: any CaptionStore, secondaries: [any CaptionStore & AssetOwning]) {
        self.primary = primary
        self.secondaries = secondaries
    }

    public func caption(for id: AssetID) async -> String? {
        await owner(of: id).caption(for: id)
    }

    public func setCaption(_ caption: String?, for id: AssetID) async throws {
        try await owner(of: id).setCaption(caption, for: id)
    }

    private func owner(of id: AssetID) async -> any CaptionStore {
        for secondary in secondaries where await secondary.owns(id) {
            return secondary
        }
        return primary
    }
}

// MARK: - ManagedCaptionStore

/// The managed store's answer to captions: it has none yet.
///
/// The managed catalog row has no caption register, so an import has no caption
/// to read and nowhere to write one. Reads answer `nil`, which is true. Writes
/// throw ``CaptionStoreError/unsupported`` instead of succeeding into nothing.
/// What this type is for is ownership: it claims imported assets, so
/// ``CompositeCaptionStore`` never sends their captions to a store that does not
/// hold them.
public struct ManagedCaptionStore: CaptionStore, AssetOwning {
    private let provider: ManagedProvider

    public init(provider: ManagedProvider) {
        self.provider = provider
    }

    public func owns(_ id: AssetID) async -> Bool {
        await provider.owns(id)
    }

    public func caption(for _: AssetID) async -> String? {
        nil
    }

    public func setCaption(_: String?, for id: AssetID) async throws {
        CapsuleLog.assetKit.notice(
            "caption refused for managed asset \(String(describing: id), privacy: .private): no caption register"
        )
        throw CaptionStoreError.unsupported
    }
}
