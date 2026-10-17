import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import SwiftUI

// MARK: - BackupAndRecoverySettingsView

/// Backup & Recovery — escrow, verification cadence, re-wrap, and restore.
public struct BackupAndRecoverySettingsView: View {
    @State private var model: BackupAndRecoverySettingsModel
    @State private var restoreGate = TypedPhraseGate(
        phraseKey: "app.settings.recovery.restore.phrase"
    )
    @State private var isRestoreSheetPresented = false

    public init(model: BackupAndRecoverySettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: BackupAndRecoverySettingsModel(
                recovery: environment.recovery,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.backupAndRecovery.titleKey,
            phase: model.phase,
            retry: { await model.load() },
            content: {
                escrowSection
                if let secret = model.mintedSecret {
                    mintedSecretSection(secret)
                }
                verificationSection
                restoreSection
            }
        )
        .task { await model.load() }
        .sheet(isPresented: $isRestoreSheetPresented) {
            TypedPhraseConfirmationSheet(
                titleKey: "app.settings.recovery.restore.confirm.title",
                messageKey: "app.settings.recovery.restore.confirm.message",
                fieldLabelKey: "app.settings.confirm.phrase.field",
                confirmKey: "app.settings.recovery.restore.confirm.action",
                gate: restoreGate
            ) {
                await model.commitRestore(gate: restoreGate)
            }
        }
    }

    // MARK: Escrow

    private var escrowSection: some View {
        Section {
            SettingsStatusRow(
                labelKey: "app.settings.recovery.escrow.label",
                statusKey: model.isConfigured
                    ? "app.settings.recovery.escrow.configured"
                    : "app.settings.recovery.escrow.missing",
                tone: model.isConfigured ? .positive : .critical
            )
            SettingsValueRow(
                labelKey: "app.settings.recovery.escrow.updated",
                value: SettingsFormat.day(model.summary?.escrowUpdatedAt)
            )
            shamirRows
            if !model.isConfigured {
                Button("app.settings.recovery.setup") {
                    Task { await model.setUpRecovery() }
                }
                .disabled(model.isWorking)
            }
            Button("app.settings.recovery.rotate") {
                Task { await model.rotateSecret() }
            }
            .disabled(model.isWorking || !model.isConfigured)
        } header: {
            Text("app.settings.recovery.escrow.header")
        } footer: {
            Text("app.settings.recovery.escrow.footer")
        }
    }

    @ViewBuilder
    private var shamirRows: some View {
        if let shares = model.summary?.shamirShareCount,
           let threshold = model.summary?.shamirThreshold {
            SettingsValueRow(
                labelKey: "app.settings.recovery.shamir.shares",
                value: SettingsFormat.count(shares)
            )
            SettingsValueRow(
                labelKey: "app.settings.recovery.shamir.threshold",
                value: SettingsFormat.count(threshold)
            )
        }
    }

    /// The secret, shown once.
    ///
    /// The app never persists it — the port hands it over exactly once, and a
    /// settings screen that stashed a copy would be creating a second place the
    /// only root can leak from.
    private func mintedSecretSection(_ secret: String) -> some View {
        Section {
            Text(verbatim: secret)
                .font(.body.monospaced())
                .textSelection(.enabled)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityLabel(Text("app.settings.recovery.minted.label"))
            Button("app.settings.recovery.minted.dismiss") { model.dismissMintedSecret() }
        } header: {
            Text("app.settings.recovery.minted.header")
        } footer: {
            Text("app.settings.recovery.minted.footer")
        }
    }

    // MARK: Verification

    private var verificationSection: some View {
        Section {
            SettingsStatusRow(
                labelKey: "app.settings.recovery.verify.state",
                statusKey: model.isVerificationDue
                    ? "app.settings.recovery.verify.due"
                    : "app.settings.recovery.verify.not_due",
                tone: model.isVerificationDue ? .caution : .positive
            )
            SettingsValueRow(
                labelKey: "app.settings.recovery.verify.next",
                value: SettingsFormat.day(model.summary?.verification.nextDueAt)
            )
            SecureField(
                "app.settings.recovery.verify.field",
                text: $model.passphraseInput
            )
            .accessibilityLabel(Text("app.settings.recovery.verify.field"))
            Button("app.settings.recovery.verify.action") {
                Task { await model.verify() }
            }
            .disabled(model.isWorking || model.passphraseInput.isEmpty)
            verificationOutcomeRow
            Button("app.settings.recovery.verify.snooze") {
                Task { await model.snooze(days: 7) }
            }
            .disabled(!model.canSnooze)
        } header: {
            Text("app.settings.recovery.verify.header")
        } footer: {
            Text("app.settings.recovery.verify.footer")
        }
    }

    @ViewBuilder
    private var verificationOutcomeRow: some View {
        switch model.lastVerification {
        case .verified:
            SettingsStatusRow(
                labelKey: "app.settings.recovery.verify.result",
                statusKey: "app.settings.recovery.verify.verified",
                tone: .positive
            )
        case .mismatch:
            SettingsStatusRow(
                labelKey: "app.settings.recovery.verify.result",
                statusKey: "app.settings.recovery.verify.mismatch",
                tone: .caution
            )
        case let .inconclusive(code):
            SettingsStatusRow(
                labelKey: "app.settings.recovery.verify.result",
                statusKey: code.rawValue,
                tone: .neutral
            )
        case .none:
            EmptyView()
        }
        if model.shouldOfferGuidedRewrap {
            SettingsNoteRow(textKey: "app.settings.recovery.verify.rewrap_offer")
        }
    }

    // MARK: Restore

    /// The dry-run report, then the typed-phrase gate.
    ///
    /// The report is shown *before* the gate for the reason the design doc
    /// gives: dry-run is the default, and a commit is a separate, explicit act.
    private var restoreSection: some View {
        Section {
            ForEach(model.restoreRules, id: \.self) { rule in
                SettingsValueRow(
                    labelKey: rule.titleKey,
                    value: SettingsPhrase.text(forKey: rule.detailKey)
                )
            }
            SecureField(
                "app.settings.recovery.restore.field",
                text: $model.restoreSecretInput
            )
            .accessibilityLabel(Text("app.settings.recovery.restore.field"))
            Button("app.settings.recovery.restore.action", role: .destructive) {
                restoreGate.reset()
                isRestoreSheetPresented = true
            }
            .disabled(model.restoreSecretInput.isEmpty)
            if let account = model.restoredAccount {
                SettingsValueRow(
                    labelKey: "app.settings.recovery.restore.restored",
                    value: account.handle
                )
            }
        } header: {
            Text("app.settings.recovery.restore.header")
        } footer: {
            Text("app.settings.recovery.restore.footer")
        }
    }
}

// MARK: - Preview

#Preview("Backup & Recovery") {
    NavigationStack {
        BackupAndRecoverySettingsView(environment: .preview())
    }
}

#Preview("Backup & Recovery — Overdue") {
    NavigationStack {
        BackupAndRecoverySettingsView(environment: .preview(.recoveryOverdue))
    }
}
