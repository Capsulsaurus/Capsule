package com.justin13888.capsule

import android.os.SystemClock
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.toRoute
import com.justin13888.capsule.screens.DetailScreen
import com.justin13888.capsule.screens.ListScreen
import com.justin13888.capsule.screens.culling.CullPort
import com.justin13888.capsule.screens.culling.CullingScreen
import com.justin13888.capsule.screens.culling.CullingViewModel
import com.justin13888.capsule.screens.culling.FfiCullPort
import com.justin13888.capsule.screens.culling.InMemoryCullPort
import com.justin13888.capsule.screens.domain.AlbumId
import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.LabelPort
import com.justin13888.capsule.screens.domain.StackId
import com.justin13888.capsule.screens.domain.TransferEventPort
import com.justin13888.capsule.screens.labels.AssetViewerScreen
import com.justin13888.capsule.screens.labels.CollectionLabelsScreen
import com.justin13888.capsule.screens.labels.FfiLabelPort
import com.justin13888.capsule.screens.labels.LabelEditorViewModel
import com.justin13888.capsule.screens.labels.LabelSubject
import com.justin13888.capsule.screens.mock.MockEnvironment
import com.justin13888.capsule.screens.transfer.FfiTransferEventPort
import com.justin13888.capsule.screens.transfer.TransferBoardScreen
import com.justin13888.capsule.screens.transfer.TransferBoardViewModel
import com.justin13888.capsule.workspace.ServerSessions
import com.justin13888.capsule.workspace.SignInScreen
import com.justin13888.capsule.workspace.SignInViewModel
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.serialization.Serializable
import uniffi.capsule_sdk.FfiSessionInterface
import uniffi.capsule_sdk.FfiWorkspaceInterface

@Serializable
object ListDestination

@Serializable
data class DetailDestination(
    val objectId: Int,
)

/** The real-time transfer view. */
@Serializable
object TransferBoardDestination

/** The asset viewer with its region editor, open on [assetId]. */
@Serializable
data class AssetViewerDestination(
    val assetId: String,
)

/** The keyboard-driven, accessible cull pass over the library. */
@Serializable
object CullingDestination

/** Sign in to a server for the session the transfer view follows. */
@Serializable
object SignInDestination

/** The label editor of a stack or an album: exactly one of the two ids is set. */
@Serializable
data class CollectionLabelsDestination(
    val stackId: String? = null,
    val albumId: String? = null,
)

/**
 * What the transfer view, the label editor and the culling pass run on: their ports, and
 * the subjects the home menu opens the editors on. A subject the world has none of hides
 * its entry.
 */
class ScreenPorts(
    val transferEvents: TransferEventPort,
    val labels: LabelPort,
    val sampleAsset: AssetId?,
    val sampleStack: StackId?,
    val sampleAlbum: AlbumId?,
    val culling: CullPort,
) {
    companion object {
        /**
         * The mock world: its recorded transfer feed, its label store, its sample subjects,
         * and a sample pass to cull.
         */
        fun mock(environment: MockEnvironment = MockEnvironment()): ScreenPorts =
            ScreenPorts(
                environment.transferEvents,
                environment.labels,
                environment.sampleAsset,
                environment.sampleStack,
                environment.sampleAlbum,
                InMemoryCullPort.sample(environment.seed),
            )

        /**
         * The Rust-backed world: labels, regions and the cull verbs over [workspace], and
         * the transfer events of [session], or none while nothing is signed in. The album editor opens
         * on the workspace's default album; [sampleAsset] and [sampleStack] are whatever
         * the caller's library shows.
         */
        fun rustBacked(
            workspace: FfiWorkspaceInterface,
            session: FfiSessionInterface?,
            sampleAsset: AssetId? = null,
            sampleStack: StackId? = null,
        ): ScreenPorts =
            ScreenPorts(
                transferEvents = session?.let { FfiTransferEventPort(it::transferEvents) } ?: TransferEventPort { emptyFlow() },
                labels = FfiLabelPort(workspace),
                sampleAsset = sampleAsset,
                sampleStack = sampleStack,
                sampleAlbum = AlbumId(workspace.defaultAlbumId()),
                culling = FfiCullPort(workspace),
            )
    }
}

/** The mock world, held for the life of the process so a recreated activity keeps its edits. */
private val mockPorts by lazy { ScreenPorts.mock() }

/**
 * The app over [ports]: the mock world by default, or [ScreenPorts.rustBacked] over the
 * device's library. [sessions] is the server session the app signs in to; while nothing is
 * signed in, the home menu offers sign-in, and without it (the mock world) it offers none.
 */
@Composable
fun App(
    ports: ScreenPorts = mockPorts,
    sessions: ServerSessions? = null,
) {
    val signedIn = sessions?.current?.collectAsStateWithLifecycle()?.value != null
    MaterialTheme(
        colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme(),
    ) {
        Surface {
            val navController = rememberNavController()
            NavHost(navController = navController, startDestination = ListDestination) {
                composable<ListDestination> {
                    HomeShell(
                        openTransfers = { navController.navigate(TransferBoardDestination) },
                        openCulling = { navController.navigate(CullingDestination) },
                        openSignIn = sessions?.takeIf { !signedIn }?.let { { navController.navigate(SignInDestination) } },
                        openSampleAsset = ports.sampleAsset?.let { { navController.navigate(AssetViewerDestination(it.value)) } },
                        openStackLabels =
                            ports.sampleStack?.let { { navController.navigate(CollectionLabelsDestination(stackId = it.value)) } },
                        openAlbumLabels =
                            ports.sampleAlbum?.let { { navController.navigate(CollectionLabelsDestination(albumId = it.value)) } },
                    ) {
                        ListScreen(navigateToDetails = { objectId ->
                            navController.navigate(DetailDestination(objectId))
                        })
                    }
                }
                composable<DetailDestination> { backStackEntry ->
                    DetailScreen(
                        objectId = backStackEntry.toRoute<DetailDestination>().objectId,
                        navigateBack = {
                            navController.popBackStack()
                        },
                    )
                }
                composable<TransferBoardDestination> {
                    val boardViewModel =
                        viewModel { TransferBoardViewModel(ports.transferEvents, SystemClock::elapsedRealtime) }
                    TransferBoardScreen(boardViewModel, navigateBack = { navController.popBackStack() })
                }
                composable<CullingDestination> {
                    val culling = viewModel { CullingViewModel(ports.culling) }
                    CullingScreen(culling, navigateBack = { navController.popBackStack() })
                }
                if (sessions != null) {
                    composable<SignInDestination> {
                        val signIn = viewModel { SignInViewModel(sessions) }
                        SignInScreen(signIn, navigateBack = { navController.popBackStack() })
                    }
                }
                composable<AssetViewerDestination> { backStackEntry ->
                    val asset = AssetId(backStackEntry.toRoute<AssetViewerDestination>().assetId)
                    val editor = viewModel { LabelEditorViewModel(LabelSubject.Asset(asset), ports.labels) }
                    AssetViewerScreen(editor, navigateBack = { navController.popBackStack() })
                }
                composable<CollectionLabelsDestination> { backStackEntry ->
                    val route = backStackEntry.toRoute<CollectionLabelsDestination>()
                    val subject =
                        route.albumId?.let { LabelSubject.Album(AlbumId(it)) }
                            ?: LabelSubject.Stack(StackId(route.stackId.orEmpty()))
                    val editor = viewModel { LabelEditorViewModel(subject, ports.labels) }
                    CollectionLabelsScreen(editor, navigateBack = { navController.popBackStack() })
                }
            }
        }
    }
}

/**
 * The home screen's app bar: an overflow menu into sign-in, the transfer view, the culling
 * pass and the label editors.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun HomeShell(
    openTransfers: () -> Unit,
    openCulling: () -> Unit,
    openSignIn: (() -> Unit)?,
    openSampleAsset: (() -> Unit)?,
    openStackLabels: (() -> Unit)?,
    openAlbumLabels: (() -> Unit)?,
    content: @Composable () -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    val choose = { action: () -> Unit ->
        menuOpen = false
        action()
    }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_name)) },
                actions = {
                    IconButton(onClick = { menuOpen = true }) {
                        Icon(Icons.Filled.MoreVert, stringResource(R.string.app_home_menu_more))
                    }
                    DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                        openSignIn?.let { open ->
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.app_auth_welcome_server_action)) },
                                onClick = { choose(open) },
                            )
                        }
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.app_transfer_board_title)) },
                            onClick = { choose(openTransfers) },
                        )
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.app_culling_open)) },
                            onClick = { choose(openCulling) },
                        )
                        openSampleAsset?.let { open ->
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.app_viewer_sample_title)) },
                                onClick = { choose(open) },
                            )
                        }
                        openStackLabels?.let { open ->
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.app_labels_title_stack)) },
                                onClick = { choose(open) },
                            )
                        }
                        openAlbumLabels?.let { open ->
                            DropdownMenuItem(
                                text = { Text(stringResource(R.string.app_labels_title_album)) },
                                onClick = { choose(open) },
                            )
                        }
                    }
                },
            )
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) { content() }
    }
}
