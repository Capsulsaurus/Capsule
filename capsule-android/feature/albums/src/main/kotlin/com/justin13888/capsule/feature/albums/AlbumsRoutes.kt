package com.justin13888.capsule.feature.albums

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.core.model.SmartAlbumId
import com.justin13888.capsule.core.model.StackId
import com.justin13888.capsule.feature.albums.model.CollectionRef
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailActions
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailScreen
import com.justin13888.capsule.feature.albums.ui.detail.AlbumDetailViewModel
import com.justin13888.capsule.feature.albums.ui.duplicates.DuplicatesScreen
import com.justin13888.capsule.feature.albums.ui.duplicates.DuplicatesViewModel
import com.justin13888.capsule.feature.albums.ui.hidden.HiddenScreen
import com.justin13888.capsule.feature.albums.ui.hidden.HiddenViewModel
import com.justin13888.capsule.feature.albums.ui.hub.CollectionsHubScreen
import com.justin13888.capsule.feature.albums.ui.hub.CollectionsHubViewModel
import com.justin13888.capsule.feature.albums.ui.hub.GatedSection
import com.justin13888.capsule.feature.albums.ui.places.PlacesScreen
import com.justin13888.capsule.feature.albums.ui.places.PlacesViewModel
import com.justin13888.capsule.feature.albums.ui.smart.RuleTreeActions
import com.justin13888.capsule.feature.albums.ui.smart.SmartAlbumBuilderScreen
import com.justin13888.capsule.feature.albums.ui.smart.SmartAlbumBuilderViewModel
import com.justin13888.capsule.feature.albums.ui.smart.SmartAlbumsScreen
import com.justin13888.capsule.feature.albums.ui.smart.SmartAlbumsViewModel
import com.justin13888.capsule.feature.albums.ui.stacks.StackDetailActions
import com.justin13888.capsule.feature.albums.ui.stacks.StackDetailScreen
import com.justin13888.capsule.feature.albums.ui.stacks.StackDetailViewModel
import com.justin13888.capsule.feature.albums.ui.stacks.StacksScreen
import com.justin13888.capsule.feature.albums.ui.stacks.StacksViewModel
import com.justin13888.capsule.feature.albums.ui.trash.RecentlyDeletedScreen
import com.justin13888.capsule.feature.albums.ui.trash.RecentlyDeletedViewModel
import org.koin.androidx.compose.koinViewModel
import org.koin.core.parameter.parametersOf

/*
 * The entry points the app module routes at.
 *
 * Every id crosses this boundary as a `String`, because that is how it travels through the
 * navigation key: a nav key is serialized reflectively, and an inline value class over
 * `String` is a needless hazard there for type safety that does not survive the round trip.
 * Re-wrapping happens here, at the one place the two halves meet.
 *
 * Each route is a thin shell: resolve the view model, collect its state, hand the stateless
 * screen its callbacks. Everything worth testing is in the screen or the view model, and both
 * can be exercised without any of this.
 */

/** The Collections hub. */
@Composable
fun CollectionsHubRoute(
    onOpenCollection: (String) -> Unit,
    onOpenSmartAlbums: () -> Unit,
    onOpenDuplicates: () -> Unit,
    onOpenPlaces: () -> Unit,
    onOpenTrash: () -> Unit,
    onOpenHidden: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: CollectionsHubViewModel = koinViewModel()
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    CollectionsHubScreen(
        state = state,
        onOpen = { ref -> onOpenCollection(ref.encode()) },
        onOpenSmartAlbums = onOpenSmartAlbums,
        onOpenDuplicates = onOpenDuplicates,
        onOpenPlaces = onOpenPlaces,
        onOpenGated = { section ->
            when (section) {
                GatedSection.Trash -> onOpenTrash()
                GatedSection.Hidden -> onOpenHidden()
            }
        },
        onCreateAlbum = viewModel::createAlbum,
        onRetry = viewModel::refreshCounts,
        modifier = modifier,
    )
}

/**
 * Whatever the album destination is currently pointing at.
 *
 * One destination covers a container, a built-in view, the stacks listing and an aggregate,
 * so the dispatch lives here rather than in the shell's sealed hierarchy — see
 * [CollectionRef] for why the id carries its own tiny grammar.
 */
@Composable
fun AlbumRoute(
    albumId: String,
    onBack: () -> Unit,
    onOpenAsset: (String) -> Unit,
    onOpenStack: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    when (val ref = CollectionRef.parse(albumId)) {
        CollectionRef.Stacks -> StacksRoute(onBack = onBack, onOpenStack = onOpenStack, modifier = modifier)
        else -> AlbumDetailRoute(ref = ref, onBack = onBack, onOpenAsset = onOpenAsset, modifier = modifier)
    }
}

@Composable
private fun AlbumDetailRoute(
    ref: CollectionRef,
    onBack: () -> Unit,
    onOpenAsset: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: AlbumDetailViewModel = koinViewModel(key = ref.encode()) { parametersOf(ref) }
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    AlbumDetailScreen(
        state = state,
        actions =
            AlbumDetailActions(
                onBack = onBack,
                onOpenAsset = { assetId -> onOpenAsset(assetId.value) },
                onRename = viewModel::rename,
                onDelete = { viewModel.delete(onBack) },
                onMakeDefault = viewModel::makeDefault,
                onInvite = viewModel::invite,
                onRemoveMember = viewModel::removeMember,
                onSetCapability = viewModel::setCapability,
                onStartUpgrade = viewModel::startUpgrade,
                onAdvanceUpgrade = viewModel::advanceUpgrade,
                onAbortUpgrade = viewModel::abortUpgrade,
                onDismissMessage = viewModel::dismissMessage,
                onRetry = viewModel::load,
            ),
        modifier = modifier,
    )
}

/** The listing of every stack in the library. */
@Composable
fun StacksRoute(
    onBack: () -> Unit,
    onOpenStack: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: StacksViewModel = koinViewModel()
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    StacksScreen(
        state = state,
        onBack = onBack,
        onOpen = { stackId -> onOpenStack(stackId.value) },
        onRetry = viewModel::load,
        modifier = modifier,
    )
}

/** One stack, expanded into its members. */
@Composable
fun StackDetailRoute(
    stackId: String,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: StackDetailViewModel = koinViewModel(key = stackId) { parametersOf(StackId(stackId)) }
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    StackDetailScreen(
        state = state,
        actions =
            StackDetailActions(
                onBack = onBack,
                onSetPrimary = viewModel::setPrimary,
                onMoveEarlier = viewModel::moveEarlier,
                onMoveLater = viewModel::moveLater,
                onRetry = viewModel::load,
            ),
        modifier = modifier,
    )
}

/** The saved searches that present themselves as albums. */
@Composable
fun SmartAlbumsRoute(
    onBack: () -> Unit,
    onEditRule: (String?) -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: SmartAlbumsViewModel = koinViewModel()
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    SmartAlbumsScreen(
        state = state,
        onBack = onBack,
        onCreate = { onEditRule(null) },
        onEdit = { id -> onEditRule(id.value) },
        onDelete = viewModel::delete,
        onRetry = onBack,
        modifier = modifier,
    )
}

/** The rule builder behind a smart album. */
@Composable
fun SmartAlbumBuilderRoute(
    smartAlbumId: String?,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: SmartAlbumBuilderViewModel =
        koinViewModel(key = smartAlbumId ?: NEW_RULE_KEY) {
            parametersOf(smartAlbumId?.let(::SmartAlbumId))
        }
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    SmartAlbumBuilderScreen(
        state = state,
        actions =
            RuleTreeActions(
                onAddCondition = viewModel::addCondition,
                onAddGroup = viewModel::addGroup,
                onRemove = viewModel::remove,
                onNegate = viewModel::setNegated,
                onGroupKind = viewModel::setGroupKind,
                onField = viewModel::setField,
                onOperator = viewModel::setOperator,
                onOperand = viewModel::setOperand,
                onModelSlot = viewModel::setModelSlot,
            ),
        onBack = onBack,
        onName = viewModel::setName,
        onSortKey = viewModel::setSortKey,
        onSortDirection = viewModel::setSortDirection,
        onSave = viewModel::save,
        onDismissRefusal = viewModel::dismissRefusal,
        modifier = modifier,
    )
}

/** Recently Deleted. Composed only after the shell's local-auth gate has been passed. */
@Composable
fun RecentlyDeletedRoute(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: RecentlyDeletedViewModel = koinViewModel()
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    RecentlyDeletedScreen(
        state = state,
        onBack = onBack,
        onRestore = viewModel::restore,
        onEmptyTrash = viewModel::emptyTrash,
        onDismissRefusal = viewModel::dismissRefusal,
        onRetry = viewModel::load,
        modifier = modifier,
    )
}

/** The Hidden view. Composed only after the shell's local-auth gate has been passed. */
@Composable
fun HiddenRoute(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: HiddenViewModel = koinViewModel()
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    HiddenScreen(
        state = state,
        onBack = onBack,
        onUnhide = viewModel::unhide,
        onRetry = viewModel::load,
        modifier = modifier,
    )
}

/** Candidate duplicates awaiting a keep-or-drop decision. */
@Composable
fun DuplicatesRoute(
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: DuplicatesViewModel = koinViewModel()
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    DuplicatesScreen(
        state = state,
        onBack = onBack,
        onKeepOnly = viewModel::keepOnly,
        onRetry = viewModel::scan,
        modifier = modifier,
    )
}

/** The library laid out by where it was taken. */
@Composable
fun PlacesRoute(
    onBack: () -> Unit,
    onOpenAsset: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val viewModel: PlacesViewModel = koinViewModel()
    val state by viewModel.uiState.collectAsStateWithLifecycle()
    PlacesScreen(
        state = state,
        onBack = onBack,
        onSelect = viewModel::select,
        onOpenAsset = { assetId -> onOpenAsset(assetId.value) },
        onRetry = viewModel::load,
        modifier = modifier,
    )
}

/** The view-model key a brand-new rule is scoped under, so it is not shared with an edit. */
private const val NEW_RULE_KEY = "albums.builder.new"
