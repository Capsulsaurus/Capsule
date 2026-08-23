import SwiftUI

/// Shared design tokens — spacing, corner radii, and semantic colours — so the
/// app stops hard-coding magic numbers inline and stays visually consistent as
/// the Liquid Glass surfaces multiply.
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

    /// Semantic colours layered on the system palette.
    public enum Colors {
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
