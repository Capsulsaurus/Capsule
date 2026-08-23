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
            Toggle("app.drops.caps.single_use", isOn: $draft.singleUse)
            ForEach(issues) { issue in
                Label(message(for: issue), systemImage: "exclamationmark.triangle")
                    .font(.footnote)
                    .foregroundStyle(.orange)
                    .accessibilityElement(children: .combine)
            }
        } header: {
            Text("app.drops.caps.header")
        } footer: {
            Text("app.drops.caps.footer")
        }
    }

    @ViewBuilder
    private var expiry: some View {
        Toggle("app.drops.caps.expiry.toggle", isOn: $draft.expiryEnabled)
        if draft.expiryEnabled {
            DatePicker(
                "app.drops.caps.expiry.date",
                selection: $draft.expiryDate,
                displayedComponents: [.date, .hourAndMinute]
            )
        }
    }

    @ViewBuilder
    private var totalBytes: some View {
        Toggle("app.drops.caps.total_bytes.toggle", isOn: $draft.totalBytesEnabled)
        if draft.totalBytesEnabled {
            Stepper(value: $draft.totalGibibytes, in: 1 ... 512, step: 1) {
                LabeledContent {
                    Text(byteCount(gibibytes: draft.totalGibibytes))
                } label: {
                    Text("app.drops.caps.total_bytes.value")
                }
            }
        }
    }

    @ViewBuilder
    private var fileCount: some View {
        Toggle("app.drops.caps.file_count.toggle", isOn: $draft.fileCountEnabled)
        if draft.fileCountEnabled {
            Stepper(value: $draft.fileCount, in: 1 ... 1000) {
                LabeledContent {
                    Text(draft.fileCount, format: .number)
                } label: {
                    Text("app.drops.caps.file_count.value")
                }
            }
        }
    }

    @ViewBuilder
    private var fileSize: some View {
        Toggle("app.drops.caps.file_size.toggle", isOn: $draft.fileSizeEnabled)
        if draft.fileSizeEnabled {
            Stepper(value: $draft.fileMebibytes, in: 1 ... 8192, step: 64) {
                LabeledContent {
                    Text(byteCount(mebibytes: draft.fileMebibytes))
                } label: {
                    Text("app.drops.caps.file_size.value")
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
        case .expiryInPast: "app.drops.caps.issue.expiry_past"
        case .zeroCap: "app.drops.caps.issue.zero"
        case .fileSizeExceedsTotal: "app.drops.caps.issue.file_over_total"
        case .singleUseWithMultipleFiles: "app.drops.caps.issue.single_use_conflict"
        }
    }
}
