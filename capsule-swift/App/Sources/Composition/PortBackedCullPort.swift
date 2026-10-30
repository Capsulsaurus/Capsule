import CapsuleDomain
import CapsuleFoundation
import CapsulePorts

/// The ``CullPort`` over the library, organize and stack ports: the mock
/// lane's pass, and the one previews and tests drive.
///
/// The pass pages ``LibraryPort/assets(matching:offset:limit:)`` and yields
/// each page as it arrives; flagging a stack lists its members through the
/// stack port and flags them in one write; the sweep moves the given assets to
/// the trash with the library's default retention window.
struct PortBackedCullPort: CullPort {
    let library: any LibraryPort
    let organize: any OrganizePort
    let stacks: any StackPort

    func pass(over query: TimelineQuery, batchSize: Int) -> AsyncThrowingStream<[CullAsset], any Error> {
        let library = library
        let size = max(1, batchSize)
        return AsyncThrowingStream { continuation in
            let task = Task {
                var offset = 0
                do {
                    while !Task.isCancelled {
                        let page = try await library.assets(matching: query, offset: offset, limit: size)
                        if !page.items.isEmpty { continuation.yield(page.items.map(CullAsset.init)) }
                        offset += page.items.count
                        if page.items.count < size { break }
                    }
                    continuation.finish()
                } catch {
                    CapsuleLog.interface.debug(
                        "culling: library page at \(offset, privacy: .public) failed: \(error.localizedDescription, privacy: .public)"
                    )
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    func setCull(_ flag: CullFlag, for assetIDs: [AssetID]) async throws {
        try await organize.setCull(flag, for: assetIDs)
    }

    func flagStack(_ stackID: StackID, as flag: CullFlag) async throws -> [AssetID] {
        let members = try await stacks.members(of: stackID).map(\.id)
        if !members.isEmpty { try await organize.setCull(flag, for: members) }
        return members
    }

    func moveToTrash(_ assetIDs: [AssetID]) async throws {
        try await organize.moveToTrash(assetIDs, retentionDays: nil)
    }
}
