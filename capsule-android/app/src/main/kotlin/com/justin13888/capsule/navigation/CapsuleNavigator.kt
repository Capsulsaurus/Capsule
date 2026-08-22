package com.justin13888.capsule.navigation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.remember
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.rememberNavBackStack

/**
 * The only thing a screen is allowed to know about navigation.
 *
 * Screens receive this, never the back stack. The point is the seam: a feature module can be
 * handed lambdas built from these four calls without ever seeing `androidx.navigation3`, so
 * swapping a placeholder for a real screen never becomes a navigation change. It also keeps
 * the back stack single-writer, which is what makes the adaptive layouts safe — a pane
 * scaffold and the navigation suite would otherwise both mutate it.
 */
@Stable
interface CapsuleNavigator {
    /**
     * The live back stack, for the one consumer that needs the whole list: `NavDisplay`.
     *
     * Read-only by type. The shell renders it; only this navigator writes it, which is what
     * keeps the navigation suite and the pane scaffolds from fighting over the same list.
     */
    val entries: List<NavKey>

    /** What is on top right now. */
    val current: CapsuleDestination

    /** The top-level section [current] belongs to, which is what the navigation suite marks. */
    val section: CapsuleDestination.TopLevel

    /** Whether anything can be popped — false only at a section root. */
    val canGoBack: Boolean

    /** Push a destination on top of the current one. */
    fun goTo(destination: CapsuleDestination)

    /** Pop one destination. Returns false at a section root, where the system handles back. */
    fun goBack(): Boolean

    /**
     * Move to a top-level section.
     *
     * Re-selecting the current section rewinds it to its root rather than doing nothing; that
     * is the behaviour a navigation bar has trained users to expect, and it is the only way
     * out of a deep stack on a compact window without repeated back presses.
     */
    fun switchTo(section: CapsuleDestination.TopLevel)
}

/**
 * Creates the navigator and the saved back stack behind it.
 *
 * The back stack survives process death because every [CapsuleDestination] is `@Serializable`
 * and [rememberNavBackStack] persists it by reflection over that annotation.
 */
@Composable
fun rememberCapsuleNavigator(start: CapsuleDestination.TopLevel = CapsuleDestination.start): CapsuleNavigator {
    val backStack = rememberNavBackStack(start)
    return remember(backStack) { BackStackNavigator(backStack) }
}

/**
 * A [CapsuleNavigator] over one flat Navigation 3 back stack.
 *
 * The stack is flat — element 0 is always the current section's root — rather than one stack
 * per section. Per-section stacks would preserve more state, but they also make "where does
 * back go" depend on invisible history, and this app's deep destinations (a viewer, a share
 * link, a settings sub-page) are reached from a listing the user can see. A flat stack keeps
 * back predictable, and the section root is cheap to rebuild.
 */
private class BackStackNavigator(
    private val backStack: NavBackStack<NavKey>,
) : CapsuleNavigator {
    override val entries: List<NavKey>
        get() = backStack

    override val current: CapsuleDestination
        get() = backStack.last() as CapsuleDestination

    override val section: CapsuleDestination.TopLevel
        get() = backStack.first() as CapsuleDestination.TopLevel

    override val canGoBack: Boolean
        get() = backStack.size > 1

    override fun goTo(destination: CapsuleDestination) {
        // Re-navigating to what is already on top is a double tap, not a new destination.
        if (backStack.lastOrNull() == destination) return
        backStack.add(destination)
    }

    override fun goBack(): Boolean {
        if (!canGoBack) return false
        backStack.removeAt(backStack.lastIndex)
        return true
    }

    override fun switchTo(section: CapsuleDestination.TopLevel) {
        backStack.clear()
        backStack.add(section)
    }
}
