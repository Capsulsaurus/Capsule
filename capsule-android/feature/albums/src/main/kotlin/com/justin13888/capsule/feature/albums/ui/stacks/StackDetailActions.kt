package com.justin13888.capsule.feature.albums.ui.stacks

import com.justin13888.capsule.core.model.AssetId

/** Every action stack detail offers. */
data class StackDetailActions(
    val onBack: () -> Unit = {},
    val onSetPrimary: (AssetId) -> Unit = {},
    val onMoveEarlier: (Int) -> Unit = {},
    val onMoveLater: (Int) -> Unit = {},
    val onRetry: () -> Unit = {},
)
