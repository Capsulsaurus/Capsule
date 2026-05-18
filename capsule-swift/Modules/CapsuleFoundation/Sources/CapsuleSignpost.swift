import OSLog

/// `OSSignposter` instances for the app's performance-critical paths.
///
/// Signposts let Instruments (Time Profiler, Animation Hitches, the custom
/// `os_signpost` instrument) attribute time to named intervals — thumbnail
/// decodes, catalog queries, and import phases — so regressions are visible
/// in a trace rather than guessed at.
public enum CapsuleSignpost {
    /// Thumbnail decode and prefetch intervals.
    public static let imagePipeline = OSSignposter(
        subsystem: CapsuleLog.subsystem,
        category: "image-pipeline"
    )

    /// SQLite catalog query intervals.
    public static let catalog = OSSignposter(
        subsystem: CapsuleLog.subsystem,
        category: "catalog"
    )

    /// Import-pipeline intervals.
    public static let importPipeline = OSSignposter(
        subsystem: CapsuleLog.subsystem,
        category: "import"
    )
}
