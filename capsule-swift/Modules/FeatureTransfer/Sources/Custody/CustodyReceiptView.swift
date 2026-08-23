import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - CustodyReceiptView

/// The server-signed receipt for an asset: what was attested, when, and by
/// which key — plus the verify-before-destroy gate.
///
/// The receipt is what makes "the server lost my photo" and "the client never
/// uploaded it" distinguishable rather than symmetric unfalsifiable claims
/// (*Storage Verification — Custody Receipts*). A non-`durable` verdict reads
/// as **"not yet confirmed on server"** and carries no destructive action at
/// all.
///
/// Route entry point. Ports required: ``UploadPort`` (receipts),
/// ``StoragePort`` (the verdict and the release).
public struct CustodyReceiptView: View {
    @State private var model: CustodyReceiptModel
    @State private var isConfirmingRelease = false

    public init(
        assetID: AssetID,
        uploads: any UploadPort,
        storage: any StoragePort,
        clock: TransferClock = .system
    ) {
        _model = State(wrappedValue: CustodyReceiptModel(
            assetID: assetID,
            uploads: uploads,
            storage: storage,
            clock: clock
        ))
    }

    public var body: some View {
        content
            .navigationTitle("app.custody.title")
            .task { await model.load() }
            .confirmationDialog(
                "app.custody.release.confirm.title",
                isPresented: $isConfirmingRelease,
                titleVisibility: .visible
            ) {
                Button("app.custody.release.confirm.action", role: .destructive) {
                    Task { await model.releaseLocalCopy() }
                }
                Button("app.common.cancel", role: .cancel) {}
            } message: {
                Text("app.custody.release.confirm.message")
            }
    }

    @ViewBuilder
    private var content: some View {
        if model.phase.hasContent {
            List {
                CustodyVerdictSection(verdict: model.verdict, freshnessSeconds: model.freshnessSeconds)
                verdictActions
                logSection
                ForEach(model.receipts) { receipt in CustodyReceiptSection(receipt: receipt) }
            }
            .listStyle(.inset)
        } else {
            PhasePlaceholderView(
                phase: model.phase,
                emptyTitle: "app.custody.empty.title",
                emptyDescription: "app.custody.empty.description",
                emptySymbol: "doc.questionmark",
                retry: { await model.reload() }
            )
        }
    }

    /// The destructive action exists **only** in the releasable verdict. Every
    /// other verdict offers re-verification and nothing else.
    @ViewBuilder
    private var verdictActions: some View {
        Section {
            Button("app.custody.action.verify") {
                Task { await model.verify(deep: false) }
            }
            .disabled(model.isBusy || !model.phase.permitsNetworkActions)
            Button("app.custody.action.verify_deep") {
                Task { await model.verify(deep: true) }
            }
            .disabled(model.isBusy || !model.phase.permitsNetworkActions)
            if model.verdict.permitsRelease {
                Button("app.custody.action.release", role: .destructive) {
                    isConfirmingRelease = true
                }
                .disabled(model.isBusy)
            }
        } footer: {
            Text(model.verdict.permitsRelease
                ? "app.custody.action.release.footer"
                : "app.custody.action.blocked.footer")
        }
    }

    private var logSection: some View {
        Section("app.custody.log.title") {
            if let sequence = model.highestReceiptSequence {
                LabeledContent("app.custody.log.sequence") {
                    Text(verbatim: TransferFormat.count(Int(clamping: sequence)))
                }
            }
            Label(
                model.isChained ? "app.custody.log.chained" : "app.custody.log.unchained",
                systemImage: model.isChained ? "link" : "link.badge.plus"
            )
            .foregroundStyle(model.isChained ? Color.secondary : Color.orange)
        }
    }
}

// MARK: - CustodyVerdictSection

/// The verdict, in the words the design doc uses.
struct CustodyVerdictSection: View {
    let verdict: CustodyVerdict
    let freshnessSeconds: Int64

    var body: some View {
        Section {
            Label(titleKey, systemImage: symbol)
                .font(.headline)
                .foregroundStyle(tint)
            Text(descriptionKey)
                .font(.footnote)
                .foregroundStyle(.secondary)
            if case let .notYetConfirmed(missing) = verdict, !missing.isEmpty {
                ForEach(missing) { blob in MissingBlobRow(blob: blob) }
            }
        } footer: {
            Text(String(format: String(localized: "app.custody.verdict.freshness"),
                        TransferFormat.count(Int(freshnessSeconds))))
        }
    }

    private var titleKey: LocalizedStringKey {
        switch verdict {
        case .unchecked: "app.custody.verdict.unchecked"
        case .notYetConfirmed: "app.custody.verdict.not_confirmed"
        case .receiptMissing: "app.custody.verdict.receipt_missing"
        case .confirmedButStale: "app.custody.verdict.stale"
        case .releasable: "app.custody.verdict.durable"
        }
    }

    private var descriptionKey: LocalizedStringKey {
        switch verdict {
        case .unchecked: "app.custody.verdict.unchecked.description"
        case .notYetConfirmed: "app.custody.verdict.not_confirmed.description"
        case .receiptMissing: "app.custody.verdict.receipt_missing.description"
        case .confirmedButStale: "app.custody.verdict.stale.description"
        case .releasable: "app.custody.verdict.durable.description"
        }
    }

    private var symbol: String {
        switch verdict {
        case .unchecked: "questionmark.circle"
        case .notYetConfirmed: "clock.badge.exclamationmark"
        case .receiptMissing: "doc.badge.ellipsis"
        case .confirmedButStale: "arrow.clockwise.circle"
        case .releasable: "checkmark.seal.fill"
        }
    }

    private var tint: Color {
        switch verdict {
        case .releasable: .green
        case .unchecked, .confirmedButStale: .secondary
        case .notYetConfirmed, .receiptMissing: .orange
        }
    }
}

// MARK: - MissingBlobRow

/// A blob the server does not fully hold.
///
/// Surfaced, never omitted: a hash the client listed that the server does not
/// associate with the asset comes back not-stored and not-indexed precisely so
/// a missing blob cannot be mistaken for one nobody asked about.
struct MissingBlobRow: View {
    let blob: BlobVerdict

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            Text(verbatim: TransferFormat.shortDigest(blob.hash.rawValue))
                .font(.caption.monospaced())
            ViewThatFits(in: .horizontal) {
                HStack(spacing: CapsuleTheme.Spacing.small) { flags }
                VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) { flags }
            }
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder
    private var flags: some View {
        factLabel("app.custody.blob.stored", isSatisfied: blob.stored)
        factLabel("app.custody.blob.indexed", isSatisfied: blob.indexed)
        factLabel("app.custody.blob.retrievable", isSatisfied: blob.retrievable)
    }

    private func factLabel(_ key: LocalizedStringKey, isSatisfied: Bool) -> some View {
        Label(key, systemImage: isSatisfied ? "checkmark.circle.fill" : "xmark.circle.fill")
            .font(.caption)
            .foregroundStyle(isSatisfied ? Color.green : Color.orange)
    }
}

// MARK: - Previews

#Preview("Durable, releasable") {
    let environment = MockEnvironment(scenario: .healthy)
    return NavigationStack {
        CustodyReceiptView(
            assetID: .managed(uuid: "preview"),
            uploads: environment.uploads,
            storage: environment.storage,
            clock: .fixed(environment.configuration.clock.now)
        )
    }
}

#Preview("Awaiting original — not yet confirmed") {
    let environment = MockEnvironment(scenario: .awaitingOriginals)
    return NavigationStack {
        CustodyReceiptView(
            assetID: .managed(uuid: "preview"),
            uploads: environment.uploads,
            storage: environment.storage,
            clock: .fixed(environment.configuration.clock.now)
        )
    }
    .preferredColorScheme(.dark)
}
