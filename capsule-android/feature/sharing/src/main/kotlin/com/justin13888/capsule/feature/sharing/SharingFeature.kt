package com.justin13888.capsule.feature.sharing

import android.content.ClipData
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.ClipEntry
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.core.model.ShareLinkId
import com.justin13888.capsule.feature.sharing.ui.ShareLinkCreateActions
import com.justin13888.capsule.feature.sharing.ui.ShareLinkCreateScreen
import com.justin13888.capsule.feature.sharing.ui.ShareLinkDetailScreen
import com.justin13888.capsule.feature.sharing.ui.ShareLinksScreen
import com.justin13888.capsule.feature.sharing.ui.SharingHubActions
import com.justin13888.capsule.feature.sharing.ui.SharingHubScreen
import kotlinx.coroutines.launch
import org.koin.androidx.compose.koinViewModel
import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.core.parameter.parametersOf
import org.koin.dsl.module

/**
 * The origin the app builds share URLs against until a session carries a real one.
 *
 * A reserved-for-documentation domain rather than a plausible-looking host: this build's data
 * is synthetic, and a URL that looked like a live server would be one screenshot away from
 * somebody trying to open it.
 */
private const val DEFAULT_LINK_ORIGIN = "https://capsule.example"

/**
 * What the sharing area contributes to the app graph.
 *
 * Declared here rather than in the app's DI file so a feature slice adds its bindings without
 * touching a file every other slice also edits. The app assembles this module alongside the
 * rest; nothing here reaches back into the shell.
 */
val sharingFeatureModule: Module =
    module {
        single { LinkOrigin(DEFAULT_LINK_ORIGIN) }
        viewModel { SharingViewModel(sharing = get(), drops = get(), albums = get()) }
        viewModel { (args: ShareLinkCreateArgs) ->
            ShareLinkCreateViewModel(
                sharing = get(),
                albums = get(),
                assetId = args.assetId?.let(::AssetId),
                albumId = args.albumId?.let(::AlbumId),
            )
        }
        viewModel { (shareId: String) ->
            ShareLinkDetailViewModel(
                sharing = get(),
                albums = get(),
                linkId = ShareLinkId(shareId),
                origin = get(),
            )
        }
    }

/**
 * What the create form was opened with.
 *
 * Destination ids travel as `String` because navigation keys are serialized reflectively, so
 * they are re-wrapped into the model's value classes here — at the feature boundary — rather
 * than being carried untyped into the view model.
 */
data class ShareLinkCreateArgs(
    /** The asset to offer a single-item scope for, when one was in hand. */
    val assetId: String? = null,
    /** The album to pre-select. */
    val albumId: String? = null,
)

/** The sharing hub, wired to its view model. */
@Composable
fun SharingHubRoute(
    actions: SharingHubActions,
    modifier: Modifier = Modifier,
) {
    val viewModel: SharingViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    SharingHubScreen(
        state = state,
        actions =
            SharingHubActions(
                openShareLinks = actions.openShareLinks,
                openUploadLinks = actions.openUploadLinks,
                openDropInbox = actions.openDropInbox,
                createShareLink = actions.createShareLink,
                createUploadLink = actions.createUploadLink,
                retry = viewModel::reload,
            ),
        modifier = modifier,
    )
}

/** The full list of issued share links. */
@Composable
fun ShareLinksRoute(
    onOpenLink: (String) -> Unit,
    onCreate: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: SharingViewModel = koinViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    ShareLinksScreen(
        state = state,
        onOpenLink = { id -> onOpenLink(id.value) },
        onCreate = onCreate,
        onRetry = viewModel::reload,
        onBack = onBack,
        modifier = modifier,
    )
}

/**
 * The create-share-link form.
 *
 * Navigation to the new link's detail happens here, once, on the state's [ShareLinkCreateUiState.created]
 * transition — the created link carries its fragment secret and this is the only moment it
 * can be shown, so dropping the user back on a list would strand a link nobody can share.
 */
@Composable
fun ShareLinkCreateRoute(
    onCreated: (String) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    albumId: String? = null,
    assetId: String? = null,
) {
    val args = remember(albumId, assetId) { ShareLinkCreateArgs(assetId = assetId, albumId = albumId) }
    val viewModel: ShareLinkCreateViewModel = koinViewModel(parameters = { parametersOf(args) })
    val state by viewModel.state.collectAsStateWithLifecycle()
    val created = state.created
    LaunchedEffect(created) {
        if (created != null) {
            viewModel.consumeCreated()
            onCreated(created.id.value)
        }
    }
    ShareLinkCreateScreen(
        state = state,
        actions =
            ShareLinkCreateActions(
                selectScope = viewModel::selectScope,
                selectAlbum = viewModel::selectAlbum,
                setPassphrase = viewModel::setPassphrase,
                selectExpiry = viewModel::selectExpiry,
                submit = viewModel::create,
                retry = viewModel::reload,
                back = onBack,
            ),
        modifier = modifier,
    )
}

/** One share link's detail, including the clipboard effect the screen must not own. */
@Composable
fun ShareLinkDetailRoute(
    shareId: String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: ShareLinkDetailViewModel =
        koinViewModel(key = shareId, parameters = { parametersOf(shareId) })
    val state by viewModel.state.collectAsStateWithLifecycle()
    val clipboard = LocalClipboard.current
    val scope = rememberCoroutineScope()
    val clipLabel = stringResource(R.string.android_destination_share_link_detail)
    var justCopied by remember(shareId) { mutableStateOf(false) }
    ShareLinkDetailScreen(
        state = state,
        justCopied = justCopied,
        onCopy = {
            val url = state.url
            if (url != null) {
                scope.launch {
                    // The whole string, fragment included. Trimming it here would hand out a
                    // link that resolves and then decrypts to nothing.
                    clipboard.setClipEntry(ClipEntry(ClipData.newPlainText(clipLabel, url)))
                    justCopied = true
                }
            }
        },
        onRevoke = viewModel::revoke,
        onRetry = viewModel::reload,
        onBack = onBack,
        modifier = modifier,
    )
}
