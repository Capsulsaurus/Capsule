import CapsuleDomain
import Testing

@testable import FeatureImport

/// The three space verdicts, and the arithmetic behind the sentence the red one
/// puts on screen.
@Suite("Import space outlook")
struct ImportSpaceOutlookTests {
    private static let gigabyte: UInt64 = 1073741824
    private static let reserve = ImportSpaceOutlook.defaultReserveBytes

    @Test("a small plan on a big disk is comfortable")
    func comfortable() {
        let outlook = ImportSpaceOutlook.assess(
            requiredBytes: 4 * Self.gigabyte,
            availableBytes: 92 * Self.gigabyte
        )

        #expect(outlook.state == .comfortable)
        #expect(outlook.shortfallBytes == 0)
        #expect(outlook.permitsImport)
    }

    /// Past half the usable headroom the run leaves the volume with less slack
    /// than it consumed, which is where release-as-you-go stops being an
    /// optimisation.
    @Test("a plan over half the usable headroom recommends streaming")
    func streamingRecommended() {
        let available = 20 * Self.gigabyte
        let usable = available - Self.reserve
        let outlook = ImportSpaceOutlook.assess(
            requiredBytes: usable / 2 + 1,
            availableBytes: available
        )

        #expect(outlook.state == .streamingRecommended)
        #expect(outlook.shortfallBytes == 0)
        #expect(outlook.permitsImport)
    }

    @Test("exactly half the usable headroom is still comfortable")
    func halfIsStillComfortable() {
        let available = 20 * Self.gigabyte
        let outlook = ImportSpaceOutlook.assess(
            requiredBytes: (available - Self.reserve) / 2,
            availableBytes: available
        )

        #expect(outlook.state == .comfortable)
    }

    /// The severe case has to name the exact shortfall: "not enough space" is
    /// not actionable and "free 4.1 GB" is.
    @Test("a plan that does not fit reports the exact shortfall")
    func insufficientReportsShortfall() {
        let available = 10 * Self.gigabyte
        let usable = available - Self.reserve
        let outlook = ImportSpaceOutlook.assess(
            requiredBytes: usable + 4 * Self.gigabyte,
            availableBytes: available
        )

        #expect(outlook.state == .insufficient)
        #expect(outlook.shortfallBytes == 4 * Self.gigabyte)
        #expect(!outlook.permitsImport)
    }

    /// The reserve is headroom deliberately left unspent, so a plan that would
    /// fit only by filling the volume to zero does not.
    @Test("the reserve is never spent")
    func reserveIsHeldBack() {
        let available = 5 * Self.gigabyte
        let outlook = ImportSpaceOutlook.assess(requiredBytes: available, availableBytes: available)

        #expect(outlook.state == .insufficient)
        #expect(outlook.shortfallBytes == Self.reserve)
    }

    /// A warning drawn from a number nobody has is a warning users learn to
    /// ignore.
    @Test("an unmeasurable disk does not block the import")
    func unknownAvailableIsComfortable() {
        let outlook = ImportSpaceOutlook.assess(requiredBytes: 500 * Self.gigabyte, availableBytes: nil)

        #expect(outlook.state == .comfortable)
        #expect(outlook.permitsImport)
        #expect(outlook.fractionOfAvailable == 0)
    }

    @Test("the meter fraction is clamped to one")
    func fractionIsClamped() {
        let outlook = ImportSpaceOutlook.assess(
            requiredBytes: 400 * Self.gigabyte,
            availableBytes: 10 * Self.gigabyte
        )

        #expect(outlook.fractionOfAvailable == 1)
    }

    @Test("a volume smaller than the reserve floors rather than trapping")
    func tinyVolumeDoesNotTrap() {
        let outlook = ImportSpaceOutlook.assess(requiredBytes: Self.gigabyte, availableBytes: 1024)

        #expect(outlook.state == .insufficient)
        #expect(outlook.shortfallBytes == Self.gigabyte)
    }

    @Test("each verdict carries its own tone")
    func tonesAreDistinct() {
        #expect(ImportSpaceOutlook.State.comfortable.tone == .positive)
        #expect(ImportSpaceOutlook.State.streamingRecommended.tone == .caution)
        #expect(ImportSpaceOutlook.State.insufficient.tone == .critical)
    }
}
