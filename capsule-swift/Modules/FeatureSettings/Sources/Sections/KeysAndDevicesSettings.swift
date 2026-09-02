import CapsuleDomain
import CapsuleMock
import CapsuleNavigation
import CapsulePorts
import Observation
import SwiftUI

// MARK: - KeysAndDevicesSettingsModel

/// Drives the Keys & Devices screen: the device directory, the advisory cohort
/// map, and enrollment.
///
/// Two contract details drive what is on screen. A revoked device is
/// **listed, not hidden** — "its directory entry is retained — marked with
/// `revoked_at`, never deleted" — because everything it signed stays verifiable
/// and a user auditing their account should see the same history the
/// cryptography does. And a cohort hash is advisory: the client "asserts, it
/// does not litigate", so there is no "this isn't my device" control, only a
/// support bundle.
@MainActor
@Observable
public final class KeysAndDevicesSettingsModel {
    public private(set) var phase: SettingsPhase = .loading
    public private(set) var devices: [DeviceRecord] = []
    public private(set) var cohorts: [DeviceCohort] = []
    public private(set) var enrollmentCode: EnrollmentCode?
    public private(set) var isWorking = false

    private let devicePort: any DevicePort
    private let enrollment: any EnrollmentPort
    private let connectivity: SettingsConnectivity

    public init(
        devices devicePort: any DevicePort,
        enrollment: any EnrollmentPort,
        connectivity: SettingsConnectivity
    ) {
        self.devicePort = devicePort
        self.enrollment = enrollment
        self.connectivity = connectivity
    }

    public func load() async {
        phase = .loading
        do {
            devices = try await devicePort.devices()
            cohorts = try await devicePort.cohorts()
            phase = devices.isEmpty ? .empty : .ready
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }

    /// Active devices first, then revoked ones — present, dimmed, and dated.
    public var orderedDevices: [DeviceRecord] {
        devices.sorted { lhs, rhs in
            if lhs.isActive != rhs.isActive { return lhs.isActive }
            return lhs.lastSeen > rhs.lastSeen
        }
    }

    /// Whether this platform's cohort survives a factory reset.
    ///
    /// Only macOS does. The honest promise is reinstall-stable everywhere and
    /// reset-stable only where the OS allows, and the screen says which one the
    /// user is getting rather than implying the stronger one.
    public func cohortSurvivesReset(_ record: DeviceRecord) -> Bool {
        record.platform.cohortSurvivesFactoryReset
    }

    /// Issue an enrollment code from this already-enrolled device.
    ///
    /// Requires fresh local authentication inside the port: a valid session
    /// token alone cannot start a cross-device add, so a stale stolen token
    /// cannot enroll a rogue device.
    public func issueEnrollmentCode() async {
        await perform { self.enrollmentCode = try await self.enrollment.issueEnrollmentCode() }
    }

    public func cancelEnrollment() async {
        guard let code = enrollmentCode else { return }
        await perform {
            try await self.enrollment.cancelEnrollment(channelHandle: code.channelHandle)
            self.enrollmentCode = nil
        }
    }

    public func revoke(_ id: DeviceID) async {
        await perform {
            try await self.devicePort.revokeDevice(id)
            self.devices = try await self.devicePort.devices()
        }
    }

    private func perform(_ work: @escaping () async throws -> Void) async {
        isWorking = true
        defer { isWorking = false }
        do {
            try await work()
        } catch {
            phase = await connectivity.phase(for: error)
        }
    }
}

// MARK: - KeysAndDevicesSettingsView

/// Keys & Devices — the directory, cohorts, and enrollment.
public struct KeysAndDevicesSettingsView: View {
    @State private var model: KeysAndDevicesSettingsModel
    @State private var devicePendingRevocation: DeviceID?

    public init(model: KeysAndDevicesSettingsModel) {
        _model = State(initialValue: model)
    }

    public init(environment: SettingsEnvironment) {
        self.init(
            model: KeysAndDevicesSettingsModel(
                devices: environment.devices,
                enrollment: environment.enrollment,
                connectivity: environment.connectivity
            )
        )
    }

    public var body: some View {
        SettingsScreen(
            titleKey: SettingsSection.keysAndDevices.titleKey,
            phase: model.phase,
            emptyTitleKey: "app.settings.keys.empty.title",
            emptyDescriptionKey: "app.settings.keys.empty.description",
            retry: { await model.load() },
            content: {
                enrollmentSection
                devicesSection
                cohortsSection
                rotationSection
            }
        )
        .task { await model.load() }
        .settingsDestructiveConfirmation(
            titleKey: "app.settings.keys.revoke.confirm.title",
            messageKey: "app.settings.keys.revoke.confirm.message",
            confirmKey: "app.settings.keys.revoke.confirm.action",
            isPresented: revocationPresented
        ) {
            if let identifier = devicePendingRevocation {
                await model.revoke(identifier)
            }
            devicePendingRevocation = nil
        }
    }

    private var revocationPresented: Binding<Bool> {
        Binding(
            get: { devicePendingRevocation != nil },
            set: { presented in if !presented { devicePendingRevocation = nil } }
        )
    }

    private var enrollmentSection: some View {
        Section {
            if let code = model.enrollmentCode {
                SettingsValueRow(labelKey: "app.settings.keys.enroll.code", value: code.code)
                SettingsValueRow(
                    labelKey: "app.settings.keys.enroll.expires",
                    value: SettingsFormat.timestamp(code.expiresAt)
                )
                Button("app.settings.keys.enroll.cancel", role: .cancel) {
                    Task { await model.cancelEnrollment() }
                }
            } else {
                Button("app.settings.keys.enroll.issue") {
                    Task { await model.issueEnrollmentCode() }
                }
                .disabled(model.isWorking)
            }
        } header: {
            Text("app.settings.keys.enroll.header")
        } footer: {
            Text("app.settings.keys.enroll.footer")
        }
    }

    private var devicesSection: some View {
        Section {
            ForEach(model.orderedDevices) { record in
                deviceRows(record)
            }
        } header: {
            Text("app.settings.keys.devices.header")
        } footer: {
            Text("app.settings.keys.devices.footer")
        }
    }

    @ViewBuilder
    private func deviceRows(_ record: DeviceRecord) -> some View {
        SettingsStatusRow(
            labelKey: record.isCurrent
                ? "app.settings.keys.devices.this_device"
                : "app.settings.keys.devices.device",
            statusKey: record.isActive
                ? "app.settings.keys.devices.active"
                : "app.settings.keys.devices.revoked",
            tone: record.isActive ? .positive : .neutral
        )
        SettingsValueRow(labelKey: "app.settings.keys.devices.model", value: record.model)
        SettingsValueRow(
            labelKey: "app.settings.keys.devices.added",
            value: SettingsFormat.day(record.firstSeen)
        )
        SettingsValueRow(
            labelKey: "app.settings.keys.devices.last_seen",
            value: SettingsFormat.timestamp(record.lastSeen)
        )
        if let revoked = record.revokedAt {
            SettingsValueRow(
                labelKey: "app.settings.keys.devices.revoked_at",
                value: SettingsFormat.day(revoked)
            )
        } else if !record.isCurrent {
            Button("app.settings.keys.devices.revoke", role: .destructive) {
                devicePendingRevocation = record.id
            }
        }
    }

    private var cohortsSection: some View {
        Section {
            if model.cohorts.isEmpty {
                SettingsNoteRow(textKey: "app.settings.keys.cohorts.none")
            }
            ForEach(model.cohorts) { cohort in
                SettingsValueRow(
                    labelKey: "app.settings.keys.cohorts.hash",
                    value: SettingsFormat.shortIdentifier(cohort.cohortHash, length: 12)
                )
                SettingsStatusRow(
                    labelKey: "app.settings.keys.cohorts.seen_before",
                    statusKey: cohort.isPreviouslySeen
                        ? "app.settings.keys.cohorts.yes"
                        : "app.settings.keys.cohorts.no",
                    tone: .neutral
                )
                SettingsValueRow(
                    labelKey: "app.settings.keys.cohorts.last_seen",
                    value: SettingsFormat.timestamp(cohort.lastSeen)
                )
            }
        } header: {
            Text("app.settings.keys.cohorts.header")
        } footer: {
            Text("app.settings.keys.cohorts.footer")
        }
    }

    private var rotationSection: some View {
        Section {
            SettingsNoteRow(textKey: "app.settings.keys.rotation.body")
        } header: {
            Text("app.settings.keys.rotation.header")
        } footer: {
            Text("app.settings.keys.rotation.footer")
        }
    }
}

// MARK: - Preview

#Preview("Keys & Devices") {
    NavigationStack {
        KeysAndDevicesSettingsView(environment: .preview())
    }
}

#Preview("Keys & Devices — Dark") {
    NavigationStack {
        KeysAndDevicesSettingsView(environment: .preview())
    }
    .preferredColorScheme(.dark)
}
