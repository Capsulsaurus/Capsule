import CoreGraphics
import Foundation

/// The column ladder a pinch moves the photo grid along, and the arithmetic that
/// turns a live gesture scale into a rung.
///
/// Pure, so the feel of the gesture is settled by tests rather than by pinching a
/// simulator and deciding it seems about right.
///
/// ## Which way is "in"
///
/// Spreading two fingers makes the thing under them *bigger*, which for a grid
/// means **fewer, larger tiles**. So a rising scale lowers the column count. This
/// is the opposite of the naive reading and is the whole reason the conversion
/// lives behind a named function instead of inline at two call sites.
public enum PhotoGridZoom {
    /// The column counts the grid is allowed to rest at, finest tiles last.
    ///
    /// Not a continuous range. Half a column cannot be drawn, and a grid that
    /// settled anywhere would leave a different ragged edge every time it was
    /// touched — the rungs are what make the gesture repeatable. The spacing
    /// widens at the coarse end because the *proportional* change is what reads
    /// as a step: 2→3 tiles is already a 50% size change, while 7→8 is barely
    /// visible, so the ladder skips to 10.
    public static let ladder = [2, 3, 4, 5, 7, 10]

    /// The default resting rung, used when nothing is stored.
    public static let defaultColumns = 5

    /// The continuous column count a pinch of `scale` implies, starting from
    /// `base` columns.
    ///
    /// Deliberately unclamped: the caller needs to see that the gesture has run
    /// past the end of the ladder in order to hand off to a coarser aggregation
    /// level, and clamping here would hide exactly that.
    public static func continuousColumns(base: Int, scale: CGFloat) -> CGFloat {
        guard scale > 0 else { return CGFloat(base) }
        return CGFloat(max(1, base)) / scale
    }

    /// The ladder rung nearest a continuous column count.
    ///
    /// Nearest in the *proportional* sense — the ratio between the candidate and
    /// the value, not their difference.
    ///
    /// A reader perceives tile size proportionally: 7→10 columns shrinks a tile
    /// by 30%, the same step as 5→7, even though one gap is 3 columns and the
    /// other is 2. Under subtraction the wide gap at the coarse end would
    /// therefore behave differently from the tight ones at the fine end — the
    /// ladder would feel sticky at 7 and skittish at 3.
    public static func settle(_ continuous: CGFloat) -> Int {
        guard continuous.isFinite else { return defaultColumns }
        let clamped = max(CGFloat(ladder[0]), min(CGFloat(ladder[ladder.count - 1]), continuous))
        return ladder.min { lhs, rhs in
            ratioDistance(CGFloat(lhs), clamped) < ratioDistance(CGFloat(rhs), clamped)
        } ?? defaultColumns
    }

    /// How far apart two column counts are, proportionally.
    private static func ratioDistance(_ lhs: CGFloat, _ rhs: CGFloat) -> CGFloat {
        guard lhs > 0, rhs > 0 else { return .greatestFiniteMagnitude }
        return abs(log(lhs / rhs))
    }

    /// A pinch that has run off the end of the ladder, as an aggregation-level
    /// step: `false` to go coarser (Months), `true` to go finer.
    ///
    /// Returns `nil` while the gesture is still inside the ladder, which is the
    /// overwhelmingly common case.
    ///
    /// Only the coarse end hands off. Past the fine end there is nothing finer
    /// than the largest tile the ladder draws — the next thing after that is the
    /// viewer, and opening it out of a pinch would mean a stray gesture on the
    /// grid could take over the screen.
    public static func levelStep(base: Int, scale: CGFloat) -> Bool? {
        let continuous = continuousColumns(base: base, scale: scale)
        guard let coarsest = ladder.last else { return nil }
        // A margin past the last rung, so resting *at* the coarsest density does
        // not tip into another level on a trembling finger.
        return continuous > CGFloat(coarsest) * levelHandoffMargin ? false : nil
    }

    /// How far past the coarsest rung a pinch must go before it changes level.
    static let levelHandoffMargin: CGFloat = 1.35

    /// The rung one step from `columns`, for the keyboard and menu paths that
    /// step rather than pinch.
    public static func stepped(from columns: Int, finer: Bool) -> Int {
        let index = ladder.firstIndex(of: columns) ?? ladder.firstIndex(of: defaultColumns) ?? 0
        let next = finer ? index + 1 : index - 1
        return ladder[max(0, min(ladder.count - 1, next))]
    }
}
