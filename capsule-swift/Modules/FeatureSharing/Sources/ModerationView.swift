import CapsuleDomain
import CapsuleMock
import CapsuleUI
import SwiftUI

// MARK: - ModerationView

/// Blocks, reports, untrusted origins, the audit log, and appeals
/// (*Moderation*).
public struct ModerationView: View {
    @State private var model: ModerationViewModel

    public init(model: ModerationViewModel) {
        _model = State(wrappedValue: model)
    }

    public var body: some View {
        SharingStateView(
            phase: model.phase,
            empty: .init(
                title: "ios.moderation.empty.title",
                message: "ios.moderation.empty.description",
                symbol: "hand.raised"
            ),
            retry: { Task { await model.load() } },
            content: {
            form
        }
        .navigationTitle("ios.moderation.title")
        .task { await model.load() }
        .safeAreaInset(edge: .top) { OfflineNotice(connection: model.connection) }
        .confirmationDialog(
            "ios.moderation.unblock.confirm_title",
            isPresented: Binding(
                get: { model.pendingUnblock != nil },
                set: { if !$0 { model.pendingUnblock = nil } }
            ),
            titleVisibility: .visible
        ) {
            Button("ios.moderation.unblock.confirm") {
                guard let entry = model.pendingUnblock else { return }
                Task { await model.unblock(entry) }
            }
            Button("ios.common.cancel", role: .cancel) { model.pendingUnblock = nil }
        } message: {
            Text("ios.moderation.unblock.confirm_message")
        }
    }

    private var form: some View {
        Form {
            blocksSection
            untrustedSection
            reportsSection
            ModerationAuditSection(model: model)
            scopeSection
        }
        .formStyle(.grouped)
        .frame(maxWidth: 720)
        .frame(maxWidth: .infinity)
    }

    // MARK: Sections

    @ViewBuilder
    private var blocksSection: some View {
        Section {
            if model.blocks.isEmpty {
                Text("ios.moderation.blocks.none")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(model.blocks) { entry in
                    BlockRowView(entry: entry) { model.pendingUnblock = entry }
                }
            }
        } header: {
            Text("ios.moderation.section.blocks")
        } footer: {
            // The honest limit: a block stops future access, it does not claw
            // back the epoch keys the blocked party already holds.
            Text("ios.moderation.section.blocks_footer")
        }
    }

    /// Untrusted origins: default-deny, with the risk stated beside the switch.
    @ViewBuilder
    private var untrustedSection: some View {
        if !model.untrustedOrigins.isEmpty {
            Section {
                ForEach(model.untrustedOrigins) { origin in
                    UntrustedOriginRow(origin: origin) { granted in
                        Task { await model.setConsent(granted, for: origin.origin) }
                    }
                }
            } header: {
                Text("ios.moderation.section.untrusted")
            } footer: {
                Text("ios.moderation.section.untrusted_footer")
            }
        }
    }

    @ViewBuilder
    private var reportsSection: some View {
        Section {
            if model.reports.isEmpty {
                Text("ios.moderation.reports.none")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(model.reports) { ReportRowView(report: $0) }
            }
        } header: {
            Text("ios.moderation.section.reports")
        } footer: {
            Text("ios.moderation.section.reports_footer")
        }
    }

    private var scopeSection: some View {
        Section("ios.moderation.section.how") {
            ScopeNote(message: "ios.moderation.note.no_scanning")
            ScopeNote(message: "ios.moderation.note.no_silent_ops")
            ScopeNote(message: "ios.moderation.note.appeal_master_key")
        }
    }
}

// MARK: - BlockRowView

struct BlockRowView: View {
    let entry: BlockEntry
    let unblock: () -> Void

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                Label {
                    Text(verbatim: subjectText)
                } icon: {
                    Image(systemName: symbol)
                }
                LabeledContent {
                    Text(entry.blockedAt.date, format: .dateTime.year().month().day())
                } label: {
                    Text("ios.moderation.block.since")
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer(minLength: CapsuleTheme.Spacing.small)
            Button("ios.moderation.unblock", action: unblock)
                .buttonStyle(.borderless)
        }
        .accessibilityElement(children: .combine)
    }

    private var subjectText: String {
        switch entry.subject {
        case let .user(handle): handle
        case let .peer(peerID): peerID.rawValue
        }
    }

    private var symbol: String {
        if case .peer = entry.subject { return "server.rack" }
        return "person.crop.circle.badge.xmark"
    }
}

// MARK: - UntrustedOriginRow

/// One origin the client will not load from without consent.
///
/// The withheld count is shown so it is clear the entries still exist — they are
/// being skipped, not deleted — and the toggle is labelled as an acceptance of
/// risk rather than as a display preference.
struct UntrustedOriginRow: View {
    let origin: UntrustedOrigin
    /// `@MainActor @Sendable` for the same reason as the export strip's setter:
    /// a `Binding` setter is `@Sendable`, and the isolation is what makes
    /// touching the view model from it correct rather than merely tolerated.
    let setConsent: @MainActor @Sendable (Bool) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(verbatim: origin.origin)
                .font(.subheadline.monospaced())
            LabeledContent {
                Text(origin.withheldAssetCount, format: .number)
            } label: {
                Text("ios.moderation.untrusted.withheld")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            Toggle("ios.moderation.untrusted.consent", isOn: Binding(
                get: { origin.isConsented },
                set: setConsent
            ))
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
    }
}

// MARK: - ReportRowView

struct ReportRowView: View {
    let report: ModerationReport

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(reasonTitle)
            LabeledContent {
                Text(report.submittedAt.date, format: .dateTime.year().month().day())
            } label: {
                Text("ios.moderation.report.submitted")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            StatusBadge(title: "ios.moderation.report.state.submitted", symbol: "paperplane", tint: .secondary)
        }
        .accessibilityElement(children: .combine)
    }

    private var reasonTitle: LocalizedStringKey {
        switch report.reason {
        case .abuse: "ios.moderation.reason.abuse"
        case .spam: "ios.moderation.reason.spam"
        case .impersonation: "ios.moderation.reason.impersonation"
        case .illegalContent: "ios.moderation.reason.illegal"
        case .other, .unknown: "ios.moderation.reason.other"
        }
    }
}

// MARK: - Previews

#Preview("Moderation — light") {
    let environment = MockEnvironment(scenario: .degradedFederation)
    let records = InMemoryModerationRecords.populated(now: MockClock.reference.now)
    return NavigationStack {
        ModerationView(model: ModerationViewModel(
            moderation: environment.moderation,
            records: records,
            originPolicy: records,
            connectivity: SharingConnectivity(sync: environment.sync)
        ))
    }
    .preferredColorScheme(.light)
}

#Preview("Moderation — dark") {
    let environment = MockEnvironment(scenario: .degradedFederation)
    let records = InMemoryModerationRecords.populated(now: MockClock.reference.now)
    return NavigationStack {
        ModerationView(model: ModerationViewModel(
            moderation: environment.moderation,
            records: records,
            originPolicy: records,
            connectivity: SharingConnectivity(sync: environment.sync)
        ))
    }
    .preferredColorScheme(.dark)
}
