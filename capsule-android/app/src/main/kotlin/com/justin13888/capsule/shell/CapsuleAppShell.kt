package com.justin13888.capsule.shell

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Icon
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Text
import androidx.compose.material3.adaptive.currentWindowAdaptiveInfoV2
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteItem
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteScaffold
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteType
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.navigation3.ui.NavDisplay
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleEntryScope
import com.justin13888.capsule.navigation.CapsuleNavigator
import com.justin13888.capsule.navigation.rememberCapsuleEntryProvider
import com.justin13888.capsule.navigation.rememberCapsuleNavigator
import com.justin13888.capsule.security.LocalAuthGate
import com.justin13888.capsule.security.LocalAuthPrompt
import com.justin13888.capsule.security.rememberPlatformLocalAuthPrompt
import com.justin13888.capsule.shell.chrome.OfflineBanner
import com.justin13888.capsule.shell.chrome.ProtocolMismatchScreen

/** Test handle carrying the navigation component the shell chose: `shell.suite:<type>`. */
fun navigationSuiteTag(type: Any): String = "shell.suite:$type"

/** Test handle for one navigation item: `shell.nav-item:<destination>`. */
fun navigationItemTag(section: CapsuleDestination.TopLevel): String = "shell.nav-item:${section::class.simpleName}"

/**
 * The whole app, below the theme and above every screen.
 *
 * One shell for three window classes rather than three shells. Everything that differs
 * between a phone, an unfolded foldable and a tablet is a *value* here — which navigation
 * component the suite renders, how many partitions a pane scaffold may use, where the hinge
 * is — so there is one composition to reason about and one set of tests to write. The
 * alternative, branching on window class into separate trees, loses state on every fold and
 * makes "does back work on a tablet" a question nobody can answer from the code.
 *
 * Order of the layers matters and is deliberate:
 *
 * 1. A protocol refusal replaces *everything*. A `426` means this build cannot write to this
 *    server, so leaving the navigation up would offer the user nothing but actions that fail.
 * 2. The offline banner sits above the content, not over it. Offline is a condition the
 *    library reads fine through, so it is announced and then ignored.
 * 3. The navigation suite wraps the destination, except on immersive destinations, where it
 *    is dropped entirely.
 *
 * @param shellState the app-wide conditions: connectivity, protocol refusal, messages.
 * @param gate fresh-local-auth grants for Trash and Hidden.
 * @param prompt the platform authenticator. Overridden in tests.
 * @param navigator the back stack. Overridden in tests that want to start somewhere else.
 * @param foldPosture published to every screen through [LocalFoldPosture].
 * @param onUpdateApp opens the store listing from the protocol-mismatch screen.
 */
@Composable
fun CapsuleAppShell(
    shellState: CapsuleShellState,
    gate: LocalAuthGate,
    modifier: Modifier = Modifier,
    prompt: LocalAuthPrompt = rememberPlatformLocalAuthPrompt(),
    navigator: CapsuleNavigator = rememberCapsuleNavigator(),
    foldPosture: FoldPosture = currentFoldPosture(),
    onUpdateApp: () -> Unit = {},
) {
    val refusal = shellState.protocolRefusal
    if (refusal != null) {
        ProtocolMismatchScreen(onUpdate = onUpdateApp, modifier = modifier)
        return
    }

    val windowClass = CapsuleWindowClass.of(currentWindowAdaptiveInfoV2().windowSizeClass)
    val suiteType = windowClass.navigationSuiteType(navigator.current.immersive)
    val scope = remember(navigator, gate, prompt) { CapsuleEntryScope(navigator, gate, prompt) }
    val entryProvider = rememberCapsuleEntryProvider(scope)

    CompositionLocalProvider(LocalFoldPosture provides foldPosture) {
        NavigationSuiteScaffold(
            navigationItems = { NavigationItems(navigator, suiteType) },
            modifier = modifier.testTag(navigationSuiteTag(suiteType)),
            navigationSuiteType = suiteType,
        ) {
            Box(modifier = Modifier.fillMaxSize()) {
                Column(modifier = Modifier.fillMaxSize()) {
                    if (shellState.isOffline) {
                        OfflineBanner(modifier = Modifier.fillMaxWidth().padding(BannerInset))
                    }
                    Box(modifier = Modifier.fillMaxWidth().weight(1f)) {
                        NavDisplay(
                            backStack = navigator.entries,
                            onBack = { navigator.goBack() },
                            entryProvider = entryProvider,
                        )
                    }
                }
                MessageHost(shellState, Modifier.align(Alignment.BottomCenter))
            }
        }
    }
}

/** The five section items, rendered by whichever component [suiteType] names. */
@Composable
private fun NavigationItems(
    navigator: CapsuleNavigator,
    suiteType: NavigationSuiteType,
) {
    CapsuleDestination.topLevel.forEach { section ->
        val selected = section == navigator.section
        NavigationSuiteItem(
            selected = selected,
            onClick = { navigator.switchTo(section) },
            icon = {
                Icon(
                    imageVector = if (selected) section.selectedIcon else section.unselectedIcon,
                    // The label beside it already names the destination; announcing it twice
                    // makes every tab stop read as a stutter.
                    contentDescription = null,
                )
            },
            label = { Text(stringResource(section.titleRes)) },
            modifier = Modifier.testTag(navigationItemTag(section)),
            navigationSuiteType = suiteType,
        )
    }
}

/** Shows queued transient messages, then tells the state they were delivered. */
@Composable
private fun MessageHost(
    shellState: CapsuleShellState,
    modifier: Modifier = Modifier,
) {
    val message = shellState.pendingMessage
    val text = message?.let { stringResource(it) }
    LaunchedEffect(text) {
        if (text != null) {
            shellState.snackbars.showSnackbar(text)
            shellState.onMessageShown()
        }
    }
    SnackbarHost(hostState = shellState.snackbars, modifier = modifier)
}

private val BannerInset = 12.dp
