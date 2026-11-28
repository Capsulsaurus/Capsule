import AssetKit
import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import CapsulePorts
import CapsuleUI
import ImagePipeline
import SwiftUI

/// The keyboard-driven cull pass (*Organization — Culling UI*).
///
/// Reached from the timeline's Cull button and from ⇧⌘R, over whatever
/// sequence was showing. Only a timeline or an album sequence resolves; the
/// rest keep the scaffold for the reason ``CullingPass/query(for:)`` gives.
/// The pass reads and writes through the lane's ``CullPort``: the workspace's
/// cull verbs where one is open, the mock world's ports otherwise.
extension RouteDestination {
    @ViewBuilder
    func cullingDestination(_ context: ViewerContext) -> some View {
        if let query = CullingPass.query(for: context) {
            CullingView(
                query: query,
                cull: environment.cull,
                assetProvider: environment.assetProvider,
                thumbnails: environment.thumbnails
            )
        } else {
            unbuilt
        }
    }
}

// MARK: - CullWriteQueue

/// Runs verdict writes one at a time, in the order they were submitted.
///
/// Each verdict used to start its own task, so a stack verdict, whose write
/// first asks the stack port for every member, could reach the library after a
/// later single-asset verdict on one of those members — leaving the library on
/// the older flag while the pass showed and stored the newer one. Every write
/// now waits for the one submitted before it, whether that one succeeded or
/// failed.
@MainActor
final class CullWriteQueue {
    /// The last write submitted; the next one waits for it.
    private var tail: Task<Void, Never>?

    /// Queue `write` behind every write already queued.
    func submit(_ write: @escaping @MainActor () async -> Void) {
        let previous = tail
        tail = Task { @MainActor in
            await previous?.value
            await write()
        }
    }

    /// Wait until every write submitted so far has finished.
    func drain() async {
        await tail?.value
    }
}

// MARK: - CullSweep

/// What sweeping the rejects does, over injected ports. The candidates are a
/// closure read when the sweep runs, not a value captured when it was
/// confirmed, so a sweep queued behind verdict writes sees their outcome.
enum CullSweep {
    /// What the sweep confirm button does: queue `sweep` behind every verdict
    /// write already queued, never start it on its own.
    @MainActor
    static func confirm(on writes: CullWriteQueue, sweep: @escaping @MainActor () async -> Void) {
        writes.submit(sweep)
    }

    /// Trash the current candidates and return them, or nil when there are none.
    @MainActor
    static func run(
        candidates: () -> [AssetID],
        moveToTrash: ([AssetID]) async throws -> Void
    ) async throws -> [AssetID]? {
        let ids = candidates()
        guard !ids.isEmpty else { return nil }
        try await moveToTrash(ids)
        return ids
    }

    /// The assets a failed sweep already moved to the trash, which leave the
    /// pass: the ones ``CullSweepIncomplete`` names, or none for any other
    /// failure.
    static func moved(before failure: any Error) -> [AssetID] {
        (failure as? CullSweepIncomplete)?.moved ?? []
    }
}

// MARK: - CullVerdictWrite

/// What writing one verdict does to the library, over injected ports so the
/// order of writes is a unit test rather than a tap.
enum CullVerdictWrite {
    /// Flag every member of a stack in the library, answering the members it
    /// reached.
    typealias FlagStack = @Sendable (StackID, CullFlag) async throws -> [AssetID]
    /// Set a cull flag on assets in the library.
    typealias SetCull = @Sendable (CullFlag, [AssetID]) async throws -> Void

    /// Write `verdict` and return the assets it reached.
    ///
    /// A collapsed stack is flagged as a stack, not through the pass: the pass
    /// holds only the members its query and its batches have brought in, and
    /// flagging a stack writes every member the library holds. A target the
    /// stack no longer lists is then flagged on its own, so every asset the
    /// pass showed the verdict on carries it.
    static func run(_ verdict: CullVerdict, flagStack: FlagStack, setCull: SetCull) async throws -> [AssetID] {
        guard let stackID = verdict.stackID else {
            try await setCull(verdict.flag, verdict.targets)
            return verdict.targets
        }
        let flagged = try await flagStack(stackID, verdict.flag)
        let ids = verdict.writeTargets(stackMembers: flagged)
        let rest = Array(ids.dropFirst(flagged.count))
        if !rest.isEmpty {
            try await setCull(verdict.flag, rest)
        }
        return ids
    }
}

// MARK: - CullFlagBadge

/// The current flag as a symbol and a word — never a colour alone.
struct CullFlagBadge: View {
    let flag: CullStepFlag?

    var body: some View {
        Label(Self.text(for: flag), systemImage: Self.systemImage(for: flag))
            .font(.headline)
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(.thinMaterial, in: Capsule())
            .accessibilityHidden(true)
    }

    static func text(for flag: CullStepFlag?) -> String {
        switch flag {
        case .asset(.pick): String(localized: "app.culling.flag.pick")
        case .asset(.reject): String(localized: "app.culling.flag.reject")
        case .asset, .none: String(localized: "app.culling.flag.unflagged")
        case let .stack(state, memberCount):
            stackText(state, memberCount: memberCount)
        }
    }

    private static func stackText(_ state: GroupCullState, memberCount: Int) -> String {
        let summary = switch state {
        case .anyPick: String(localized: "app.culling.stack.any_pick")
        case .allRejected: String(localized: "app.culling.stack.all_rejected")
        case .mixed, .empty: String(localized: "app.culling.stack.mixed")
        }
        let members = String(localized: "app.culling.stack.members", defaultValue: "\(memberCount) photos")
        return String(localized: "app.culling.stack.summary", defaultValue: "\(summary), \(members)")
    }

    private static func systemImage(for flag: CullStepFlag?) -> String {
        switch flag {
        case .asset(.pick): "flag.fill"
        case .asset(.reject): "xmark.circle.fill"
        case .asset, .none: "circle.dashed"
        case .stack(.anyPick, _): "square.stack.3d.up.fill"
        case .stack(.allRejected, _): "square.stack.3d.up.slash"
        case .stack: "square.stack.3d.up"
        }
    }
}

// MARK: - CullingShortcutsSheet

/// Every key the pass answers, read from the same catalog the menu is built
/// from — the in-app shortcut list a phone needs, and on every other platform a
/// second way to find them.
struct CullingShortcutsSheet: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List(NavigationCommand.cullingPass) { command in
                LabeledContent {
                    Text(command.shortcut?.displayText ?? "")
                        .font(.body.monospaced())
                } label: {
                    Label(LocalizedStringKey(command.titleKey), systemImage: command.systemImage)
                }
            }
            .navigationTitle(Text("app.culling.action.shortcuts"))
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("app.common.done") { dismiss() }
                }
            }
        }
    }
}
