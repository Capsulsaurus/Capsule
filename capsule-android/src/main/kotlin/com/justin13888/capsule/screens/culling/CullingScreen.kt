package com.justin13888.capsule.screens.culling

import androidx.activity.compose.LocalActivity
import androidx.compose.foundation.background
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.outlined.CheckCircle
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.R
import com.justin13888.capsule.i18n.RegionalFormat

/**
 * The keyboard-driven cull pass (*Organization - Culling UI*): one asset at a time, a verdict
 * per key, and the pass advancing on its own. The same layout and feature set as the Apple
 * client's culling surface, in Material 3.
 *
 * Every verb has three doors that reach [CullingViewModel.perform]: a key from
 * [CullKeyboard] (listed in the system keyboard shortcuts helper, which the overflow menu
 * opens), an on-screen button, and a TalkBack custom action on the asset, so no verb depends
 * on a pointer, a gesture or sight. The flag shows as a symbol and a word, never by colour
 * alone, and the asset is a polite live region, so every move announces its flag and
 * position. The reject sweep asks for confirmation first.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CullingScreen(
    viewModel: CullingViewModel,
    navigateBack: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    DisposableEffect(Unit) {
        CullKeyboard.shown()
        onDispose { CullKeyboard.hidden() }
    }
    val notice = state.notice
    val noticeText = notice?.let { noticeText(it) }
    LaunchedEffect(notice) {
        if (noticeText != null) {
            snackbar.showSnackbar(noticeText)
            viewModel.consumeNotice()
        }
    }
    Scaffold(
        modifier =
            Modifier.onPreviewKeyEvent { event ->
                if (state.asset == null) return@onPreviewKeyEvent false
                val native = event.nativeKeyEvent
                val isDown = event.type == KeyEventType.KeyDown
                val verb =
                    CullKeyboard.verbForEvent(isDown, native.repeatCount, native.keyCode, native.metaState)
                if (verb != null) viewModel.perform(verb)
                // A held key's repeats are consumed too, so they never reach a focused button.
                isDown && CullKeyboard.verbFor(native.keyCode, native.metaState) != null
            },
        topBar = { CullingTopBar(state, viewModel::perform, navigateBack) },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            CullingContent(state, viewModel::perform)
        }
    }
    if (state.isConfirmingSweep) {
        SweepConfirmation(state.sweepCount, confirm = viewModel::confirmSweep, dismiss = viewModel::dismissSweep)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CullingTopBar(
    state: CullingState,
    perform: (CullVerb) -> Unit,
    navigateBack: () -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    val activity = LocalActivity.current
    TopAppBar(
        title = { Text(stringResource(R.string.app_culling_title)) },
        navigationIcon = {
            IconButton(onClick = navigateBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back))
            }
        },
        actions = {
            IconButton(
                onClick = { perform(CullVerb.SWEEP) },
                enabled = CullVerb.SWEEP in state.accepted,
                modifier = Modifier.testTag(CullingTags.SWEEP),
            ) {
                Icon(Icons.Filled.Delete, stringResource(CullVerb.SWEEP.label))
            }
            IconButton(onClick = { menuOpen = true }, modifier = Modifier.testTag(CullingTags.MORE)) {
                Icon(Icons.Filled.MoreVert, stringResource(R.string.app_home_menu_more))
            }
            DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.app_culling_action_shortcuts)) },
                    onClick = {
                        menuOpen = false
                        activity?.requestShowKeyboardShortcuts()
                    },
                )
            }
        },
    )
}

@Composable
private fun CullingContent(
    state: CullingState,
    perform: (CullVerb) -> Unit,
) {
    when {
        state.phase == CullingPhase.LOADING ->
            Placeholder { CircularProgressIndicator() }
        state.phase == CullingPhase.FAILED ->
            Placeholder {
                Text(stringResource(R.string.app_culling_title), style = MaterialTheme.typography.titleMedium)
                Text(stringResource(R.string.app_culling_error_load), textAlign = TextAlign.Center)
            }
        state.asset == null ->
            Placeholder {
                Icon(Icons.Filled.CheckCircle, contentDescription = null, modifier = Modifier.size(48.dp))
                Text(stringResource(R.string.app_culling_empty_title), style = MaterialTheme.typography.titleMedium)
                Text(stringResource(R.string.app_culling_empty_body), textAlign = TextAlign.Center)
            }
        else ->
            Column(
                Modifier.fillMaxSize().padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                AssetPane(state, perform, Modifier.weight(1f))
                FlagBadge(state.flag)
                Text(
                    positionText(state.position),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.clearAndSetSemantics {},
                )
                Controls(state, perform)
            }
    }
}

/**
 * The asset under review, as one accessibility element: its label is the flag and the
 * position, every verb is a custom action on it, and it is a polite live region, so each
 * move is announced. It takes keyboard focus when the pass opens.
 */
@Composable
private fun AssetPane(
    state: CullingState,
    perform: (CullVerb) -> Unit,
    modifier: Modifier = Modifier,
) {
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }
    val description = stringResource(R.string.app_culling_announcement, flagText(state.flag), positionText(state.position))
    val labels = CullVerb.entries.associateWith { stringResource(it.label) }
    val actions =
        ACTION_VERBS
            .filter { it in ALWAYS_OFFERED || it in state.accepted }
            .map { verb ->
                CustomAccessibilityAction(labels.getValue(verb)) {
                    perform(verb)
                    true
                }
            }
    Box(
        modifier
            .fillMaxWidth()
            .clip(MaterialTheme.shapes.medium)
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .focusRequester(focus)
            .focusable()
            .semantics {
                contentDescription = description
                role = Role.Image
                liveRegion = LiveRegionMode.Polite
                customActions = actions
            }.testTag(CullingTags.ASSET),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            flagIcon(state.flag),
            contentDescription = null,
            modifier = Modifier.size(96.dp),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** The current flag as a symbol and a word, never a colour alone; the asset already says it to TalkBack. */
@Composable
private fun FlagBadge(flag: CullStepFlag?) {
    Row(
        Modifier
            .clip(MaterialTheme.shapes.extraLarge)
            .background(MaterialTheme.colorScheme.secondaryContainer)
            .padding(horizontal = 12.dp, vertical = 6.dp)
            .clearAndSetSemantics {},
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(flagIcon(flag), contentDescription = null, tint = MaterialTheme.colorScheme.onSecondaryContainer)
        Text(
            flagText(flag),
            style = MaterialTheme.typography.titleMedium,
            color = MaterialTheme.colorScheme.onSecondaryContainer,
        )
    }
}

/** A button per verb; the stack slot expands a collapsed stack or collapses an open one. */
@Composable
private fun Controls(
    state: CullingState,
    perform: (CullVerb) -> Unit,
) {
    val stackVerb = if (CullVerb.COLLAPSE_STACK in state.accepted) CullVerb.COLLAPSE_STACK else CullVerb.EXPAND_STACK
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
        for (verb in listOf(CullVerb.PREVIOUS, CullVerb.REJECT, CullVerb.NEUTRAL, CullVerb.PICK, CullVerb.NEXT, stackVerb)) {
            IconButton(
                onClick = { perform(verb) },
                enabled = verb in state.accepted,
                modifier = Modifier.testTag(CullingTags.verb(verb)),
            ) {
                Icon(verbIcon(verb), stringResource(verb.label))
            }
        }
    }
}

@Composable
private fun SweepConfirmation(
    count: Int,
    confirm: () -> Unit,
    dismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(pluralStringResource(R.plurals.app_culling_sweep_title, count, count)) },
        text = { Text(stringResource(R.string.app_culling_sweep_message)) },
        confirmButton = {
            TextButton(
                onClick = confirm,
                colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
            ) { Text(stringResource(R.string.app_culling_sweep_confirm)) }
        },
        dismissButton = {
            TextButton(onClick = dismiss) { Text(stringResource(R.string.app_common_cancel)) }
        },
    )
}

@Composable
private fun Placeholder(content: @Composable () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp, Alignment.CenterVertically),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) { content() }
}

/** The flag as a word: an asset's own, or a collapsed stack's state and size. */
@Composable
private fun flagText(flag: CullStepFlag?): String =
    when (flag) {
        is CullStepFlag.Stack -> {
            val summary =
                stringResource(
                    when (flag.state) {
                        StackCullState.ANY_PICK -> R.string.app_culling_stack_any_pick
                        StackCullState.ALL_REJECTED -> R.string.app_culling_stack_all_rejected
                        StackCullState.MIXED -> R.string.app_culling_stack_mixed
                    },
                )
            val members = pluralStringResource(R.plurals.app_culling_stack_members, flag.memberCount, flag.memberCount)
            stringResource(R.string.app_culling_stack_summary, summary, members)
        }
        is CullStepFlag.Asset ->
            stringResource(
                when (flag.flag) {
                    CullFlag.PICK -> R.string.app_culling_flag_pick
                    CullFlag.REJECT -> R.string.app_culling_flag_reject
                    CullFlag.NEUTRAL -> R.string.app_culling_flag_unflagged
                },
            )
        null -> stringResource(R.string.app_culling_flag_unflagged)
    }

@Composable
private fun positionText(position: CullPosition): String {
    val format = RegionalFormat.number(LocalContext.current)
    return stringResource(R.string.app_culling_position, format.format(position.index), format.format(position.count))
}

@Composable
private fun noticeText(notice: CullNotice): String =
    when (notice) {
        CullNotice.FlagFailed -> stringResource(R.string.app_culling_error_flag)
        CullNotice.SweepFailed -> stringResource(R.string.app_culling_error_sweep)
        is CullNotice.Swept -> pluralStringResource(R.plurals.app_culling_sweep_done, notice.count, notice.count)
    }

/** The flag's symbol. A stack shows its derived state's: a pick, all rejected, or undecided. */
private fun flagIcon(flag: CullStepFlag?): ImageVector =
    when (flag) {
        is CullStepFlag.Asset ->
            when (flag.flag) {
                CullFlag.PICK -> Icons.Filled.CheckCircle
                CullFlag.REJECT -> Icons.Filled.Close
                CullFlag.NEUTRAL -> Icons.Outlined.CheckCircle
            }
        is CullStepFlag.Stack ->
            when (flag.state) {
                StackCullState.ANY_PICK -> Icons.Filled.CheckCircle
                StackCullState.ALL_REJECTED -> Icons.Filled.Close
                StackCullState.MIXED -> Icons.Outlined.CheckCircle
            }
        null -> Icons.Outlined.CheckCircle
    }

private fun verbIcon(verb: CullVerb): ImageVector =
    when (verb) {
        CullVerb.PICK -> Icons.Filled.CheckCircle
        CullVerb.NEUTRAL -> Icons.Outlined.CheckCircle
        CullVerb.REJECT -> Icons.Filled.Close
        CullVerb.NEXT -> Icons.AutoMirrored.Filled.KeyboardArrowRight
        CullVerb.PREVIOUS -> Icons.AutoMirrored.Filled.KeyboardArrowLeft
        CullVerb.EXPAND_STACK -> Icons.Filled.KeyboardArrowDown
        CullVerb.COLLAPSE_STACK -> Icons.Filled.KeyboardArrowUp
        CullVerb.SWEEP -> Icons.Filled.Delete
    }

/** The verbs offered as TalkBack actions on the asset, in the order the menu lists them. */
private val ACTION_VERBS =
    listOf(
        CullVerb.PICK,
        CullVerb.NEUTRAL,
        CullVerb.REJECT,
        CullVerb.NEXT,
        CullVerb.PREVIOUS,
        CullVerb.EXPAND_STACK,
        CullVerb.COLLAPSE_STACK,
        CullVerb.SWEEP,
    )

/** Offered whatever the step, as the Apple surface does; the rest only where they would act. */
private val ALWAYS_OFFERED = setOf(CullVerb.PICK, CullVerb.NEUTRAL, CullVerb.REJECT, CullVerb.NEXT, CullVerb.PREVIOUS)
