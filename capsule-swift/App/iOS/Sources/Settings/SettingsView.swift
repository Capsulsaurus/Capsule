import CapsuleDiagnostics
import CapsuleFoundation
import SwiftUI

/// Privacy & Diagnostics settings: consent toggles, an optional self-hosted
/// upload endpoint, and a user-initiated "Report a Problem" flow.
///
/// Local on-device diagnostics are on by default; uploads are strictly opt-in.
/// The report is assembled by the ``DiagnosticsCoordinator`` (redacted) and
/// shared via the system share sheet.
struct SettingsView: View {
    let consentStore: ConsentStore
    let diagnostics: DiagnosticsCoordinator

    @State private var consent: DiagnosticsConsent = .privacyDefault
    @State private var endpointText = ""
    @State private var report: DiagnosticsReport?
    @State private var isBuildingReport = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    Toggle("Share On-Device Diagnostics", isOn: diagnosticsBinding)
                } header: {
                    Text("Diagnostics")
                } footer: {
                    Text("Collects crash and performance diagnostics on this device using Apple's MetricKit. Nothing leaves your device unless you turn on uploads below.")
                }

                Section {
                    Toggle("Upload Reports", isOn: uploadBinding)
                    if consent.remoteUploadEnabled {
                        TextField("https://your-server/v1/telemetry", text: $endpointText)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .keyboardType(.URL)
                            .onSubmit(saveEndpoint)
                            .submitLabel(.done)
                    }
                } header: {
                    Text("Self-Hosted Upload")
                } footer: {
                    Text("Optionally send diagnostic reports to your own Capsule server. Off by default — Capsule never sends anything to a third party.")
                }

                Section {
                    Button("Report a Problem…") {
                        Task { await buildReport() }
                    }
                    .disabled(isBuildingReport)
                } footer: {
                    Text("Builds a redacted report — recent logs, device info, and the last crash. No photos or album contents are included. You choose where to send it.")
                }

                Section {
                    LabeledContent("Version", value: appVersion)
                }
            }
            .navigationTitle("Settings")
            .task { await load() }
            .sheet(item: $report) { DiagnosticsReportView(report: $0) }
        }
    }

    // MARK: Bindings

    private var diagnosticsBinding: Binding<Bool> {
        Binding(
            get: { consent.diagnosticsEnabled },
            set: { newValue in
                consent.diagnosticsEnabled = newValue
                persist { $0.diagnosticsEnabled = newValue }
            }
        )
    }

    private var uploadBinding: Binding<Bool> {
        Binding(
            get: { consent.remoteUploadEnabled },
            set: { newValue in
                consent.remoteUploadEnabled = newValue
                persist { $0.remoteUploadEnabled = newValue }
            }
        )
    }

    // MARK: Actions

    private func saveEndpoint() {
        let trimmed = endpointText.trimmingCharacters(in: .whitespacesAndNewlines)
        let url = trimmed.isEmpty ? nil : URL(string: trimmed)
        consent.uploadEndpoint = url
        persist { $0.uploadEndpoint = url }
    }

    private func persist(_ transform: @escaping (inout DiagnosticsConsent) -> Void) {
        Task { await consentStore.update(transform) }
    }

    private func load() async {
        consent = await consentStore.current()
        endpointText = consent.uploadEndpoint?.absoluteString ?? ""
    }

    private func buildReport() async {
        isBuildingReport = true
        defer { isBuildingReport = false }
        let bundle = await diagnostics.makeReportBundle()
        if let data = try? bundle.jsonData() {
            report = DiagnosticsReport(data: data)
        }
    }

    private var appVersion: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "—"
        let build = info?["CFBundleVersion"] as? String ?? "—"
        return "\(version) (\(build))"
    }
}
