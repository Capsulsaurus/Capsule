package com.justin13888.capsule.feature.viewer.di

import com.justin13888.capsule.core.model.AlbumId
import com.justin13888.capsule.core.model.AssetId
import com.justin13888.capsule.feature.viewer.ViewerArgs
import com.justin13888.capsule.feature.viewer.ViewerViewModel
import com.justin13888.capsule.feature.viewer.editor.MetadataEditorViewModel
import com.justin13888.capsule.feature.viewer.info.AssetInfoViewModel
import com.justin13888.capsule.feature.viewer.provenance.ProvenanceViewModel
import org.koin.core.module.Module
import org.koin.core.module.dsl.viewModel
import org.koin.dsl.module

/**
 * The viewer's bindings.
 *
 * Declared by the feature rather than in the app's shared DI file, so a slice contributes its
 * own graph without touching a file every other slice also edits, and so a reader can check
 * the module against the view models it constructs without changing directory.
 *
 * Identifiers arrive as `String` because a nav key is serialized reflectively and an inline
 * value class over `String` is a needless hazard on that path. They are re-wrapped here, at
 * the feature boundary — the last point before the value reaches code that cares which kind
 * of id it is holding.
 */
val viewerFeatureModule: Module =
    module {
        viewModel { parameters ->
            val args = parameters.get<ViewerArgs>()
            ViewerViewModel(
                openAt = AssetId(args.assetId),
                contextAlbum = args.contextId?.let(::AlbumId),
                assets = get(),
                timeline = get(),
                albums = get(),
                sync = get(),
                sessions = get(),
            )
        }
        viewModel { parameters ->
            AssetInfoViewModel(
                assetId = AssetId(parameters.get<String>()),
                assets = get(),
                storage = get(),
                sync = get(),
                albums = get(),
            )
        }
        viewModel { parameters ->
            ProvenanceViewModel(
                assetId = AssetId(parameters.get<String>()),
                assets = get(),
                sessions = get(),
            )
        }
        viewModel { parameters ->
            MetadataEditorViewModel(
                assetId = AssetId(parameters.get<String>()),
                assets = get(),
                metadata = get(),
            )
        }
    }
