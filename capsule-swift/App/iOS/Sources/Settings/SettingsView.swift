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
                    Toggle("ios.settings.diagnostics.toggle", isOn: diagnosticsBinding)
                } header: {
                    Text("ios.settings.diagnostics.header")
                } footer: {
                    Text("ios.settings.diagnostics.footer")
                }

                Section {
                    Toggle("ios.settings.upload.toggle", isOn: uploadBinding)
                    if consent.remoteUploadEnabled {
                        TextField("https://your-server/v1/telemetry", text: $endpointText)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .keyboardType(.URL)
                            .onSubmit(saveEndpoint)
                            .submitLabel(.done)
                    }
                } header: {
                    Text("ios.settings.upload.header")
                } footer: {
                    Text("ios.settings.upload.footer")
                }

                Section {
                    Button("ios.settings.report.button") {
                        Task { await buildReport() }
                    }
                    .disabled(isBuildingReport)
                } footer: {
                    Text("ios.settings.report.footer")
                }

                Section {
                    LabeledContent("ios.settings.version", value: appVersion)
                }
            }
            .navigationTitle("ios.tab.settings")
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

    private func persist(_ transform: @escaping @Sendable (inout DiagnosticsConsent) -> Void) {
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
