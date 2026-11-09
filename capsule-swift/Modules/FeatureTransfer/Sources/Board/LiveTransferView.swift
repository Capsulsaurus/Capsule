import CapsuleDomain
import CapsuleFoundation
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - LiveTransferView

/// The real-time transfer view: every upload and fetch as it moves, chunk by
/// chunk, drawn from ``TransferBoard``.
///
/// Reached from the Transfer Center. Port required: ``TransferEventPort``.
///
/// Each transfer draws its chunk strip (``TransferBoard/Transfer/cells(_:)``),
/// where trouble was (scars), the adaptive chunk size and the sizes before it,
/// its throughput, and its custody receipt; a header totals throughput, active
/// transfers, missed events and custody answers. The shell is chosen by size
/// class: a regular width draws a longer strip and lays a row's facts out in
/// one line, a compact width stacks them.
public struct LiveTransferView: View {
    @State private var model: LiveTransferModel
    @Environment(\.horizontalSizeClass) private var sizeClass

    public init(transferEvents: any TransferEventPort, clock: LiveTransferClock = .continuous) {
        _model = State(wrappedValue: LiveTransferModel(port: transferEvents, clock: clock))
    }

    public var body: some View {
        List {
            summarySection
            transfersSection
        }
        .listStyle(.inset)
        .navigationTitle("app.transfer.live.title")
        .overlay { placeholder }
        .task { await model.watch() }
        .task { await model.runClock() }
    }

    private var isRegular: Bool { sizeClass == .regular }

    // MARK: Summary

    private var summarySection: some View {
        Section {
            LabeledContent("app.transfer.live.summary.active") {
                Text(verbatim: TransferFormat.count(model.board.activeCount))
            }
            LabeledContent("app.transfer.detail.throughput") {
                Text(verbatim: TransferFormat.rate(bytesPerSecond: Double(model.board.throughput)))
            }
            if model.board.missed > 0 {
                LabeledContent("app.transfer.live.summary.missed") {
                    Text(verbatim: TransferFormat.count(Int(clamping: model.board.missed)))
                }
            }
            custodyRow
        }
    }

    private var custodyRow: some View {
        let custody = model.board.custody
        return LabeledContent("app.transfer.live.custody.title") {
            HStack(spacing: CapsuleTheme.Spacing.small) {
                Label {
                    Text(verbatim: TransferFormat.count(Int(clamping: custody.durable)))
                } icon: {
                    Image(systemName: "checkmark.shield")
                }
                .foregroundStyle(.green)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("app.transfer.live.custody.durable")
                .accessibilityValue(Text(verbatim: TransferFormat.count(Int(clamping: custody.durable))))
                Label {
                    Text(verbatim: TransferFormat.count(Int(clamping: custody.notDurable)))
                } icon: {
                    Image(systemName: "exclamationmark.shield")
                }
                .foregroundStyle(custody.notDurable > 0 ? .orange : .secondary)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("app.transfer.live.custody.not_durable")
                .accessibilityValue(Text(verbatim: TransferFormat.count(Int(clamping: custody.notDurable))))
                Label {
                    Text(verbatim: TransferFormat.count(Int(clamping: custody.receipts)))
                } icon: {
                    Image(systemName: "doc.text.magnifyingglass")
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel("app.transfer.live.custody.receipts")
                .accessibilityValue(Text(verbatim: TransferFormat.count(Int(clamping: custody.receipts))))
            }
            .labelStyle(.titleAndIcon)
            .font(.callout.monospacedDigit())
        }
    }

    // MARK: Transfers

    @ViewBuilder
    private var transfersSection: some View {
        if !model.transfers.isEmpty {
            Section("app.transfer.live.transfers") {
                ForEach(model.transfers, id: \.identity) { transfer in
                    LiveTransferRow(
                        transfer: transfer,
                        now: model.board.nowMilliseconds,
                        cells: isRegular ? 64 : 32,
                        isRegular: isRegular
                    )
                }
            }
        }
    }

    @ViewBuilder
    private var placeholder: some View {
        if model.transfers.isEmpty {
            ContentUnavailableView {
                Label("app.transfer.live.empty.title", systemImage: "arrow.up.arrow.down.circle")
            } description: {
                Text("app.transfer.live.empty.description")
            }
        }
    }
}

// MARK: - LiveTransferRow

/// One transfer: what it is, where it is, its chunk strip, and the facts the
/// strip cannot say.
struct LiveTransferRow: View {
    let transfer: TransferBoard.Transfer
    let now: UInt64
    let cells: Int
    let isRegular: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            header
            if let strip = transfer.cells(cells) {
                ChunkStripView(cells: strip, fraction: transfer.fractionDone)
            }
            facts
        }
        .padding(.vertical, CapsuleTheme.Spacing.xSmall)
    }

    private var header: some View {
        HStack(spacing: CapsuleTheme.Spacing.small) {
            Label(LocalizedStringKey(transfer.direction.badge.titleKey), systemImage: transfer.direction.badge.systemImage)
                .labelStyle(.iconOnly)
                .foregroundStyle(transfer.direction.badge.tint)
            Text(verbatim: TransferFormat.shortDigest(transfer.blob.rawValue))
                .font(.callout.monospaced())
                .lineLimit(1)
            Spacer(minLength: CapsuleTheme.Spacing.small)
            Label(LocalizedStringKey(transfer.phase.badge.titleKey), systemImage: transfer.phase.badge.systemImage)
                .font(.caption.weight(.semibold))
                .foregroundStyle(transfer.phase.badge.tint)
        }
    }

    @ViewBuilder
    private var facts: some View {
        let layout = isRegular
            ? AnyLayout(HStackLayout(alignment: .top, spacing: CapsuleTheme.Spacing.large))
            : AnyLayout(VStackLayout(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall))
        layout {
            progressFact
            if let chunkSize = transfer.chunkSize {
                fact("app.transfer.live.detail.chunk_size", TransferFormat.bytes(chunkSize))
            }
            if !transfer.earlierChunkSizes.isEmpty {
                fact(
                    "app.transfer.live.detail.chunk_history",
                    transfer.earlierChunkSizes.map(TransferFormat.bytes).formatted(.list(type: .and))
                )
            }
            if !transfer.phase.isFinished {
                fact(
                    "app.transfer.detail.throughput",
                    TransferFormat.rate(bytesPerSecond: Double(transfer.throughput(at: now)))
                )
            }
            if transfer.retries > 0 {
                fact("app.transfer.live.detail.retries", TransferFormat.count(transfer.retries))
            }
            if transfer.rejects > 0 {
                fact("app.transfer.live.detail.rejects", TransferFormat.count(transfer.rejects))
            }
            if transfer.sessions > 1 {
                fact("app.transfer.live.detail.sessions", TransferFormat.count(transfer.sessions))
            }
            if let resumedAt = transfer.resumedAt {
                fact("app.transfer.live.detail.resumed_at", TransferFormat.bytes(resumedAt))
            }
            if let receipt = transfer.receiptSequence {
                fact("app.transfer.live.detail.receipt", TransferFormat.count(Int(clamping: receipt)))
            }
        }
        .font(.caption)
        if let note = transfer.lastRetry {
            Label(LocalizedStringKey(note.titleKey), systemImage: "arrow.clockwise")
                .font(.caption)
                .foregroundStyle(.orange)
        }
    }

    @ViewBuilder
    private var progressFact: some View {
        if let size = transfer.size {
            fact(
                "app.transfer.live.detail.progress",
                String(
                    format: String(localized: "app.transfer.live.detail.progress.value"),
                    TransferFormat.bytes(transfer.done),
                    TransferFormat.bytes(size)
                )
            )
        } else {
            fact("app.transfer.live.detail.progress", TransferFormat.bytes(transfer.done))
        }
    }

    private func fact(_ key: LocalizedStringKey, _ value: String) -> some View {
        HStack(spacing: CapsuleTheme.Spacing.xSmall) {
            Text(key).foregroundStyle(.secondary)
            Text(verbatim: value).monospacedDigit()
        }
        .accessibilityElement(children: .combine)
    }
}

// MARK: - ChunkStripView

/// A transfer's bytes as a strip of cells, coloured by what each span is
/// doing. One accessibility element whose value is the share done.
struct ChunkStripView: View {
    let cells: [TransferBoard.Cell]
    let fraction: Double?

    var body: some View {
        HStack(spacing: 1) {
            ForEach(cells.indices, id: \.self) { index in
                Rectangle().fill(cells[index].tint)
            }
        }
        .frame(height: 10)
        .clipShape(RoundedRectangle(cornerRadius: CapsuleTheme.Radius.small, style: .continuous))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("app.transfer.live.strip")
        .accessibilityValue(Text(verbatim: fraction.map(TransferFormat.percent) ?? ""))
    }
}

// MARK: - Previews

#Preview("Live transfers") {
    NavigationStack {
        LiveTransferView(transferEvents: MockEnvironment(scenario: .healthy).transferEvents)
    }
}
