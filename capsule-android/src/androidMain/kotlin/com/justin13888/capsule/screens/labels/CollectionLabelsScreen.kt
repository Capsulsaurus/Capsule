package com.justin13888.capsule.screens.labels

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.justin13888.capsule.R

/** The labels of a stack or an album: the same [LabelList] a region's labels use. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CollectionLabelsScreen(
    viewModel: LabelEditorViewModel,
    navigateBack: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val title =
        when (viewModel.subject) {
            is LabelSubject.Album -> R.string.app_labels_title_album
            is LabelSubject.Stack -> R.string.app_labels_title_stack
            is LabelSubject.Asset -> R.string.app_labels_title_regions
        }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(title)) },
                navigationIcon = {
                    IconButton(onClick = navigateBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.back))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            LabelList(
                labels = state.labels,
                enabled = !state.isWriting,
                onAdd = viewModel::addLabel,
                onRemove = viewModel::removeLabel,
            )
            LabelFailureText(state.failure)
        }
    }
}
