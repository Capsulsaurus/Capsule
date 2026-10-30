import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import os

/// The cull pass's log. Every write is an INFO line naming its count; reads
/// and refusals are DEBUG.
private let cullLog = Logger(subsystem: CapsuleLog.subsystem, category: "culling")

// MARK: - Workspace seam

/// The cull and trash verbs of `FfiWorkspace`, and nothing else of it.
///
/// Narrowed to a protocol so the port is tested over an in-memory double
/// rather than a passphrase-sealed workspace on disk; the generated object
/// conforms as declared.
protocol FFICullWorkspace: Sendable {
    func assetsByCull(flag: FfiCullFlag) throws -> [String]
    func stackMemberships() throws -> [FfiStackMember]
    func setCull(assetId: String, flag: FfiCullFlag) throws
    func flagStack(stackId: String, flag: FfiCullFlag) throws -> [String]
    func deleteAsset(assetId: String, retainDays: UInt32) throws -> FfiTrashEntry
}

extension FfiWorkspace: FFICullWorkspace {}

// MARK: - FFICullPort

/// The Rust-backed ``CullPort``: the workspace's cull verbs
/// (`assets_by_cull`, `stack_memberships`, `set_cull`, `flag_stack`) and its
/// trash (`delete_asset`).
///
/// The pass is every live asset of the workspace, read as the three flag sets
/// and ordered by id, which for UUIDv7 ids is the order they were created.
/// Each asset carries the stack place `stack_memberships` lists for it, its
/// cover being the stack's primary member; an asset it does not list arrives
/// as a lone one. The workspace has no verb that filters by album, media kind,
/// capture date or rating, so a pass over such a sequence is refused with
/// ``CullPortRefusal/unsupportedSequence`` rather than widened to the whole
/// library.
///
/// The sweep deletes exactly the assets it is given, one signed `delete` each,
/// rather than calling `reject_sweep`, which trashes every reject in the
/// workspace: a reject synced in after the pass was read is not one the user
/// confirmed. Workspace failures are rethrown as they came.
public struct FFICullPort: CullPort {
    /// The retention window a swept reject gets: the CLI's `cull sweep`
    /// default.
    public static let defaultRetainDays: UInt32 = 30

    private let workspace: any FFICullWorkspace
    private let retainDays: UInt32

    /// Cull the assets of `workspace`, sweeping rejects into its trash for
    /// `retainDays` days.
    public init(workspace: FfiWorkspace, retainDays: UInt32 = Self.defaultRetainDays) {
        self.init(source: workspace, retainDays: retainDays)
    }

    /// Cull through `source`. The seam tests drive.
    init(source: any FFICullWorkspace, retainDays: UInt32 = Self.defaultRetainDays) {
        workspace = source
        self.retainDays = retainDays
    }

    public func pass(over query: TimelineQuery, batchSize: Int) -> AsyncThrowingStream<[CullAsset], any Error> {
        let workspace = workspace
        let size = max(1, batchSize)
        return AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    let assets = try Self.read(query, from: workspace)
                    for start in stride(from: 0, to: assets.count, by: size) {
                        try Task.checkCancellation()
                        continuation.yield(Array(assets[start ..< min(start + size, assets.count)]))
                    }
                    continuation.finish()
                } catch {
                    cullLog.debug("pass read failed: \(error.localizedDescription, privacy: .public)")
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    public func setCull(_ flag: CullFlag, for assetIDs: [AssetID]) async throws {
        let ffiFlag = try FfiCullFlag(flag)
        let ids = try assetIDs.map(workspaceAssetID)
        for id in ids {
            try workspace.setCull(assetId: id, flag: ffiFlag)
        }
        cullLog.info("flagged \(ids.count, privacy: .public) as \(flag.rawValue, privacy: .public)")
    }

    public func flagStack(_ stackID: StackID, as flag: CullFlag) async throws -> [AssetID] {
        let ffiFlag = try FfiCullFlag(flag)
        let members = try workspace.flagStack(stackId: stackID.rawValue, flag: ffiFlag)
        cullLog.info(
            "flagged stack \(stackID.rawValue, privacy: .public): \(members.count, privacy: .public) as \(flag.rawValue, privacy: .public)"
        )
        return members.map { .managed(uuid: $0) }
    }

    public func moveToTrash(_ assetIDs: [AssetID]) async throws {
        let ids = try assetIDs.map(workspaceAssetID)
        for (moved, id) in ids.enumerated() {
            do {
                _ = try workspace.deleteAsset(assetId: id, retainDays: retainDays)
            } catch {
                cullLog.error(
                    "sweep stopped after \(moved, privacy: .public) of \(ids.count, privacy: .public): \(error.localizedDescription, privacy: .public)"
                )
                throw error
            }
        }
        cullLog.info("moved \(ids.count, privacy: .public) to the trash for \(retainDays, privacy: .public) days")
    }

    // MARK: Reading

    /// The pass over `query`: the flag sets it selects, each asset with its
    /// stack place, in id order.
    static func read(_ query: TimelineQuery, from workspace: any FFICullWorkspace) throws -> [CullAsset] {
        let flags = try passFlags(for: query)
        var places: [String: CullStackPlace] = [:]
        for member in try workspace.stackMemberships() {
            places[member.assetId] = CullStackPlace(
                stackID: StackID(member.stackId),
                isCover: member.isCover,
                memberIndex: member.memberIndex
            )
        }
        var rows: [(key: String, asset: CullAsset)] = []
        for flag in flags {
            for id in try workspace.assetsByCull(flag: FfiCullFlag(flag)) {
                rows.append((id, CullAsset(id: .managed(uuid: id), cull: flag, stack: places[id])))
            }
        }
        rows.sort { $0.key < $1.key }
        let stacked = rows.count { $0.asset.stack != nil }
        cullLog.debug("pass read: \(rows.count, privacy: .public) assets, \(stacked, privacy: .public) stacked")
        return rows.map(\.asset)
    }

    /// The flag sets a pass over `query` reads, or a refusal for a sequence
    /// the workspace has no verb to select.
    static func passFlags(for query: TimelineQuery) throws(CullPortRefusal) -> [CullFlag] {
        guard query.slice == .live,
              query.albumID == nil,
              query.mediaKind == nil,
              query.capturedAfter == nil,
              query.capturedBefore == nil,
              query.minimumRating == nil
        else {
            cullLog.debug("pass refused: the workspace cannot select this sequence")
            throw .unsupportedSequence
        }
        guard let flag = query.cull else { return CullFlag.knownCases }
        guard flag.isKnown else { throw .unsupportedSequence }
        return [flag]
    }
}

// MARK: - Mapping

/// The workspace's id for an asset: its catalog UUID. A system-Photos asset is
/// not in the workspace at all.
private func workspaceAssetID(_ assetID: AssetID) throws(CullPortRefusal) -> String {
    guard case let .managed(uuid) = assetID else { throw .notFound }
    return uuid
}

extension FfiCullFlag {
    /// The workspace's flag for a domain flag. A flag this build cannot name
    /// is never written.
    init(_ flag: CullFlag) throws(CullPortRefusal) {
        switch flag {
        case .pick: self = .pick
        case .neutral: self = .neutral
        case .reject: self = .reject
        case .unknown: throw .unknownFlag
        }
    }
}
