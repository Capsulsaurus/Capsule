import CapsuleDomain
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - ImportPlanConfirmView

/// **Step 3 — the decision point.**
///
/// Nothing has been written yet, and after the button at the bottom something
/// will have been. So this screen has to carry, on one scroll: how much, where
/// to and *why there*, what will be skipped and why, what needs an answer, and
/// whether the disk can take it.
///
/// The confirm action lives in a pinned bar rather than at the end of the list,
/// because a plan with fifteen hundred rows has no reachable end.
public struct ImportPlanConfirmView: View {
    @State private var model: ImportPlanConfirmModel
    private let onConfirm: (@MainActor (ImportPlan) -> Void)?
    private let onFreeUpSpace: (@MainActor () -> Void)?

    public init(
        model: ImportPlanConfirmModel,
        onConfirm: (@MainActor (ImportPlan) -> Void)? = nil,
        onFreeUpSpace: (@MainActor () -> Void)? = nil
    ) {
        _model = State(initialValue: model)
        self.onConfirm = onConfirm
        self.onFreeUpSpace = onFreeUpSpace
    }

    public init(
        scan: ImportScan,
        environment: ImportEnvironment,
        onConfirm: (@MainActor (ImportPlan) -> Void)? = nil,
        onFreeUpSpace: (@MainActor () -> Void)? = nil
    ) {
        self.init(
            model: ImportPlanConfirmModel(scan: scan, environment: environment),
            onConfirm: onConfirm,
            onFreeUpSpace: onFreeUpSpace
        )
    }

    public var body: some View {
        ImportScreen(
            titleKey: "ios.import.plan.title",
            phase: model.phase,
            emptyTitleKey: "ios.import.plan.empty.title",
            emptyDescriptionKey: "ios.import.plan.empty.description",
            emptySymbol: "tray",
            retry: { await model.load() },
            content: { planBody }
        )
        .task { await model.load() }
    }

    private var planBody: some View {
        List {
            summarySection
            tilesSection
            expandedSection
            destinationSection
            spaceSection
            modeSection
        }
        .safeAreaInset(edge: .bottom) { confirmBar }
    }

    // MARK: Sections

    private var summarySection: some View {
        Section {
            ImportSummaryCard(itemCount: model.totalCount, byteCount: model.totalBytes)
        }
    }

    private var tilesSection: some View {
        Section {
            HStack(spacing: CapsuleTheme.Spacing.small) {
                ForEach(ImportPlanCategory.allCases) { category in
                    tile(category)
                }
            }
            .listRowInsets(EdgeInsets(top: 8, leading: 12, bottom: 8, trailing: 12))
        }
    }

    private func tile(_ category: ImportPlanCategory) -> some View {
        ImportStatTile(
            category: category,
            count: model.count(for: category),
            isExpanded: model.expanded == category,
            tone: tone(for: category)
        ) {
            model.expanded = model.expanded == category ? nil : category
        }
    }

    /// Conflicts are amber only while there are any; an empty bucket is not a
    /// warning, and colouring it would train users to ignore the colour.
    private func tone(for category: ImportPlanCategory) -> ImportTone {
        switch category {
        case .add: .positive
        case .skip: .neutral
        case .conflicts: model.count(for: .conflicts) > 0 ? .caution : .neutral
        }
    }

    @ViewBuilder
    private var expandedSection: some View {
        if let category = model.expanded {
            Section {
                expandedRows(category)
            } header: {
                Text(LocalizedStringKey(category.listHeaderKey))
            }
        }
    }

    @ViewBuilder
    private func expandedRows(_ category: ImportPlanCategory) -> some View {
        if category == .conflicts {
            ForEach(model.conflicts) { conflict in
                ImportConflictRow(conflict: conflict) { resolution in
                    model.resolve(conflict.candidateID, as: resolution)
                }
            }
        } else {
            ForEach(model.decisions(for: category)) { decision in
                ImportDecisionRow(decision: decision)
            }
        }
    }

    @ViewBuilder
    private var destinationSection: some View {
        if let rule = model.destinationRule {
            Section {
                ImportDestinationRow(albumName: model.destinationName, rule: rule)
            } header: {
                Text("ios.import.plan.destination.header")
            } footer: {
                Text("ios.import.plan.destination.footer")
            }
        }
    }

    private var spaceSection: some View {
        Section {
            ImportSpaceMeter(
                outlook: model.outlook,
                itemCount: model.totalCount,
                onFreeUpSpace: onFreeUpSpace,
                onEnableStreaming: { Task { await model.setStreaming(true) } }
            )
        } header: {
            Text("ios.import.plan.space.header")
        } footer: {
            Text("ios.import.plan.space.footer")
        }
    }

    private var modeSection: some View {
        Section {
            ImportValueRow(
                labelKey: "ios.import.plan.mode.header",
                value: String(localized: String.LocalizationValue(modeKey))
            )
            if model.releasesSource {
                ImportStatusLabel(titleKey: "ios.import.plan.move.warning", tone: .caution)
                    .font(.footnote)
            }
            if model.isStreaming {
                ImportNote(textKey: "ios.import.plan.streaming.enabled")
            }
        }
    }

    private var modeKey: String {
        model.releasesSource ? ImportMode.move.titleKey : ImportMode.copy.titleKey
    }

    // MARK: Confirm bar

    /// Pinned rather than trailing, and it states the number it is about to act
    /// on — a bare "Import" at the bottom of a fifteen-hundred-row list is a
    /// button nobody can check before pressing.
    private var confirmBar: some View {
        VStack(spacing: CapsuleTheme.Spacing.xSmall) {
            Button(action: confirm) {
                Text(verbatim: confirmTitle)
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(!model.canConfirm)
            blockedNote
        }
        .padding(CapsuleTheme.Spacing.large)
        .background(.bar)
    }

    @ViewBuilder
    private var blockedNote: some View {
        if !model.canConfirm, model.phase.isReady {
            ImportNote(textKey: "ios.import.plan.confirm.blocked")
        }
    }

    private var confirmTitle: String {
        String(
            format: String(localized: "ios.import.plan.confirm"),
            ImportFormat.count(model.count(for: .add))
        )
    }

    private func confirm() {
        guard let plan = model.confirm() else { return }
        onConfirm?(plan)
    }
}

// MARK: - Previews

#Preview("Plan — healthy") {
    NavigationStack {
        ImportPlanConfirmView(scan: PreviewScans.healthy, environment: .preview(.healthy), onFreeUpSpace: {})
    }
}

#Preview("Plan — no room") {
    NavigationStack {
        ImportPlanConfirmView(scan: PreviewScans.enormous, environment: .preview(.healthy), onFreeUpSpace: {})
    }
}

#Preview("Plan — offline") {
    NavigationStack {
        ImportPlanConfirmView(scan: PreviewScans.healthy, environment: .preview(.offline))
    }
}

#Preview("Plan — nothing to import") {
    NavigationStack {
        ImportPlanConfirmView(scan: PreviewScans.empty, environment: .preview(.emptyLibrary))
    }
}

// MARK: - PreviewScans

/// Fixed scans for the previews.
///
/// ``enormous`` exists to reach the red space verdict, which no healthy library
/// produces: a plan bigger than the volume is not a state a preview can stumble
/// into, and it is the state most worth looking at.
enum PreviewScans {
    static let healthy = ImportScan(
        scope: PreviewScopes.cameraRoll,
        candidates: (0 ..< 48).map(candidate),
        unreadableLocators: ["photokit://camera-roll/locked/IMG_0042.HEIC"]
    )

    static let enormous = ImportScan(
        scope: PreviewScopes.takeout,
        candidates: (0 ..< 90).map { enormousCandidate($0) }
    )

    static let empty = ImportScan(scope: PreviewScopes.cameraRoll, candidates: [])

    private static func candidate(_ ordinal: Int) -> ImportCandidate {
        ImportCandidate(
            id: "preview-\(ordinal)",
            locator: "photokit://camera-roll/IMG_\(4000 + ordinal).HEIC",
            contentType: .heic,
            byteSize: UInt64(3400000 + ordinal * 11000)
        )
    }

    private static func enormousCandidate(_ ordinal: Int) -> ImportCandidate {
        ImportCandidate(
            id: "preview-big-\(ordinal)",
            locator: "file:///Downloads/takeout/VID_\(ordinal).MOV",
            contentType: .quicktime,
            byteSize: 4000000000
        )
    }
}
