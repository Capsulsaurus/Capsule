import CapsuleDomain
import CapsuleUI
import SwiftUI

// MARK: - SessionStateTrack

/// The session state machine, drawn as the track it is.
///
/// ```text
/// pending ─▶ uploading ─▶ waitingForProcessing ─▶ completed
///                                             └─▶ failedProcessing
/// ```
///
/// Both terminal states are **receipts, not disappearances** (*Upload Protocol
/// — Session Lifetime and Discard*): a client whose finalization
/// acknowledgement was lost re-queries and learns the upload already succeeded
/// or failed. Drawing the track all the way to a terminal marker is how the
/// screen tells that truth instead of showing a vanished session.
struct SessionStateTrack: View {
    let state: UploadSessionState

    private static let happyPath: [UploadSessionState] = [
        .pending, .uploading, .waitingForProcessing, .completed,
    ]

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: CapsuleTheme.Spacing.xSmall) { steps }
                VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) { steps }
            }
            if state == .failedProcessing {
                Label("app.transfer.session.failed.description", systemImage: "xmark.octagon.fill")
                    .font(.caption)
                    .foregroundStyle(.red)
            }
            if state == .waitingForProcessing {
                Label("app.transfer.session.waiting.description", systemImage: "lock")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("app.transfer.session.track")
    }

    @ViewBuilder
    private var steps: some View {
        ForEach(Self.happyPath, id: \.rawValue) { step in
            Label(LocalizedStringKey(step.badge.titleKey), systemImage: symbol(for: step))
                .font(.caption)
                .foregroundStyle(isReached(step) ? step.badge.tint : Color.secondary)
        }
    }

    /// A reached step is filled; an unreached one is an outline. Shape carries
    /// the state as well as colour.
    private func symbol(for step: UploadSessionState) -> String {
        isReached(step) ? "circle.fill" : "circle"
    }

    private func isReached(_ step: UploadSessionState) -> Bool {
        guard let current = Self.happyPath.firstIndex(of: state),
              let candidate = Self.happyPath.firstIndex(of: step)
        else { return state == .failedProcessing && step != .completed }
        return candidate <= current
    }
}

// MARK: - UploadFailureRow

/// A failure and its **documented recovery, as the button label**.
struct UploadFailureRow: View {
    let failure: UploadFailure
    let recover: () async -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            Label(failure.option.messageKey, systemImage: "exclamationmark.triangle.fill")
                .font(.subheadline)
                .foregroundStyle(.red)
            Text(failure.option.explanationKey)
                .font(.caption)
                .foregroundStyle(.secondary)
            HStack(spacing: CapsuleTheme.Spacing.small) {
                BadgeChip(failure.tier.badge)
                Spacer(minLength: CapsuleTheme.Spacing.small)
                Button(failure.option.buttonTitleKey) {
                    Task { await recover() }
                }
                .buttonStyle(.bordered)
            }
        }
        .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
    }
}

// MARK: - AdaptiveChunkDisclosure

/// The chunk-size ladder, behind a disclosure.
///
/// Behind a disclosure because it is diagnostic detail, not a decision the user
/// makes: the client owns adaptation and the server enforces bounds by
/// rejection only (*Upload Protocol — Adaptive Chunk Sizing*). Showing it at
/// all is a traceability choice — when an upload is slow, the reason should be
/// inspectable rather than folklore.
struct AdaptiveChunkDisclosure: View {
    let plan: AdaptiveChunkPlan

    var body: some View {
        DisclosureGroup {
            LabeledContent("app.transfer.chunk.current") {
                Text(verbatim: TransferFormat.bytes(plan.currentBytes))
            }
            LabeledContent("app.transfer.chunk.suggested") {
                Text(verbatim: TransferFormat.bytes(plan.suggestedBytes))
            }
            LabeledContent("app.transfer.chunk.bounds") {
                Text(verbatim: bounds)
            }
            LabeledContent("app.transfer.chunk.window") {
                Text(verbatim: TransferFormat.count(AdaptiveChunkPlan.windowSeconds))
            }
            Label(LocalizedStringKey(plan.adjustment.titleKey), systemImage: reasonSymbol)
                .font(.footnote)
                .foregroundStyle(.secondary)
            if !plan.isAligned {
                Label("app.transfer.chunk.misaligned", systemImage: "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(.red)
            }
        } label: {
            Label("app.transfer.chunk.title", systemImage: "square.stack.3d.up")
        }
    }

    private var bounds: String {
        let low = TransferFormat.bytes(AdaptiveChunkPlan.minimumBytes)
        let high = TransferFormat.bytes(AdaptiveChunkPlan.maximumBytes)
        return String(format: String(localized: "app.transfer.chunk.bounds.range"), low, high)
    }

    private var reasonSymbol: String {
        switch plan.adjustment {
        case .raised: "arrow.up.right"
        case .lowered: "arrow.down.right"
        case .held: "equal"
        case .warmingUp: "hourglass"
        case .conservativeForAdverseLink: "tortoise"
        case .unmeasured: "questionmark"
        }
    }
}
