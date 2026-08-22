package com.justin13888.capsule.core.designsystem.component

import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector

/**
 * A Capsule button.
 *
 * Features never call Material's button composables directly. Going through one wrapper
 * is what lets the design system change how emphasis is expressed — press morph, shape,
 * container colour — without a sweep across seventeen feature modules, and it is the
 * only place the 48dp touch floor can be guaranteed.
 *
 * @param label already-localized text. The design system owns no copy.
 * @param icon an optional leading icon; decorative, so it is never announced separately.
 * @param contentDescription overrides the announced label where [label] alone would be
 *   ambiguous out of context ("Delete" → "Delete 12 selected photos").
 */
@Composable
fun CapsuleButton(
    onClick: () -> Unit,
    label: String,
    modifier: Modifier = Modifier,
    emphasis: CapsuleButtonEmphasis = CapsuleButtonEmphasis.Primary,
    icon: ImageVector? = null,
    enabled: Boolean = true,
    contentDescription: String? = null,
) {
    val buttonModifier = modifier.capsuleTouchTarget().capsuleDescription(contentDescription)
    when (emphasis) {
        CapsuleButtonEmphasis.Primary ->
            Button(
                onClick = onClick,
                shapes = ButtonDefaults.shapes(),
                modifier = buttonModifier,
                enabled = enabled,
                content = { CapsuleLabelWithIcon(label, icon) },
            )

        CapsuleButtonEmphasis.Secondary ->
            FilledTonalButton(
                onClick = onClick,
                shapes = ButtonDefaults.shapes(),
                modifier = buttonModifier,
                enabled = enabled,
                content = { CapsuleLabelWithIcon(label, icon) },
            )

        CapsuleButtonEmphasis.Tertiary ->
            TextButton(
                onClick = onClick,
                shapes = ButtonDefaults.shapes(),
                modifier = buttonModifier,
                enabled = enabled,
                content = { CapsuleLabelWithIcon(label, icon) },
            )

        CapsuleButtonEmphasis.Destructive ->
            Button(
                onClick = onClick,
                shapes = ButtonDefaults.shapes(),
                modifier = buttonModifier,
                enabled = enabled,
                colors =
                    ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.error,
                        contentColor = MaterialTheme.colorScheme.onError,
                    ),
                content = { CapsuleLabelWithIcon(label, icon) },
            )
    }
}

/**
 * How loudly a button asks to be pressed.
 *
 * Four levels, not five: every extra level is a decision a feature author has to make
 * correctly on every screen, and a photo library only ever needs "the action", "another
 * action", "a way out", and "this deletes originals".
 */
enum class CapsuleButtonEmphasis {
    /** The one action a screen exists for. At most one per surface. */
    Primary,

    /** A real alternative to the primary action. */
    Secondary,

    /** Dismissals and navigational escapes; carries no container of its own. */
    Tertiary,

    /** Irreversible loss of user data. Always paired with a confirmation. */
    Destructive,
}
