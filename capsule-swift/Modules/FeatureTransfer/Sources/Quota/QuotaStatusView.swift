import CapsuleDomain
import CapsuleMock
import CapsulePorts
import CapsuleUI
import SwiftUI

// MARK: - QuotaStatusView

/// The account's storage position: a stacked bar by category with the trash
/// segment highlighted, and a banner for the five quota states.
///
/// Quota is what the **server** is charging. What this device holds is a
/// different question with different remedies, and it lives on
/// ``StorageReclamationView`` — the two screens are deliberately separate and
/// cross-linked, because conflating them is how a cache-clearing feature ends
/// up deleting an only copy.
///
/// Route entry point. Ports required: ``QuotaPort``, ``StoragePort`` (the
/// category ratios), ``SyncPort`` (connection class).
public struct QuotaStatusView: View {
    @State private var model: QuotaStatusModel
    private let onReviewLargest: (@MainActor () -> Void)?
    private let onEmptyTrash: (@MainActor () -> Void)?

    public init(
        quota: any QuotaPort,
        storage: any StoragePort,
        sync: any SyncPort,
        clock: TransferClock = .system,
        onEmptyTrash: (@MainActor () -> Void)? = nil,
        onReviewLargest: (@MainActor () -> Void)? = nil
    ) {
        _model = State(wrappedValue: QuotaStatusModel(
            quota: quota,
            storage: storage,
            sync: sync,
            clock: clock
        ))
        self.onEmptyTrash = onEmptyTrash
        self.onReviewLargest = onReviewLargest
    }

    public var body: some View {
        content
            .navigationTitle("ios.quota.title")
            .task { await model.load() }
    }

    @ViewBuilder
    private var content: some View {
        if model.phase.hasContent {
            List {
                if !model.phase.permitsNetworkActions { staleNotice }
                if model.warrants { Section { banner } }
                Section("ios.quota.usage.title") {
                    QuotaStackedBar(breakdown: model.breakdown)
                    LabeledContent("ios.quota.usage.used") {
                        Text(verbatim: TransferFormat.bytes(model.quota.used))
                    }
                    limitRows
                }
                crossLink
            }
            .listStyle(.inset)
        } else {
            PhasePlaceholderView(
                phase: model.phase,
                emptyTitle: "ios.quota.empty.title",
                emptyDescription: "ios.quota.empty.description",
                emptySymbol: "externaldrive",
                retry: { await model.reload() }
            )
        }
    }

    private var banner: some View {
        QuotaStateBanner(
            state: model.quota.state,
            permissions: model.permissions,
            remediations: model.remediations,
            graceDeadline: model.graceDeadline,
            now: model.now,
            perform: perform
        )
    }

    /// A quota figure read offline is the last one the server sent. Saying so
    /// is cheaper than pretending it is live.
    private var staleNotice: some View {
        Section {
            Label("ios.quota.offline.notice", systemImage: "wifi.slash")
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private var limitRows: some View {
        if model.quota.isUnlimited {
            Label("ios.quota.limit.unlimited", systemImage: "infinity")
                .foregroundStyle(.secondary)
        } else {
            LabeledContent("ios.quota.usage.soft_limit") {
                Text(verbatim: TransferFormat.bytes(model.quota.softLimit))
            }
            LabeledContent("ios.quota.usage.hard_limit") {
                Text(verbatim: TransferFormat.bytes(model.quota.hardLimit))
            }
            LabeledContent("ios.quota.usage.remaining") {
                Text(verbatim: TransferFormat.bytes(model.quota.remaining))
            }
        }
    }

    private var crossLink: some View {
        Section {
            Button {
                onReviewLargest?()
            } label: {
                Label("ios.quota.link.local_storage", systemImage: "internaldrive")
            }
            .disabled(onReviewLargest == nil)
        } footer: {
            Text("ios.quota.link.local_storage.footer")
        }
    }

    private func perform(_ remediation: QuotaRemediation) {
        switch remediation {
        case .emptyTrash: onEmptyTrash?()
        case .reviewLargest: onReviewLargest?()
        case .contactAdministrator: break
        }
    }
}

// MARK: - Previews

#Preview("Grace expired") {
    let environment = MockEnvironment(scenario: .quotaGraceExpired)
    return NavigationStack {
        QuotaStatusView(
            quota: environment.quota,
            storage: environment.storage,
            sync: environment.sync,
            clock: .fixed(environment.configuration.clock.now),
            onEmptyTrash: {},
            onReviewLargest: {}
        )
    }
}

#Preview("Soft warning, dark") {
    let environment = MockEnvironment(scenario: .quotaSoftWarning)
    return NavigationStack {
        QuotaStatusView(
            quota: environment.quota,
            storage: environment.storage,
            sync: environment.sync,
            clock: .fixed(environment.configuration.clock.now),
            onEmptyTrash: {},
            onReviewLargest: {}
        )
    }
    .preferredColorScheme(.dark)
}

#Preview("Offline") {
    let environment = MockEnvironment(scenario: .offline)
    return NavigationStack {
        QuotaStatusView(
            quota: environment.quota,
            storage: environment.storage,
            sync: environment.sync,
            clock: .fixed(environment.configuration.clock.now)
        )
    }
}
