package com.justin13888.capsule.shell

import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteType
import androidx.window.core.layout.WindowSizeClass

/**
 * The three layouts this app is validated against: a phone, an unfolded foldable, a tablet.
 *
 * Material's own width classes go up to extra-large, but Capsule collapses everything at or
 * above the expanded breakpoint into one case on purpose. The shell's decisions — is there a
 * rail, is it expanded, can a detail pane sit beside a list — are all settled by 840dp; the
 * screens above that differ in how much *content* fits, which is the gallery engine's
 * problem, not the shell's. Three cases means three things to test rather than five.
 */
enum class CapsuleWindowClass {
    /** Below 600dp. A phone. One pane, bottom bar, viewer takes the whole window. */
    Compact,

    /** 600dp to 840dp. An unfolded foldable. Collapsed rail, list and detail side by side. */
    Medium,

    /** 840dp and up. A tablet. Expanded rail, list-detail plus a supporting pane. */
    Expanded,
    ;

    /**
     * Which navigation component the suite should render.
     *
     * Immersive destinations get [NavigationSuiteType.None] rather than a hidden-state
     * animation: the viewer and the onboarding steps are not "the app with its chrome
     * collapsed", they are screens that own the window, and letting the suite exist behind
     * them means it can reappear on a recomposition nobody asked for.
     */
    fun navigationSuiteType(immersive: Boolean): NavigationSuiteType =
        when {
            immersive -> NavigationSuiteType.None
            this == Compact -> NavigationSuiteType.ShortNavigationBarCompact
            this == Medium -> NavigationSuiteType.WideNavigationRailCollapsed
            else -> NavigationSuiteType.WideNavigationRailExpanded
        }

    companion object {
        /**
         * Buckets a measured window.
         *
         * Reads the breakpoint constants rather than [WindowSizeClass.windowWidthSizeClass],
         * which is deprecated in `window-core` 1.5 in favour of the breakpoint predicates.
         */
        fun of(sizeClass: WindowSizeClass): CapsuleWindowClass =
            when {
                sizeClass.isWidthAtLeastBreakpoint(WindowSizeClass.WIDTH_DP_EXPANDED_LOWER_BOUND) ->
                    Expanded

                sizeClass.isWidthAtLeastBreakpoint(WindowSizeClass.WIDTH_DP_MEDIUM_LOWER_BOUND) ->
                    Medium

                else -> Compact
            }
    }
}
