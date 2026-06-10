import CapsuleFoundation
import Foundation

/// A persisted set of hidden asset ids, overlaid on the library.
///
/// Hidden assets are filtered out of the Library timeline and shown only in the
/// Face-ID-gated Hidden screen. A Swift overlay keeps this symmetric across
/// PhotoKit and managed sources without a backing-store change. Persisted to
/// `UserDefaults` (the id set is small and `Codable`).
public actor HiddenStore {
    private var ids: Set<AssetID>
    private var observers: [UUID: AsyncStream<Void>.Continuation] = [:]
    private static let defaultsKey = "capsule.hidden.assetIDs"

    public init() {
        ids = Self.load()
    }

    public func hiddenIDs() -> Set<AssetID> { ids }

    /// Hide or reveal the given assets, then notify observers.
    public func setHidden(_ hidden: Bool, for assetIDs: [AssetID]) {
        for id in assetIDs {
            if hidden { ids.insert(id) } else { ids.remove(id) }
        }
        persist()
        for continuation in observers.values { continuation.yield(()) }
    }

    /// A stream that fires whenever the hidden set changes.
    public nonisolated func changes() -> AsyncStream<Void> {
        AsyncStream { continuation in
            let token = UUID()
            Task { await self.register(continuation, token: token) }
            continuation.onTermination = { _ in
                Task { await self.unregister(token) }
            }
        }
    }

    private func register(_ continuation: AsyncStream<Void>.Continuation, token: UUID) {
        observers[token] = continuation
    }

    private func unregister(_ token: UUID) {
        observers[token] = nil
    }

    private func persist() {
        guard let data = try? JSONEncoder().encode(ids) else { return }
        UserDefaults.standard.set(data, forKey: Self.defaultsKey)
    }

    private static func load() -> Set<AssetID> {
        guard let data = UserDefaults.standard.data(forKey: defaultsKey),
              let ids = try? JSONDecoder().decode(Set<AssetID>.self, from: data)
        else {
            return []
        }
        return ids
    }
}
