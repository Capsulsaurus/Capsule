import SwiftUI

/// Shared design tokens — spacing, corner radii, type, and semantic colours —
/// so the app stops hard-coding magic numbers inline and stays visually
/// consistent as the Liquid Glass surfaces multiply.
public enum CapsuleTheme {
    /// 4-pt spacing scale.
    public enum Spacing {
        public static let xxSmall: CGFloat = 2
        public static let xSmall: CGFloat = 4
        public static let small: CGFloat = 8
        public static let medium: CGFloat = 12
        public static let large: CGFloat = 16
        public static let xLarge: CGFloat = 24
        public static let xxLarge: CGFloat = 32
    }

    /// Corner radii for cards, sheets, and floating glass surfaces.
    public enum Radius {
        public static let small: CGFloat = 8
        public static let medium: CGFloat = 16
        public static let large: CGFloat = 22
        public static let card: CGFloat = 12
    }

    /// The type scale, as **roles** rather than as sizes.
    ///
    /// Every value resolves to a system text style, so Dynamic Type, the bold-text
    /// accessibility setting, and the platform's own metrics all keep working —
    /// this names *what a piece of text is for*, not how big it should be. A
    /// fixed point size would break all three, which is why there are none here.
    ///
    /// The scale exists because the app previously spelled its fonts inline at
    /// every call site (`.font(.caption2.weight(.semibold))` and forty variants
    /// of it), so the same role was drawn four different ways on four screens and
    /// nothing could be changed in one place.
    public enum Typography {
        /// A screen's own title, where it is drawn in content rather than in a
        /// navigation bar.
        public static let screenTitle = Font.largeTitle.weight(.bold)
        /// The heading of a card or a group of rows.
        public static let sectionTitle = Font.headline
        /// A card's primary line — the one thing it is about.
        public static let cardTitle = Font.subheadline.weight(.semibold)
        /// Running body copy and a row's primary value.
        public static let body = Font.body
        /// A row's label, next to the value it names.
        public static let rowLabel = Font.subheadline
        /// Secondary detail beneath a title.
        public static let detail = Font.footnote
        /// A badge, chip, or overlay stamped on top of media.
        public static let badge = Font.caption2.weight(.semibold)
        /// Explanatory copy under a control.
        public static let caption = Font.caption
        /// A control glyph in a toolbar or a floating bar.
        public static let controlGlyph = Font.title3
        /// Figures meant to be compared down a column — durations, sizes, counts.
        ///
        /// Monospaced *digits* rather than a monospaced face: the text around the
        /// number stays proportional and readable, while the digits themselves
        /// stop jittering as they tick.
        public static let numeric = Font.footnote.monospacedDigit()
    }

    /// Semantic colours layered on the system palette.
    public enum Colors {
        /// The app's accent, applied once at the scene root.
        ///
        /// `Color.accentColor` resolves the asset catalog's `AccentColor`, which
        /// is what the platform already tints its own controls with — so naming
        /// it here changes nothing today and gives the brand accent exactly one
        /// place to land when it is chosen.
        public static let accent = Color.accentColor
        /// The tint for a favourited asset (the filled heart).
        public static let favorite = Color.red
        /// Foreground for controls floating over photo content.
        public static let onMedia = Color.white

        /// The hairline around a photo card that floats over other content.
        ///
        /// A near-white rather than pure white, and never a system separator:
        /// this border's whole job is to hold a photograph apart from whatever
        /// is behind it — a map, another photo in a fanned stack — and a
        /// semantic separator colour inverts in dark mode, which is exactly
        /// when a card over a dark map needs a *light* edge most.
        public static let cardBorder = Color(white: 0.96)
    }

    /// Widths for the hairlines the theme owns.
    public enum Stroke {
        /// The card hairline. Thin enough to read as an edge rather than a
        /// frame at any Dynamic Type size.
        public static let hairline: CGFloat = 1.5
    }
}
