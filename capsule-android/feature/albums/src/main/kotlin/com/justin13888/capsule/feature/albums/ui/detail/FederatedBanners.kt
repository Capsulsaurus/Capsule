package com.justin13888.capsule.feature.albums.ui.detail

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.justin13888.capsule.core.designsystem.component.CapsuleBanner
import com.justin13888.capsule.core.designsystem.component.CapsuleBannerSeverity
import com.justin13888.capsule.core.i18n.R
import com.justin13888.capsule.feature.albums.model.FederatedAlbum
import com.justin13888.capsule.feature.albums.ui.common.AlbumsTestTags
import com.justin13888.capsule.feature.albums.ui.common.withArgs

/**
 * What an aggregated federated album has to admit about itself.
 *
 * Two things, and both are stated rather than implied. First, the aggregate is a *view* of N
 * separate albums, each sovereign to its own contributor — so you can stop someone seeing
 * your photos, and you cannot remove theirs. Second, an origin that did not answer degrades
 * **visibly**: its entries render from the local index and nothing is removed, which is only
 * reassuring if the screen says so.
 *
 * One banner per unreachable constituent rather than one summary banner, because per-origin
 * containment is the point: `photos.example.org` being down says nothing about
 * `capsule.example.net`, and a merged message would imply otherwise.
 */
@Composable
internal fun FederatedBanners(
    group: FederatedAlbum,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.testTag(AlbumsTestTags.FEDERATED)) {
        CapsuleBanner(
            title = stringResource(R.string.albums_federated_badge),
            modifier = Modifier.padding(horizontal = GUTTER, vertical = GUTTER / 2),
            body = stringResource(R.string.albums_federated_explainer),
            severity = CapsuleBannerSeverity.Info,
        )
        group.unavailable.forEach { constituent ->
            CapsuleBanner(
                title =
                    stringResource(R.string.albums_federated_unavailable)
                        .withArgs("server" to constituent.homeServer),
                modifier =
                    Modifier
                        .padding(horizontal = GUTTER, vertical = GUTTER / 2)
                        .testTag("${AlbumsTestTags.FEDERATED}:${constituent.homeServer}"),
                body = stringResource(R.string.albums_federated_unavailable_body),
                severity = CapsuleBannerSeverity.Degraded,
            )
        }
    }
}
