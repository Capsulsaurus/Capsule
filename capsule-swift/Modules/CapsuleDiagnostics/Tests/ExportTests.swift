import CapsuleFoundation
import CapsuleTestSupport
import Foundation
import Testing

@testable import CapsuleDiagnostics

@Suite("Redaction & export")
struct ExportTests {
    @Test("Redactor strips paths, UUIDs, and long hex")
    func redactorStrips() {
        let path = "/Users/alice/Library/Application Support/Capsule/store.sqlite"
        let uuid = "B84E8479-475C-4727-A4A4-B77AA9980897"
        let hash = "deadbeefdeadbeefdeadbeefdeadbeef00112233"
        let output = Redactor.redact("opening \(path) id \(uuid) hash \(hash)")

        #expect(!output.contains("/Users/alice"))
        #expect(!output.contains(uuid))
        #expect(!output.contains(hash))
        #expect(output.contains(Redactor.placeholder))
    }

    @Test("exporter redacts log messages and leaks no planted secrets")
    func exporterRedacts() async throws {
        let secretPath = "/Users/alice/Library/Capsule/store.sqlite"
        let secretUUID = "B84E8479-475C-4727-A4A4-B77AA9980897"
        let reader = MockLogExcerptReader([
            DiagnosticsBundle.LogEntry(
                timestamp: Date(timeIntervalSince1970: 1),
                category: "managed-store",
                level: "info",
                message: "opening \(secretPath) id \(secretUUID)"
            ),
        ])
        let exporter = DefaultDiagnosticsExporter(
            logReader: reader,
            time: FixedTimeSource(now: Date(timeIntervalSince1970: 5))
        )
        let metadata = DeviceMetadata(
            appVersion: "1.0", appBuild: "1", systemName: "iOS", systemVersion: "18.0",
            model: "iPhone", locale: "en_US", freeDiskSpace: .ample
        )
        let crumb = BreadcrumbRing.Breadcrumb(name: "operation_failed", detail: "delete", timestamp: Date(timeIntervalSince1970: 2))

        let bundle = await exporter.exportBundle(metadata: metadata, breadcrumbs: [crumb], crash: nil)

        #expect(bundle.createdAt == Date(timeIntervalSince1970: 5))
        #expect(bundle.logExcerpt.first?.message.contains(secretPath) == false)
        #expect(bundle.logExcerpt.first?.message.contains(secretUUID) == false)

        let data = try bundle.jsonData()
        let json = try #require(String(bytes: data, encoding: .utf8))
        #expect(!json.contains(secretPath))
        #expect(!json.contains(secretUUID))
    }

    @Test("device metadata carries no stable identifiers")
    func metadataHasNoIdentifiers() throws {
        let metadata = DeviceMetadata(
            appVersion: "1.0", appBuild: "1", systemName: "iOS", systemVersion: "18.0",
            model: "iPhone", locale: "en_US", freeDiskSpace: .low
        )
        let data = try JSONEncoder().encode(metadata)
        let json = try #require(String(bytes: data, encoding: .utf8)).lowercased()
        for forbidden in ["idfv", "idfa", "udid", "identifierforvendor", "serial"] {
            #expect(!json.contains(forbidden))
        }
    }

    @Test("disk space buckets by threshold")
    func diskBuckets() {
        #expect(DiskSpaceBucket.bucket(forBytes: 100_000_000) == .critical)
        #expect(DiskSpaceBucket.bucket(forBytes: 1_000_000_000) == .low)
        #expect(DiskSpaceBucket.bucket(forBytes: 5_000_000_000) == .moderate)
        #expect(DiskSpaceBucket.bucket(forBytes: 50_000_000_000) == .ample)
    }

    @Test("bundle round-trips through Codable")
    func bundleCodable() throws {
        let bundle = makeBundle()
        let data = try bundle.jsonData()
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let decoded = try decoder.decode(DiagnosticsBundle.self, from: data)
        #expect(decoded == bundle)
    }

    @Test("OSLogExcerptReader completes without throwing")
    func osLogReaderSmoke() async {
        CapsuleLog.diagnostics.info("diagnostics export smoke marker")
        // Environment-dependent (the store may be empty in CI): assert only that
        // the read completes and returns a bounded result.
        let entries = await OSLogExcerptReader().recentEntries(within: 60, limit: 50)
        #expect(entries.count <= 50)
    }
}
