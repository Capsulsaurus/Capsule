package com.justin13888.capsule.feature.drops

import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.core.model.UploadLinkId
import com.justin13888.capsule.feature.drops.ui.DropInboxActions
import com.justin13888.capsule.feature.drops.ui.DropInboxScreen
import com.justin13888.capsule.feature.drops.ui.UploadLinkCreateActions
import com.justin13888.capsule.feature.drops.ui.UploadLinkCreateScreen
import com.justin13888.capsule.feature.drops.ui.UploadLinkDetailScreen
import com.justin13888.capsule.feature.drops.ui.UploadLinksScreen
import org.koin.androidx.compose.koinViewModel
import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.core.parameter.parametersOf
import org.koin.dsl.module

/**
 * What the drops area contributes to the app graph.
 *
 * Declared here rather than in the app's DI file so a feature slice adds its bindings without
 * touching a file every other slice also edits. The app assembles this module alongside the
 * rest; nothing here reaches back into the shell.
 */
val dropsFeatureModule: Module =
    module {
        viewModel { UploadLinksViewModel(sharing = get()) }
        viewModel { UploadLinkCreateViewModel(sharing = get()) }
        viewModel { (linkId: String) ->
            UploadLinkDetailViewModel(sharing = get(), linkId = UploadLinkId(linkId))
        }
        viewModel { DropInboxViewModel(drops = get(), albums = get(), sharing = get()) }
    }

/** The list of provisioned upload links. */
@Composable
fun UploadLinksRoute(
    onOpenLink: (String) -> Unit,
    onCreate: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: UploadLinksViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    UploadLinksScreen(
        state = state,
        onOpenLink = { id -> onOpenLink(id.value) },
        onCreate = onCreate,
        onRetry = viewModel::reload,
        onBack = onBack,
        modifier = modifier,
    )
}

/**
 * The create-upload-link form.
 *
 * Navigation to the new link's detail happens here once, on the created transition, so the
 * user lands on the caps they just chose rather than on a list where the new row looks like
 * every other one.
 */
@Composable
fun UploadLinkCreateRoute(
    onCreated: (String) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: UploadLinkCreateViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val created = state.created
    LaunchedEffect(created) {
        if (created != null) {
            viewModel.consumeCreated()
            onCreated(created.id.value)
        }
    }
    UploadLinkCreateScreen(
        state = state,
        actions =
            UploadLinkCreateActions(
                selectExpiry = viewModel::selectExpiry,
                setCap = viewModel::setCap,
                setSingleUse = viewModel::setSingleUse,
                setPassphrase = viewModel::setPassphrase,
                submit = viewModel::create,
                back = onBack,
            ),
        modifier = modifier,
    )
}

/** One upload link's caps and how much of each is gone. */
@Composable
fun UploadLinkDetailRoute(
    uploadLinkId: String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: UploadLinkDetailViewModel =
        koinViewModel(key = uploadLinkId, parameters = { parametersOf(uploadLinkId) })
    val state by viewModel.state.collectAsStateWithLifecycle()
    UploadLinkDetailScreen(
        state = state,
        onRevoke = viewModel::revoke,
        onRetry = viewModel::reload,
        onBack = onBack,
        modifier = modifier,
    )
}

/** The inbox of guest deposits awaiting review. */
@Composable
fun DropInboxRoute(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: DropInboxViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    DropInboxScreen(
        state = state,
        actions =
            DropInboxActions(
                beginAdoption = viewModel::beginAdoption,
                cancelAdoption = viewModel::cancelAdoption,
                adopt = viewModel::adopt,
                discard = viewModel::discard,
                retry = viewModel::reload,
                back = onBack,
            ),
        modifier = modifier,
    )
}
