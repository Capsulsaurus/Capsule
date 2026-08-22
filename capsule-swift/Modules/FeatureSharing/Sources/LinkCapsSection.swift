import CapsuleUI
import SwiftUI

// MARK: - LinkCapsSection

/// The caps form for a guest upload link (*Web Upload — Security Contract*).
///
/// Split out of the composer because caps are the part of the screen with real
/// rules: every cap is independently optional, and the combinations that cannot
/// bind are reported inline rather than by a disabled button with no
/// explanation.
struct LinkCapsSection: View {
    @Binding var draft: LinkCapsDraft
    let issues: [LinkCapsIssue]

    var body: some View {
        Section {
            expiry
            totalBytes
            fileCount
            fileSize
            Toggle("ios.drops.caps.single_use", isOn: $draft.singleUse)
            ForEach(issues) { issue in
                Label(message(for: issue), systemImage: "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(.orange)
                    .accessibilityElement(children: .combine)
            }
        } header: {
            Text("ios.drops.caps.header")
        } footer: {
            Text("ios.drops.caps.footer")
        }
    }

    @ViewBuilder
    private var expiry: some View {
        Toggle("ios.drops.caps.expiry.toggle", isOn: $draft.expiryEnabled)
        if draft.expiryEnabled {
            DatePicker(
                "ios.drops.caps.expiry.date",
                selection: $draft.expiryDate,
                displayedComponents: [.date, .hourAndMinute]
            )
        }
    }

    @ViewBuilder
    private var totalBytes: some View {
        Toggle("ios.drops.caps.total_bytes.toggle", isOn: $draft.totalBytesEnabled)
        if draft.totalBytesEnabled {
            Stepper(value: $draft.totalGibibytes, in: 1 ... 512, step: 1) {
                LabeledContent {
                    Text(byteCount(gibibytes: draft.totalGibibytes))
                } label: {
                    Text("ios.drops.caps.total_bytes.value")
                }
            }
        }
    }

    @ViewBuilder
    private var fileCount: some View {
        Toggle("ios.drops.caps.file_count.toggle", isOn: $draft.fileCountEnabled)
        if draft.fileCountEnabled {
            Stepper(value: $draft.fileCount, in: 1 ... 1000) {
                LabeledContent {
                    Text(draft.fileCount, format: .number)
                } label: {
                    Text("ios.drops.caps.file_count.value")
                }
            }
        }
    }

    @ViewBuilder
    private var fileSize: some View {
        Toggle("ios.drops.caps.file_size.toggle", isOn: $draft.fileSizeEnabled)
        if draft.fileSizeEnabled {
            Stepper(value: $draft.fileMebibytes, in: 1 ... 8192, step: 64) {
                LabeledContent {
                    Text(byteCount(mebibytes: draft.fileMebibytes))
                } label: {
                    Text("ios.drops.caps.file_size.value")
                }
            }
        }
    }

    private func byteCount(gibibytes: Double) -> String {
        Int64(gibibytes * LinkCapsDraft.bytesPerGibibyte).formatted(.byteCount(style: .binary))
    }

    private func byteCount(mebibytes: Double) -> String {
        Int64(mebibytes * LinkCapsDraft.bytesPerMebibyte).formatted(.byteCount(style: .binary))
    }

    private func message(for issue: LinkCapsIssue) -> LocalizedStringKey {
        switch issue {
        case .expiryInPast: "ios.drops.caps.issue.expiry_past"
        case .zeroCap: "ios.drops.caps.issue.zero"
        case .fileSizeExceedsTotal: "ios.drops.caps.issue.file_over_total"
        case .singleUseWithMultipleFiles: "ios.drops.caps.issue.single_use_conflict"
        }
    }
}
