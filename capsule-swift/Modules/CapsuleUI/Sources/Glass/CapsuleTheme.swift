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
    }
}
