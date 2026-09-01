import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - ImportSourcePickerModel

/// Drives the source picker: which sources this device offers, which of them it
/// has already found, and what a tap resolves to.
///
/// The platform is injected rather than read from a `#if` in a view body, so the
/// rule that matters — macOS-only sources never appear on a handheld — is
/// assertable from an iPhone test run, which is exactly the run where a `#if`
/// would have compiled the assertion away.
@MainActor
@Observable
public final class ImportSourcePickerModel {
    public private(set) var phase: ImportPhase = .loading
    /// Every offered source, paired with whatever the device already found.
    public private(set) var rows: [ImportSourceRow] = []
    /// The scope the user settled on, once they have.
    public private(set) var selection: ImportScope?

    private let importing: any ImportPort
    private let connectivity: ImportConnectivity
    private let platform: ImportPlatform

    public init(
        importing: any ImportPort,
        connectivity: ImportConnectivity,
        platform: ImportPlatform
    ) {
        self.importing = importing
        self.connectivity = connectivity
        self.platform = platform
    }

    public convenience init(environment: ImportEnvironment) {
        self.init(
            importing: environment.importing,
            connectivity: environment.connectivity,
            platform: environment.platform
        )
    }

    /// Build the row list.
    ///
    /// A failure to enumerate discovered scopes is **not** a failure of the
    /// screen: the pickable sources — Files, a Takeout archive — do not depend
    /// on that call at all, and hiding them because a volume scan threw would
    /// leave a user with no way to import anything.
    public func load() async {
        phase = .loading
        let offered = ImportSourceOption.available(on: platform)
        let discovered = await (try? importing.availableScopes()) ?? []
        rows = offered.map { option in
            ImportSourceRow(option: option, discovered: discovered.first { $0.sourceKind == option.kind })
        }
        phase = rows.isEmpty ? .empty : .ready
    }

    /// Choose a row that already has a scope.
    ///
    /// - Returns: the scope to scan, or `nil` when the row needs a location
    ///   first.
    @discardableResult
    public func select(_ row: ImportSourceRow) -> ImportScope? {
        guard let scope = row.discovered, row.scansImmediately else { return nil }
        selection = scope
        return scope
    }

    /// Resolve a location the user pointed at into a scope.
    ///
    /// The scope id is computed by the port, never here: a Swift derivation
    /// would be a second, drift-prone source of a value two devices must agree
    /// on byte-for-byte.
    @discardableResult
    public func choose(_ row: ImportSourceRow, locator: String) async -> ImportScope? {
        do {
            let scope = try await importing.resolveScope(sourceKind: row.option.kind, locator: locator)
            selection = scope
            phase = .ready
            return scope
        } catch {
            phase = await connectivity.phase(for: error)
            return nil
        }
    }

    /// Forget the choice, so the picker can be shown again.
    public func clearSelection() {
        selection = nil
    }

    /// Whether an option is offered at all on this device.
    public func offers(_ kind: SourceKind) -> Bool {
        rows.contains { $0.option.kind == kind }
    }
}
