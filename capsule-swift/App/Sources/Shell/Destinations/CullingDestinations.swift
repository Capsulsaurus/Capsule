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
extension RouteDestination {
    @ViewBuilder
    func cullingDestination(_ context: ViewerContext) -> some View {
        if let query = CullingPass.query(for: context) {
            CullingView(
                query: query,
                library: environment.mock.library,
                organize: environment.mock.organize,
                stacks: environment.mock.stacks,
                assetProvider: environment.assetProvider,
                thumbnails: environment.thumbnails
            )
        } else {
            unbuilt
        }
    }
}

// MARK: - CullingView

/// One asset at a time, a verdict per key, and the pass advancing on its own.
///
/// Every verb has three doors that reach the same ``CullingPass`` call: a key
/// (the Culling menu where the platform has one, a key handler on a phone), an
/// on-screen button, and a VoiceOver custom action on the asset — so no verb
/// depends on a pointer, a gesture or sight. The flag is shown as a symbol and a
/// word, never by colour alone, and every move announces the flag and position.
struct CullingView: View {
    let query: TimelineQuery
    let library: any LibraryPort
    let organize: any OrganizePort
    let stacks: any StackPort
    let assetProvider: any AssetProvider
    let thumbnails: any ThumbnailProvider

    @State private var pass = CullingPass()
    @State private var phase = Phase.loading
    @State private var image: PlatformImage?
    @State private var isConfirmingSweep = false
    @State private var isShowingShortcuts = false
    @State private var failure: Failure?
    /// Verdict writes, one at a time in the order the verdicts were given.
    @State private var writes = CullWriteQueue()
    @FocusState private var isFocused: Bool

    /// Rows per library page. The pass shows its first page as soon as it
    /// arrives and keeps reading behind it.
    private static let pageSize = 200
    /// The size the photo is decoded to: large enough for an iPad in landscape,
    /// small enough to stay a thumbnail-cache request.
    private static let photoPixels = CGSize(width: 2048, height: 2048)

    enum Phase: Equatable {
        case loading
        case ready
        case failed
    }

    enum Failure: Identifiable {
        case flag
        case sweep

        var id: Self { self }

        var messageKey: LocalizedStringKey {
            switch self {
            case .flag: "app.culling.error.flag"
            case .sweep: "app.culling.error.sweep"
            }
        }
    }

    var body: some View {
        content
            .navigationTitle(Text("app.culling.title"))
            .capsuleNavigationBarInline()
            .toolbar { toolbar }
            .focusable(handlesKeysInView)
            .focused($isFocused)
            .focusEffectDisabled()
            .onKeyPress(phases: .down) { press in handle(press) }
            .focusedSceneValue(\.culling, CullingCommandTarget(
                accepts: { action in CullVerb(action: action).map { pass.accepts($0) } ?? false },
                perform: { action in
                    if let verb = CullVerb(action: action) { perform(verb) }
                }
            ))
            .task { await load() }
            .task(id: pass.currentAsset?.id) { await loadImage() }
            .onAppear { isFocused = true }
            .confirmationDialog(
                sweepTitle,
                isPresented: $isConfirmingSweep,
                titleVisibility: .visible
            ) {
                Button("app.culling.sweep.confirm", role: .destructive) {
                    writes.submit { await sweep() }
                }
                Button("app.common.cancel", role: .cancel) {}
            } message: {
                Text("app.culling.sweep.message")
            }
            .alert(
                Text(failure?.messageKey ?? "app.culling.error.flag"),
                isPresented: Binding(get: { failure != nil }, set: { if !$0 { failure = nil } })
            ) {
                Button("app.common.ok") { failure = nil }
            }
            .sheet(isPresented: $isShowingShortcuts) {
                CullingShortcutsSheet()
            }
    }

    @ViewBuilder
    private var content: some View {
        switch phase {
        case .loading:
            ProgressView()
                .controlSize(.large)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .failed:
            ContentUnavailableView(
                "app.culling.title",
                systemImage: "exclamationmark.triangle",
                description: Text("app.culling.error.load")
            )
        case .ready where pass.isEmpty:
            ContentUnavailableView(
                "app.culling.empty.title",
                systemImage: "checkmark.circle",
                description: Text("app.culling.empty.body")
            )
        case .ready:
            VStack(spacing: 12) {
                photo
                CullFlagBadge(flag: pass.currentFlag)
                Text(positionText)
                    .font(.footnote.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                controls
            }
            .padding()
        }
    }

    // MARK: The asset

    private var photo: some View {
        ZStack {
            if let image {
                Image(platformImage: image)
                    .resizable()
                    .scaledToFit()
            } else {
                ProgressView()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        // One element for the whole step, so VoiceOver reads the flag and the
        // position together and every verb is a custom action on it.
        .accessibilityElement(children: .ignore)
        .accessibilityAddTraits(.isImage)
        .accessibilityLabel(Text(stepDescription))
        .accessibilityIdentifier("culling.asset")
        .accessibilityAdjustableAction { direction in
            switch direction {
            case .increment: perform(.next)
            case .decrement: perform(.previous)
            @unknown default: break
            }
        }
        .accessibilityAction(named: Text("app.culling.action.pick")) { perform(.pick) }
        .accessibilityAction(named: Text("app.culling.action.unflag")) { perform(.neutral) }
        .accessibilityAction(named: Text("app.culling.action.reject")) { perform(.reject) }
        .accessibilityAction(named: Text("app.culling.action.next")) { perform(.next) }
        .accessibilityAction(named: Text("app.culling.action.previous")) { perform(.previous) }
        .accessibilityActions {
            if pass.accepts(.expandStack) {
                Button("app.culling.action.expand_stack") { perform(.expandStack) }
            }
            if pass.accepts(.collapseStack) {
                Button("app.culling.action.collapse_stack") { perform(.collapseStack) }
            }
            if pass.accepts(.sweep) {
                Button("app.culling.action.sweep") { perform(.sweep) }
            }
        }
    }

    // MARK: Controls

    private var controls: some View {
        HStack(spacing: 8) {
            control(.previous, titleKey: "app.culling.action.previous", systemImage: "chevron.left")
            control(.reject, titleKey: "app.culling.action.reject", systemImage: "xmark.circle")
            control(.neutral, titleKey: "app.culling.action.unflag", systemImage: "flag.slash")
            control(.pick, titleKey: "app.culling.action.pick", systemImage: "flag")
            control(.next, titleKey: "app.culling.action.next", systemImage: "chevron.right")
            if pass.accepts(.collapseStack) {
                control(
                    .collapseStack,
                    titleKey: "app.culling.action.collapse_stack",
                    systemImage: "square.stack.3d.up"
                )
            } else {
                control(
                    .expandStack,
                    titleKey: "app.culling.action.expand_stack",
                    systemImage: "square.stack.3d.down.right"
                )
            }
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.bordered)
        .controlSize(.large)
    }

    private func control(_ verb: CullVerb, titleKey: LocalizedStringKey, systemImage: String) -> some View {
        Button {
            perform(verb)
        } label: {
            Label(titleKey, systemImage: systemImage)
                .frame(minWidth: 44, minHeight: 44)
        }
        .disabled(!pass.accepts(verb))
        .accessibilityIdentifier("culling.verb.\(String(describing: verb))")
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .primaryAction) {
            Button("app.culling.action.sweep", systemImage: "trash") { perform(.sweep) }
                .disabled(!pass.accepts(.sweep))
                .accessibilityIdentifier("culling.sweep")
        }
        ToolbarItem(placement: .secondaryAction) {
            Button("app.culling.action.shortcuts", systemImage: "keyboard") { isShowingShortcuts = true }
                .accessibilityIdentifier("culling.shortcuts")
        }
    }

    // MARK: Text

    /// The flag and the position, as one sentence: what VoiceOver reads on the
    /// asset and what is announced after every move.
    private var stepDescription: String {
        let flag = CullFlagBadge.text(for: pass.currentFlag)
        let position = positionText
        return String(localized: "app.culling.announcement", defaultValue: "\(flag), \(position)")
    }

    private var positionText: String {
        let (index, count) = pass.position
        let current = index.formatted()
        let total = count.formatted()
        return String(localized: "app.culling.position", defaultValue: "\(current) of \(total)")
    }

    private var sweepTitle: String {
        let count = pass.sweepCandidates.count
        return String(localized: "app.culling.sweep.title", defaultValue: "Move \(count) rejected photos to the Trash?")
    }

    // MARK: Verbs

    /// Run a verb from any of its three doors.
    private func perform(_ verb: CullVerb) {
        guard pass.accepts(verb) else { return }
        CapsuleLog.interface.debug("culling: \(String(describing: verb), privacy: .public)")
        switch verb {
        case .pick, .neutral, .reject:
            guard let flag = verb.flag else { return }
            guard let verdict = pass.verdict(flag) else { return }
            writes.submit { await write(verdict) }
        case .next: _ = pass.next()
        case .previous: _ = pass.previous()
        case .expandStack: _ = pass.expandStack()
        case .collapseStack: _ = pass.collapseStack()
        case .sweep:
            isConfirmingSweep = true
            return
        }
        announce(stepDescription)
    }

    /// Write a verdict. Runs on ``writes``, so it starts only once every
    /// earlier verdict's write has finished.
    private func write(_ verdict: CullVerdict) async {
        do {
            let ids = try await CullVerdictWrite.run(
                verdict,
                members: { [stacks] stackID in try await stacks.members(of: stackID).map(\.id) },
                setCull: { [organize] flag, ids in try await organize.setCull(flag, for: ids) }
            )
            pass.settle(verdict, written: ids)
            CapsuleLog.interface.info(
                "culling: flagged \(ids.count, privacy: .public) as \(verdict.flag.rawValue, privacy: .public)"
            )
        } catch {
            CapsuleLog.interface.error("culling: flag write failed: \(error.localizedDescription, privacy: .public)")
            pass.revert(verdict)
            failure = .flag
        }
    }

    /// Trash the rejected photos. Runs on ``writes`` behind every queued
    /// verdict, so the candidates are read once those writes have settled or
    /// reverted and the trash never reaches an asset whose flag write is still
    /// to come.
    private func sweep() async {
        do {
            guard let ids = try await CullSweep.run(
                candidates: { pass.sweepCandidates },
                moveToTrash: { [organize] ids in try await organize.moveToTrash(ids, retentionDays: nil) }
            ) else { return }
            pass.remove(Set(ids))
            let count = ids.count
            CapsuleLog.interface.info("culling: swept \(count, privacy: .public) rejects to the trash")
            announce(String(localized: "app.culling.sweep.done", defaultValue: "Moved \(count) photos to the Trash"))
        } catch {
            CapsuleLog.interface.error("culling: sweep failed: \(error.localizedDescription, privacy: .public)")
            failure = .sweep
        }
    }

    private func announce(_ text: String) {
        AccessibilityNotification.Announcement(text).post()
    }

    // MARK: Keys

    /// Whether this view reads the keyboard itself.
    ///
    /// Where a menu system exists — an iPad, a Mac — the Culling menu's items
    /// carry the keys and reach this view through ``CullingCommandTarget``, and
    /// the same keys handled here too would run every verb twice. A phone has no
    /// menu to deliver them, so there the view handles them against the same
    /// catalog.
    private var handlesKeysInView: Bool {
        !PlatformEnvironment.supportsMultipleWindows
    }

    private func handle(_ press: KeyPress) -> KeyPress.Result {
        guard handlesKeysInView else { return .ignored }
        let modifiers = press.modifiers.intersection([.command, .shift, .option, .control])
        let command = NavigationCommand.cullingPass.first { command in
            guard let shortcut = command.shortcut else { return false }
            return KeyEquivalent(shortcut.key) == press.key && EventModifiers(shortcut.modifiers) == modifiers
        }
        guard let command, let verb = CullVerb(action: command.action) else { return .ignored }
        perform(verb)
        return .handled
    }

    // MARK: Loading

    /// Read the sequence a page at a time, showing the pass from the first.
    private func load() async {
        var offset = 0
        do {
            while !Task.isCancelled {
                let page = try await library.assets(matching: query, offset: offset, limit: Self.pageSize)
                pass.append(page.items)
                phase = .ready
                offset += page.items.count
                if page.items.count < Self.pageSize { break }
            }
            CapsuleLog.interface.info("culling: pass loaded with \(pass.steps.count, privacy: .public) steps")
        } catch {
            CapsuleLog.interface.error("culling: loading the pass failed: \(error.localizedDescription, privacy: .public)")
            if pass.isEmpty { phase = .failed }
        }
        if !pass.isEmpty { announce(stepDescription) }
    }

    private func loadImage() async {
        image = nil
        guard let id = pass.currentAsset?.id,
              let asset = try? await assetProvider.asset(for: id)
        else { return }
        // A Preview render, not a tile: a RAW's embedded preview short of the
        // surface is replaced by the Preview generated from the original.
        let request = RenderRequest(intent: .preview, pixelSize: Self.photoPixels)
        await thumbnails.renderInPlace(asset, request: request, shown: nil, pixelSize: Self.photoPixels) {
            image = $0.image
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
}

// MARK: - CullVerdictWrite

/// What writing one verdict does to the library, over injected ports so the
/// order of writes is a unit test rather than a tap.
enum CullVerdictWrite {
    /// The members of a stack, as the stack port lists them.
    typealias Members = @Sendable (StackID) async throws -> [AssetID]
    /// Set a cull flag on assets in the library.
    typealias SetCull = @Sendable (CullFlag, [AssetID]) async throws -> Void

    /// Write `verdict` and return the assets it reached.
    ///
    /// A collapsed stack's targets come from the stack port, not the pass: the
    /// pass holds only the members its query and its pages have brought in,
    /// and flagging a stack writes every member.
    static func run(_ verdict: CullVerdict, members: Members, setCull: SetCull) async throws -> [AssetID] {
        let ids: [AssetID] = if let stackID = verdict.stackID {
            try await verdict.writeTargets(stackMembers: members(stackID))
        } else {
            verdict.targets
        }
        try await setCull(verdict.flag, ids)
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
