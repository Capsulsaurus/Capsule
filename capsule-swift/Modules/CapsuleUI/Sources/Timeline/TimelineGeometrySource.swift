import CoreGraphics
import Foundation

// MARK: - TimelineGeometrySource

/// The geometry questions a collection-view layout asks of a timeline.
///
/// ``TimelineLayout`` answers every one of them already; this protocol exists so
/// the attribute maths in ``TimelineAttributeSolver`` can be written against the
/// *queries* rather than against the concrete type. Two things fall out of that:
///
/// - The solver compiles and is tested without UIKit or AppKit, so the same
///   arithmetic that drives the iPhone is verified on a Mac test run.
/// - A test can substitute a **counting** source and assert the thing that
///   actually matters about this layout — that answering a viewport costs one
///   query per *visible* item and none per item in the library. That claim is
///   not observable from the output alone, and an unverified claim about a
///   250 000-item grid is the one most likely to quietly stop being true.
///
/// Every requirement here is a method ``TimelineLayout`` already vends, so the
/// conformance below adds no behaviour — only the two aggregate accessors a
/// layout object needs and the value type spells differently.
public protocol TimelineGeometrySource {
    /// How many sections the timeline holds.
    var sectionCount: Int { get }
    /// How many items it holds in total.
    var itemCount: Int { get }
    /// The exact content height, known before anything is fetched.
    var totalContentHeight: CGFloat { get }
    /// The height one section header reserves; `0` when headers are hidden.
    var headerHeight: CGFloat { get }
    /// The vertical gap between one section's last row and the next header.
    var sectionSpacing: CGFloat { get }

    /// How many items `section` holds.
    func itemCount(inSection section: Int) -> Int
    /// The global item indices intersecting `rect` — the binary search.
    func indexRange(intersecting rect: CGRect) -> Range<Int>
    /// The frame of one item, or `nil` when the index is out of range.
    func frame(forGlobalIndex globalIndex: Int) -> CGRect?
    /// The section containing `globalIndex`, or `nil` when out of range.
    func sectionIndex(forGlobalIndex globalIndex: Int) -> Int?
    /// The global index of `section`'s first item.
    func firstGlobalIndex(inSection section: Int) -> Int?
    /// The section whose vertical span contains `offsetY`, clamped to the ends.
    func sectionIndex(atOffset offsetY: CGFloat) -> Int
    /// The y offset of `section`'s header, or `nil` when out of range.
    func offset(forSection section: Int) -> CGFloat?
}

// MARK: - TimelineLayout conformance

extension TimelineLayout: TimelineGeometrySource {
    public var sectionCount: Int { sections.count }

    public var headerHeight: CGFloat { metrics.headerHeight }

    public var sectionSpacing: CGFloat { metrics.sectionSpacing }

    public func itemCount(inSection section: Int) -> Int {
        guard section >= 0, section < sections.count else { return 0 }
        return sections[section].count
    }
}

// MARK: - Derived spans

public extension TimelineGeometrySource {
    /// The y offset just below `section`'s last row, excluding the inter-section
    /// gap.
    ///
    /// The gap belongs to neither section visually, so a header pinned into it
    /// would hang below its own content. Deriving the bottom from the *next*
    /// section's offset keeps that arithmetic in one place.
    func contentBottom(ofSection section: Int) -> CGFloat {
        guard section >= 0, section < sectionCount else { return 0 }
        guard let next = offset(forSection: section + 1) else { return totalContentHeight }
        return next - sectionSpacing
    }
}
