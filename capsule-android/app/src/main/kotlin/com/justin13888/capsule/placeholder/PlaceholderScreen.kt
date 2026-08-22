package com.justin13888.capsule.placeholder

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Construction
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleEmptyState
import com.justin13888.capsule.core.designsystem.component.CapsuleTopAppBar
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.navigation.CapsuleDestination
import com.justin13888.capsule.navigation.CapsuleNavigator

/** Test handle shared by every placeholder, so a test can assert *a* destination rendered. */
const val PLACEHOLDER_TAG: String = "shell.placeholder"

/** Test handle for the placeholder's back affordance. */
const val PLACEHOLDER_BACK_TAG: String = "shell.placeholder-back"

/** Prefix for the per-destination test handle: `shell.placeholder:Timeline`. */
fun placeholderTagFor(destination: CapsuleDestination): String = "$PLACEHOLDER_TAG:${destination::class.simpleName}"

/**
 * What a destination renders until the feature module that owns it exists.
 *
 * Deliberately loud rather than blank. A silent empty screen is indistinguishable from a
 * screen that failed to load, and the shell is being validated on three window classes and
 * two fold postures right now — a reviewer resizing the window needs to be able to tell "the
 * layout is right, the content is pending" from "the layout broke".
 *
 * It carries the destination's own title and its own back affordance so that the chrome under
 * test is the real chrome: replacing this call with the feature's screen must not change what
 * the shell does around it.
 *
 * @param destination the key being rendered; supplies the title and the test handle.
 * @param navigator used for the back affordance, and the same object the real screen gets.
 */
@Composable
fun PlaceholderScreen(
    destination: CapsuleDestination,
    navigator: CapsuleNavigator,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize().testTag(placeholderTagFor(destination))) {
        CapsuleTopAppBar(
            title = stringResource(destination.titleRes),
            navigationIcon = {
                if (navigator.canGoBack) {
                    IconButton(
                        onClick = { navigator.goBack() },
                        modifier = Modifier.testTag(PLACEHOLDER_BACK_TAG),
                    ) {
                        Icon(
                            imageVector = Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.android_shell_back),
                        )
                    }
                }
            },
        )
        CapsuleEmptyState(
            title = stringResource(R.string.android_shell_placeholder_title),
            modifier = Modifier.fillMaxSize().testTag(PLACEHOLDER_TAG),
            body = stringResource(R.string.android_shell_placeholder_body),
            icon = Icons.Filled.Construction,
        )
    }
}
