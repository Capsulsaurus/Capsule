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
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.toRoute
import com.justin13888.capsule.screens.DetailScreen
import com.justin13888.capsule.screens.ListScreen
import com.justin13888.capsule.screens.domain.AlbumId
import com.justin13888.capsule.screens.domain.AssetId
import com.justin13888.capsule.screens.domain.StackId
import com.justin13888.capsule.screens.labels.AssetViewerScreen
import com.justin13888.capsule.screens.labels.CollectionLabelsScreen
import com.justin13888.capsule.screens.labels.LabelEditorViewModel
import com.justin13888.capsule.screens.labels.LabelSubject
import com.justin13888.capsule.screens.mock.MockEnvironment
import com.justin13888.capsule.screens.transfer.TransferBoardScreen
import com.justin13888.capsule.screens.transfer.TransferBoardViewModel
import kotlinx.serialization.Serializable

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

/** The label editor of a stack or an album: exactly one of the two ids is set. */
@Serializable
data class CollectionLabelsDestination(
    val stackId: String? = null,
    val albumId: String? = null,
)

/** The mock lane every new surface runs on until the Rust-backed adapters land. */
private val environment = MockEnvironment()

@Composable
fun App() {
    MaterialTheme(
        colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme(),
    ) {
        Surface {
            val navController = rememberNavController()
            NavHost(navController = navController, startDestination = ListDestination) {
                composable<ListDestination> {
                    HomeShell(
                        openTransfers = { navController.navigate(TransferBoardDestination) },
                        openSampleAsset = { navController.navigate(AssetViewerDestination(environment.sampleAsset.value)) },
                        openStackLabels = {
                            navController.navigate(CollectionLabelsDestination(stackId = environment.sampleStack.value))
                        },
                        openAlbumLabels = {
                            navController.navigate(CollectionLabelsDestination(albumId = environment.sampleAlbum.value))
                        },
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
                        viewModel { TransferBoardViewModel(environment.transferEvents, SystemClock::elapsedRealtime) }
                    TransferBoardScreen(boardViewModel, navigateBack = { navController.popBackStack() })
                }
                composable<AssetViewerDestination> { backStackEntry ->
                    val asset = AssetId(backStackEntry.toRoute<AssetViewerDestination>().assetId)
                    val editor = viewModel { LabelEditorViewModel(LabelSubject.Asset(asset), environment.labels) }
                    AssetViewerScreen(editor, navigateBack = { navController.popBackStack() })
                }
                composable<CollectionLabelsDestination> { backStackEntry ->
                    val route = backStackEntry.toRoute<CollectionLabelsDestination>()
                    val subject =
                        route.albumId?.let { LabelSubject.Album(AlbumId(it)) }
                            ?: LabelSubject.Stack(StackId(route.stackId.orEmpty()))
                    val editor = viewModel { LabelEditorViewModel(subject, environment.labels) }
                    CollectionLabelsScreen(editor, navigateBack = { navController.popBackStack() })
                }
            }
        }
    }
}

/** The home screen's app bar: an overflow menu into the transfer view and the label editors. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun HomeShell(
    openTransfers: () -> Unit,
    openSampleAsset: () -> Unit,
    openStackLabels: () -> Unit,
    openAlbumLabels: () -> Unit,
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
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.app_transfer_board_title)) },
                            onClick = { choose(openTransfers) },
                        )
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.app_viewer_sample_title)) },
                            onClick = { choose(openSampleAsset) },
                        )
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.app_labels_title_stack)) },
                            onClick = { choose(openStackLabels) },
                        )
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.app_labels_title_album)) },
                            onClick = { choose(openAlbumLabels) },
                        )
                    }
                },
            )
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) { content() }
    }
}
