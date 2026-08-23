import CapsuleDomain
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - RestoreFlowView

/// Restoring a backup artifact: preview → dry run → typed-phrase commit.
///
/// **Dry run is the default and commit is never the default.** A restore that
/// silently overwrote live state is the worst foot-gun a backup system can ship,
/// so the ladder is enforced by the model rather than by button placement — the
/// commit control cannot act until a dry run has produced a *committable* diff
/// and the user has typed the phrase exactly.
///
/// The two verification results are refusals, not warnings. An incomplete AMK
/// ledger means some asset in the artifact is silently unrecoverable, and a
/// broken signature chain means the manifest cannot be trusted to say what it
/// contains; either one takes commit off the table entirely, and the screen
/// explains which rather than greying a button out in silence.
///
/// Entry point: ``init(artifact:restore:recovery:)``, needing ``RestorePort``
/// and ``RecoveryPort``.
public struct RestoreFlowView: View {
    @State private var model: RestoreFlowViewModel
    @State private var isConfirmingCommit = false
    @State private var restoredAccount: AccountSummary?

    public init(artifact: URL, restore: any RestorePort, recovery: any RecoveryPort) {
        _model = State(wrappedValue: RestoreFlowViewModel(
            artifact: artifact,
            restore: restore,
            recovery: recovery
        ))
    }

    public var body: some View {
        CeremonyContainer {
            VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xLarge) {
                CeremonyHeader(
                    titleKey: "app.restore.title",
                    subtitleKey: "app.restore.subtitle",
                    symbolName: "arrow.clockwise.icloud"
                )
                banner
                previewStep
                dryRunStep
                commitStep
                shamirStep
            }
        }
        .task { await model.loadShares() }
        .sheet(isPresented: $isConfirmingCommit) { commitSheet }
    }

    @ViewBuilder
    private var banner: some View {
        if let failure = model.state.failure {
            AuthErrorBanner(error: failure) { Task { await model.runDryRun() } }
        }
        if model.isWorking {
            AuthLoadingView(labelKey: "app.restore.working")
        }
    }

    // MARK: Step 1 — preview

    private var previewStep: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "app.restore.preview.title",
                descriptionKey: "app.restore.preview.description",
                symbolName: "doc.text.magnifyingglass"
            )
            if let preview = model.preview {
                previewFacts(preview)
            }
            Button("app.restore.preview.run") { Task { await model.runPreview() } }
                .capsuleGlassButtonStyle(prominent: model.preview == nil)
                .disabled(model.isWorking)
                .accessibilityLabel("app.restore.preview.run")
        }
        .authCard()
    }

    private func previewFacts(_ preview: RestorePreview) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            AuthLabeledValue(
                labelKey: "app.restore.preview.assets",
                value: "\(preview.assetCount)"
            )
            LabeledContent {
                Text(preview.totalBytes, format: .byteCount(style: .file))
                    .foregroundStyle(.secondary)
            } label: {
                Text("app.restore.preview.size")
            }
            .accessibilityElement(children: .combine)
            AuthLabeledDate(labelKey: "app.restore.preview.exported", date: preview.exportedAt.date)
            AuthLabeledValue(labelKey: "app.restore.preview.exporter", value: preview.exporterModel)
            AuthLabeledValue(
                labelKey: "app.restore.preview.artifact_version",
                value: "\(preview.artifactVersion)"
            )
        }
    }

    // MARK: Step 2 — dry run

    private var dryRunStep: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "app.restore.dry_run.title",
                descriptionKey: "app.restore.dry_run.description",
                symbolName: "checklist"
            )
            if let diff = model.diff {
                diffFacts(diff)
                verificationFacts(diff)
            }
            Button("app.restore.dry_run.run") { Task { await model.runDryRun() } }
                .capsuleGlassButtonStyle(prominent: model.preview != nil && model.diff == nil)
                .disabled(model.isWorking || model.preview == nil)
                .accessibilityLabel("app.restore.dry_run.run")
        }
        .authCard()
    }

    private func diffFacts(_ diff: RestoreDiff) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            AuthLabeledValue(labelKey: "app.restore.diff.added", value: "\(diff.addedCount)")
            AuthLabeledValue(
                labelKey: "app.restore.diff.already_present",
                value: "\(diff.alreadyPresentCount)"
            )
            AuthLabeledValue(
                labelKey: "app.restore.diff.conflicting",
                value: "\(diff.conflictingCount)"
            )
            AuthLabeledValue(
                labelKey: "app.restore.diff.superseded",
                value: "\(diff.supersededByLocalCount)"
            )
            // Conflicts are quarantined for explicit merge, never applied: a
            // six-month-old backup must not resurrect an asset the user later
            // deleted.
            Text("app.restore.diff.reconciliation_note")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// The two checks that are refusals rather than warnings.
    private func verificationFacts(_ diff: RestoreDiff) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            StatusChip(
                titleKey: diff.amkLedgerIsComplete
                    ? "app.restore.verify.ledger_complete"
                    : "app.restore.verify.ledger_incomplete",
                symbolName: diff.amkLedgerIsComplete ? "checkmark.seal.fill" : "xmark.octagon.fill",
                tint: diff.amkLedgerIsComplete ? .green : .red
            )
            StatusChip(
                titleKey: diff.signatureChainIsIntact
                    ? "app.restore.verify.signatures_intact"
                    : "app.restore.verify.signatures_broken",
                symbolName: diff.signatureChainIsIntact ? "checkmark.seal.fill" : "xmark.octagon.fill",
                tint: diff.signatureChainIsIntact ? .green : .red
            )
            if !diff.isCommittable {
                Text("app.restore.verify.refused")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    // MARK: Step 3 — commit

    private var commitStep: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            AuthSectionHeader(
                titleKey: "app.restore.commit.title",
                descriptionKey: "app.restore.commit.description",
                symbolName: "square.and.arrow.down.on.square"
            )
            if model.committedDiff != nil {
                StatusChip(
                    titleKey: "app.restore.commit.done",
                    symbolName: "checkmark.seal.fill",
                    tint: .green
                )
            }
            Button("app.restore.commit.open", role: .destructive) { isConfirmingCommit = true }
                .buttonStyle(.bordered)
                .disabled(!model.hasCommittableDiff || model.isWorking)
                .accessibilityLabel("app.restore.commit.open")
            Text("app.restore.commit.note")
                .font(.footnote)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .authCard()
    }

    private var commitSheet: some View {
        AuthTypedPhraseSheet(
            titleKey: "app.restore.commit.confirm.title",
            messageKey: "app.restore.commit.confirm.message",
            fieldLabelKey: "app.restore.commit.confirm.field",
            confirmKey: "app.restore.commit.confirm.action",
            requiredPhrase: model.requiredPhrase,
            typedPhrase: $model.confirmationInput,
            isSatisfied: model.confirmationMatches,
            confirm: { await model.commit() }
        )
    }

    // MARK: Shamir

    private var shamirStep: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            RestoreShamirSection(
                shares: model.shares,
                selected: model.selectedShareIDs,
                threshold: RestoreFlowViewModel.defaultShamirThreshold,
                canReconstruct: model.canReconstructFromShares,
                isWorking: model.isWorking,
                toggle: { model.toggleShare($0) },
                reconstruct: { Task { restoredAccount = await model.restoreFromSelectedShares() } }
            )
            if let account = restoredAccount {
                AuthLabeledValue(labelKey: "app.restore.shamir.restored", value: account.handle)
            }
        }
    }
}

// MARK: - Previews

#Preview("Restore flow") {
    let world = AuthPreviewEnvironment.healthy
    return RestoreFlowView(
        artifact: world.artifactURL,
        restore: world.restore,
        recovery: world.recovery
    )
}

#Preview("Restore flow — artifact refused") {
    let world = AuthPreviewEnvironment(scenario: .healthy, restoreLedgerIsComplete: false)
    return RestoreFlowView(
        artifact: world.artifactURL,
        restore: world.restore,
        recovery: world.recovery
    )
}

#Preview("Restore flow — broken signature chain, dark") {
    let world = AuthPreviewEnvironment(scenario: .healthy, restoreSignatureChainIsIntact: false)
    return RestoreFlowView(
        artifact: world.artifactURL,
        restore: world.restore,
        recovery: world.recovery
    )
    .preferredColorScheme(.dark)
}
