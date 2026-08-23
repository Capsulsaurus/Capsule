import CapsuleDomain
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - QuarantineDetailView

/// One held item: what happened, what bytes are preserved, and exactly three
/// explicit actions.
///
/// **Inspect / Repair / Discard, with no default.** No action is prominent, no
/// action is the keyboard default, and none is pre-selected — automatic
/// resolution of a quarantine is the same thing as silently applying or
/// silently dropping, which is the behaviour this whole surface exists to
/// prevent. Discard confirms, because it destroys the preserved bytes.
///
/// Route entry point. Ports required: ``QuarantinePort``, ``SyncPort``.
public struct QuarantineDetailView: View {
    @State private var model: QuarantineDetailModel
    @State private var isConfirmingDiscard = false
    private let onResolved: (@MainActor () -> Void)?

    public init(
        item: QuarantineItem,
        quarantine: any QuarantinePort,
        sync: any SyncPort,
        onResolved: (@MainActor () -> Void)? = nil
    ) {
        _model = State(wrappedValue: QuarantineDetailModel(
            item: item,
            quarantine: quarantine,
            sync: sync
        ))
        self.onResolved = onResolved
    }

    public var body: some View {
        content
            .navigationTitle("app.quarantine.detail.title")
            .task { await model.load() }
            .onChange(of: model.isResolved) { _, isResolved in
                if isResolved { onResolved?() }
            }
            .confirmationDialog(
                "app.quarantine.discard.confirm.title",
                isPresented: $isConfirmingDiscard,
                titleVisibility: .visible
            ) {
                Button("app.quarantine.discard.confirm.action", role: .destructive) {
                    Task { await model.discard() }
                }
                Button("app.common.cancel", role: .cancel) {}
            } message: {
                Text("app.quarantine.discard.confirm.message")
            }
    }

    @ViewBuilder
    private var content: some View {
        if case let .failed(error) = model.phase {
            PhasePlaceholderView(
                phase: .failed(error),
                emptyTitle: "app.quarantine.detail.title",
                emptyDescription: "app.quarantine.detail.title",
                emptySymbol: "exclamationmark.triangle",
                retry: { await model.load() }
            )
        } else {
            List {
                explanationSection
                preservationSection
                actionsSection
                if let bytes = model.inspectedBytes { inspectionSection(bytes) }
            }
            .listStyle(.inset)
        }
    }

    // MARK: Sections

    /// What happened, in plain language, with no blame and no jargon.
    private var explanationSection: some View {
        Section {
            Label(LocalizedStringKey(model.item.surface.badge.titleKey), systemImage: model.item.surface.badge.systemImage)
                .font(.headline)
                .foregroundStyle(model.item.surface.badge.tint)
            Text(model.item.surface.explanationKey)
                .font(.body)
            Text(model.item.reason.explanationKey)
                .font(.footnote)
                .foregroundStyle(.secondary)
            LabeledContent("app.quarantine.detail.reason_code") {
                Text(verbatim: model.item.reason.code)
                    .font(.caption.monospaced())
            }
            LabeledContent("app.quarantine.detail.detected_at") {
                Text(verbatim: TransferFormat.captureDate(model.item.detectedAt))
            }
        } header: {
            Text("app.quarantine.detail.what_happened")
        }
    }

    /// What is preserved, and where.
    private var preservationSection: some View {
        Section {
            Label(model.item.surface.storage.titleKey, systemImage: "archivebox")
            Text(model.item.surface.storage.preservationKey)
                .font(.footnote)
                .foregroundStyle(.secondary)
            if let bytes = model.preservedBytes {
                LabeledContent("app.quarantine.detail.preserved_bytes") {
                    Text(verbatim: TransferFormat.bytes(bytes))
                }
            }
        } header: {
            Text("app.quarantine.detail.what_is_kept")
        } footer: {
            Text("app.quarantine.detail.what_is_kept.footer")
        }
    }

    /// The three actions. Uniform styling on purpose: nothing here is the
    /// recommended choice.
    private var actionsSection: some View {
        Section {
            ForEach(model.options) { option in
                QuarantineActionRow(option: option, isBusy: model.isBusy) {
                    perform(option)
                }
            }
        } header: {
            Text("app.quarantine.detail.actions")
        } footer: {
            Text("app.quarantine.detail.actions.footer")
        }
    }

    private func inspectionSection(_ bytes: Data) -> some View {
        Section {
            Text(verbatim: TransferFormat.fingerprint(bytes, keeping: 64))
                .font(.caption.monospaced())
                .textSelection(.enabled)
            LabeledContent("app.quarantine.inspect.sample_size") {
                Text(verbatim: TransferFormat.bytes(UInt64(bytes.count)))
            }
        } header: {
            Text("app.quarantine.inspect.title")
        } footer: {
            Text("app.quarantine.inspect.footer")
        }
    }

    private func perform(_ option: QuarantineActionOption) {
        switch option.resolution {
        case .inspect: Task { await model.inspect() }
        case .repair: Task { await model.repair() }
        case .discard: isConfirmingDiscard = true
        }
    }
}

// MARK: - QuarantineActionRow

/// One of the three resolutions.
///
/// Every row uses the same `.bordered` treatment: no `.borderedProminent`, no
/// `.keyboardShortcut(.defaultAction)`, no pre-selection. The user chooses, or
/// nothing happens.
struct QuarantineActionRow: View {
    let option: QuarantineActionOption
    let isBusy: Bool
    let perform: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Button(role: option.isDestructive ? .destructive : nil, action: perform) {
                Label(option.resolution.titleKey, systemImage: option.resolution.systemImage)
            }
            .buttonStyle(.bordered)
            .disabled(!option.isEnabled || isBusy)
            Text(option.resolution.explanationKey)
                .font(.caption)
                .foregroundStyle(.secondary)
            if let reasonKey = option.unavailableReasonKey {
                Label(LocalizedStringKey(reasonKey), systemImage: "info.circle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
    }
}

// MARK: - Previews

#Preview("Malformed sidecar — bytes preserved") {
    let environment = MockEnvironment(scenario: .quarantine)
    let item = QuarantineItem(
        id: QuarantineID("preview-malformed"),
        surface: .malformedSidecar,
        reason: .malformedEncoding,
        detectedAt: environment.configuration.clock.offset(days: -2),
        preservedBytes: 4096,
        resolutions: [.inspect, .repair, .discard]
    )
    return NavigationStack {
        QuarantineDetailView(item: item, quarantine: environment.quarantine, sync: environment.sync)
    }
}

#Preview("Federation soft-fail — recorded only") {
    let environment = MockEnvironment(scenario: .quarantine)
    let item = QuarantineItem(
        id: QuarantineID("preview-federation"),
        surface: .federationSoftFail,
        reason: .verifyRejected(.forgedChain),
        detectedAt: environment.configuration.clock.offset(days: -9),
        resolutions: [.inspect, .discard]
    )
    return NavigationStack {
        QuarantineDetailView(item: item, quarantine: environment.quarantine, sync: environment.sync)
    }
    .preferredColorScheme(.dark)
}
