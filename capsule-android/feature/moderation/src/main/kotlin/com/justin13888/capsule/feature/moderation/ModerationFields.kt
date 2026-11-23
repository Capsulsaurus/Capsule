package com.justin13888.capsule.feature.moderation

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.TextFieldColors
import androidx.compose.runtime.Composable

/**
 * Text-field colours that clear the contrast floor at rest.
 *
 * `core:designsystem` has no text-field wrapper yet, so these screens reach for Material's own
 * — and Material draws an unfocused outline in `outline`, which measures 4.27:1 against this
 * theme's light surface where small text needs 4.5:1. The border is the only thing marking
 * where a field begins, so a user who cannot resolve it cannot find the field at all.
 *
 * Overriding only the two resting borders keeps every focused, errored and selected colour
 * exactly as the theme defines it; this is a floor being raised, not a palette being forked.
 */
@Composable
internal fun capsuleFieldColors(): TextFieldColors =
    OutlinedTextFieldDefaults.colors(
        unfocusedBorderColor = MaterialTheme.colorScheme.onSurfaceVariant,
        disabledBorderColor = MaterialTheme.colorScheme.onSurfaceVariant,
    )
