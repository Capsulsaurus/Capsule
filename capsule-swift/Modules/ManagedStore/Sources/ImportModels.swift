import Foundation

/// One file queued for import — a temporary file produced by the photo picker.
public struct ImportSource: Sendable, Equatable {
    /// The location of the file to import.
    public var url: URL
    /// The file's display name, recorded in the sidecar.
    public var originalFilename: String

    public init(url: URL, originalFilename: String) {
        self.url = url
        self.originalFilename = originalFilename
    }
}

/// One source file that could not be imported.
public struct ImportFailure: Sendable, Equatable {
    /// The file's display name.
    public var filename: String
    /// A human-readable reason.
    public var reason: String

    public init(filename: String, reason: String) {
        self.filename = filename
        self.reason = reason
    }
}

/// The outcome of an import run.
public struct ImportResult: Sendable, Equatable {
    /// The catalog UUIDs of newly imported assets.
    public var importedAssetIDs: [String]
    /// The names of files skipped because their content was already imported.
    public var duplicateFilenames: [String]
    /// The files that failed, with reasons.
    public var failures: [ImportFailure]

    public init(
        importedAssetIDs: [String] = [],
        duplicateFilenames: [String] = [],
        failures: [ImportFailure] = []
    ) {
        self.importedAssetIDs = importedAssetIDs
        self.duplicateFilenames = duplicateFilenames
        self.failures = failures
    }

    /// The number of assets newly imported.
    public var importedCount: Int { importedAssetIDs.count }
    /// The number of files skipped as duplicates.
    public var duplicateCount: Int { duplicateFilenames.count }
    /// The number of files that failed.
    public var failureCount: Int { failures.count }

    /// Whether nothing at all was processed.
    public var isEmpty: Bool {
        importedAssetIDs.isEmpty && duplicateFilenames.isEmpty && failures.isEmpty
    }
}
