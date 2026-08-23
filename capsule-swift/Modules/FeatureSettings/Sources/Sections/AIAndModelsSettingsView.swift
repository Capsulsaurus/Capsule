import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import SwiftUI

// MARK: - AIAndModelsSettingsView

/// AI & Models — slots, versions, provenance, staleness, and re-index.
public struct AIAndModelsSettingsView: View {
    @State private var model: AIAndModelsSettingsModel
    @State private var slotPendingRemoval: ModelSlot?

    public init(model: AIAndModelsSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: AIAndModelsSettingsModel(
                intelligence: environment.intelligence,
                settings: environment.settings,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.aiAndModels.titleKey,
            phase: model.phase,
            emptyTitleKey: "app.settings.ai.empty.title",
            emptyDescriptionKey: "app.settings.ai.empty.description",
            retry: { await model.load() },
            content: {
                processingSection
                if model.hasStaleExclusions {
                    staleSection
                }
                slotsSection
                provenanceSection
            }
        )
        .task { await model.load() }
        .settingsDestructiveConfirmation(
            titleKey: "app.settings.ai.remove.confirm.title",
            messageKey: "app.settings.ai.remove.confirm.message",
            confirmKey: "app.settings.ai.remove.confirm.action",
            isPresented: removalPresented
        ) {
            if let slot = slotPendingRemoval {
                await model.remove(slot)
            }
            slotPendingRemoval = nil
        }
    }

    private var removalPresented: Binding<Bool> {
        Binding(
            get: { slotPendingRemoval != nil },
            set: { presented in if !presented { slotPendingRemoval = nil } }
        )
    }

    // MARK: Processing

    private var processingSection: some View {
        Section {
            Toggle("app.settings.ai.processing.toggle", isOn: processingBinding)
            Toggle("app.settings.ai.power.toggle", isOn: powerBinding)
                .disabled(!model.isProcessingEnabled)
            SettingsValueRow(
                labelKey: "app.settings.ai.pending.label",
                value: SettingsFormat.count(model.pendingAssetCount)
            )
        } header: {
            Text("app.settings.ai.processing.header")
        } footer: {
            Text("app.settings.ai.processing.footer")
        }
    }

    private var processingBinding: Binding<Bool> {
        Binding(
            get: { model.isProcessingEnabled },
            set: { newValue in Task { await model.setProcessingEnabled(newValue) } }
        )
    }

    private var powerBinding: Binding<Bool> {
        Binding(
            get: { model.requiresPower },
            set: { newValue in Task { await model.setRequiresPower(newValue) } }
        )
    }

    // MARK: Staleness

    /// The banner that stops a shrunken search result reading as data loss.
    private var staleSection: some View {
        Section {
            ForEach(model.excludedSlots, id: \.self) { slot in
                SettingsStatusRow(
                    labelKey: "app.settings.ai.stale.slot",
                    statusKey: "app.settings.ai.state.stale_excluded",
                    tone: .caution
                )
                SettingsValueRow(
                    labelKey: "app.settings.ai.slot.label",
                    value: SettingsFormat.modelSlot(slot)
                )
                Button("app.settings.ai.regenerate") {
                    Task { await model.regenerate(slot) }
                }
                .disabled(model.busySlot != nil)
            }
        } header: {
            Text("app.settings.ai.stale.header")
        } footer: {
            Text("app.settings.ai.stale.footer")
        }
    }

    // MARK: Slots

    private var slotsSection: some View {
        Section {
            ForEach(model.statuses) { status in
                slotRows(status)
            }
        } header: {
            Text("app.settings.ai.slots.header")
        } footer: {
            Text("app.settings.ai.slots.footer")
        }
    }

    @ViewBuilder
    private func slotRows(_ status: AIModelStatus) -> some View {
        let report = model.report(for: status)
        SettingsStatusRow(
            labelKey: purposeKey(status.purpose),
            statusKey: report.statusKey,
            tone: report.tone
        )
        SettingsValueRow(
            labelKey: "app.settings.ai.slot.label",
            value: SettingsFormat.modelSlot(status.slot)
        )
        if case let .downloading(fraction) = report {
            ProgressView(value: fraction)
                .accessibilityLabel(Text("app.settings.ai.state.downloading"))
        }
        slotActions(status: status, report: report)
    }

    @ViewBuilder
    private func slotActions(status: AIModelStatus, report: AISlotReport) -> some View {
        switch report {
        case .notDownloaded:
            Button("app.settings.ai.download") {
                Task { await model.download(status.slot) }
            }
            .disabled(model.busySlot != nil)
        case .ready, .staleExcluded:
            Button("app.settings.ai.regenerate") {
                Task { await model.regenerate(status.slot) }
            }
            .disabled(model.busySlot != nil)
            Button("app.settings.ai.remove", role: .destructive) {
                slotPendingRemoval = status.slot
            }
        case .downloading, .unsupportedOnThisDevice:
            EmptyView()
        }
    }

    private func purposeKey(_ purpose: AIModelStatus.Purpose) -> String {
        switch purpose {
        case .imageEmbedding: "app.settings.ai.purpose.image_embedding"
        case .faceDetection: "app.settings.ai.purpose.face_detection"
        case .faceEmbedding: "app.settings.ai.purpose.face_embedding"
        case .sceneTagging: "app.settings.ai.purpose.scene_tagging"
        case .unknown: "app.settings.ai.purpose.unknown"
        }
    }

    // MARK: Provenance

    private var provenanceSection: some View {
        Section {
            SettingsNoteRow(textKey: "app.settings.ai.provenance.body")
        } header: {
            Text("app.settings.ai.provenance.header")
        } footer: {
            Text("app.settings.ai.provenance.footer")
        }
    }
}

// MARK: - Preview

#Preview("AI & Models") {
    NavigationStack {
        AIAndModelsSettingsView(environment: .preview())
    }
}

#Preview("AI & Models — Dark") {
    NavigationStack {
        AIAndModelsSettingsView(environment: .preview())
    }
    .preferredColorScheme(.dark)
}
