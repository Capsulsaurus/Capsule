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

    /// Local-alert refresh intervals (facts, predicate, timer reconcile).
    public static let alerts = OSSignposter(
        subsystem: CapsuleLog.subsystem,
        category: "alerts"
    )

    /// Input-to-frame intervals: one `inputToFrame` interval per tap, begun when the tap is
    /// delivered and ended once the frame answering it has been committed.
    ///
    /// The parity benchmark reads them with `XCTOSSignpostMetric(subsystem:category:name:)`
    /// using this subsystem, the `input` category and `inputToFrameInterval`; renaming any of
    /// the three without the benchmark leaves the measure with no samples, which its import refuses.
    public static let inputToFrame = OSSignposter(
        subsystem: CapsuleLog.subsystem,
        category: "input"
    )

    /// The name of every `inputToFrame` interval.
    public static let inputToFrameInterval: StaticString = "inputToFrame"
}
