use async_graphql::*;
use async_stream::stream;
use futures_util::Stream;

#[derive(Default)]
pub struct AssetSubscription;

#[Subscription]
impl AssetSubscription {
    async fn upload_session_progress(&self) -> impl Stream<Item = i32> {
        stream! {
            // TODO: IMPLEMENT
            yield 1;
        }
    }
}
