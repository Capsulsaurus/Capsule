use std::convert::Infallible;

use config::SyncServerConfig;
use eyre::Result;
use futures_util::{Stream, StreamExt};
use http_body_util::BodyStream;
use proto::photolibrary::metadata::v1::photo_library_metadata_service_server::{
    PhotoLibraryMetadataService, PhotoLibraryMetadataServiceServer,
};
use proto::photolibrary::metadata::v1::{
    CreateAlbumRequest, CreateAlbumResponse, CreatePhotoMetadataRequest,
    CreatePhotoMetadataResponse, CreateTagRequest, CreateTagResponse, DeleteAlbumRequest,
    DeleteAlbumResponse, DeletePhotoRequest, DeletePhotoResponse, DeleteTagRequest,
    DeleteTagResponse, GetAlbumRequest, GetAlbumResponse, GetPhotoRequest, GetPhotoResponse,
    GetTagRequest, GetTagResponse, ListAlbumsRequest, ListAlbumsResponse, ListPhotosRequest,
    ListPhotosResponse, ListTagsRequest, ListTagsResponse, SyncMetadataRequest,
    SyncMetadataResponse, UpdateAlbumRequest, UpdateAlbumResponse, UpdatePhotoMetadataRequest,
    UpdatePhotoMetadataResponse,
};
use salvo::http::{ResBody, StatusError};
use salvo::prelude::*;
use salvo::{BoxedError, hyper};
use sea_orm::DatabaseConnection;
use tonic::{Request, Response, Status};
use tower::Service;

pub mod config;
pub mod cursor;
pub mod federation;
pub mod feed;

#[cfg(test)]
mod tests;

pub use feed::SyncFeedService;

pub mod proto {
    // LEGACY-PLAINTEXT (frozen): SLICES.md S-G2 — the pre-E2EE metadata service. It
    // models plaintext photos/albums/tags the key-free server never sees; it is kept
    // compiling but frozen, and retires once `capsule.sync.v1` reaches parity.
    pub mod photolibrary {
        pub mod metadata {
            pub mod v1 {
                tonic::include_proto!("photolibrary.metadata.v1");
            }
        }
    }

    /// The key-free sync feed (contract skeleton — slice `S-C2` in the repo-root
    /// `SLICES.md`; SSoT: <https://docs/design/import/download-sync/>).
    pub mod capsule {
        pub mod sync {
            pub mod v1 {
                tonic::include_proto!("capsule.sync.v1");
            }
        }
    }
}

// LEGACY-PLAINTEXT (frozen): SLICES.md S-G2 — see the proto module note above.
#[derive(Default, Debug, Clone)]
pub struct CapsuleMetadataService {
    // Inject DB or Config here if needed
}

type SyncMetadataStream =
    std::pin::Pin<Box<dyn Stream<Item = Result<SyncMetadataResponse, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl PhotoLibraryMetadataService for CapsuleMetadataService {
    async fn list_photos(
        &self,
        _request: Request<ListPhotosRequest>,
    ) -> Result<Response<ListPhotosResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn get_photo(
        &self,
        _request: Request<GetPhotoRequest>,
    ) -> Result<Response<GetPhotoResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn create_photo_metadata(
        &self,
        _request: Request<CreatePhotoMetadataRequest>,
    ) -> Result<Response<CreatePhotoMetadataResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn update_photo_metadata(
        &self,
        _request: Request<UpdatePhotoMetadataRequest>,
    ) -> Result<Response<UpdatePhotoMetadataResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn delete_photo(
        &self,
        _request: Request<DeletePhotoRequest>,
    ) -> Result<Response<DeletePhotoResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn list_albums(
        &self,
        _request: Request<ListAlbumsRequest>,
    ) -> Result<Response<ListAlbumsResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn get_album(
        &self,
        _request: Request<GetAlbumRequest>,
    ) -> Result<Response<GetAlbumResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn create_album(
        &self,
        _request: Request<CreateAlbumRequest>,
    ) -> Result<Response<CreateAlbumResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn update_album(
        &self,
        _request: Request<UpdateAlbumRequest>,
    ) -> Result<Response<UpdateAlbumResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn delete_album(
        &self,
        _request: Request<DeleteAlbumRequest>,
    ) -> Result<Response<DeleteAlbumResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn list_tags(
        &self,
        _request: Request<ListTagsRequest>,
    ) -> Result<Response<ListTagsResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn get_tag(
        &self,
        _request: Request<GetTagRequest>,
    ) -> Result<Response<GetTagResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn create_tag(
        &self,
        _request: Request<CreateTagRequest>,
    ) -> Result<Response<CreateTagResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    async fn delete_tag(
        &self,
        _request: Request<DeleteTagRequest>,
    ) -> Result<Response<DeleteTagResponse>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }

    type SyncMetadataStream = SyncMetadataStream;

    async fn sync_metadata(
        &self,
        _request: Request<SyncMetadataRequest>,
    ) -> Result<Response<Self::SyncMetadataStream>, Status> {
        Err(Status::unimplemented("Not implemented yet"))
    }
}

/// A Salvo handler that wraps a tonic gRPC service (the legacy metadata service and the
/// key-free `SyncService` both ride it).
#[derive(Clone)]
pub struct GrpcHandler<S> {
    service: S,
}

impl<S> GrpcHandler<S> {
    pub fn new(service: S) -> Self {
        Self { service }
    }
}

#[async_trait]
impl<S> Handler for GrpcHandler<S>
where
    S: Service<
            hyper::Request<salvo::http::ReqBody>,
            Response = hyper::Response<tonic::body::Body>,
            Error = Infallible,
        > + Clone
        + Send
        + Sync
        + 'static,
    S::Future: Send,
{
    async fn handle(
        &self,
        req: &mut salvo::Request,
        _depot: &mut Depot,
        res: &mut salvo::Response,
        _ctrl: &mut FlowCtrl,
    ) {
        let mut svc = self.service.clone();

        // Convert Salvo request to hyper request
        let hyper_req: hyper::Request<salvo::http::ReqBody> = if let Ok(r) = req.strip_to_hyper() {
            r
        } else {
            res.render(StatusError::internal_server_error());
            return;
        };

        // Call the gRPC service
        let result: Result<hyper::Response<tonic::body::Body>, Infallible> =
            svc.call(hyper_req).await;
        match result {
            Ok(hyper_res) => {
                // Extract parts and body
                let (parts, body) = hyper_res.into_parts();

                // Stream ALL body frames (DATA *and* TRAILERS) so the gRPC status trailer
                // survives the bridge — `into_data_stream()` would drop it and the client
                // would see a truncated stream with no final status.
                let stream = BodyStream::new(body).map(|result| {
                    result
                        .map(salvo::http::body::BytesFrame)
                        .map_err(|e| Box::new(e) as BoxedError)
                });

                // Reconstruct response with Stream body
                let stream_body = ResBody::Stream(sync_wrapper::SyncWrapper::new(Box::pin(stream)));
                let mut new_res = hyper::Response::from_parts(parts, stream_body);

                // Copy status and headers
                res.status_code(new_res.status());
                res.headers_mut().extend(new_res.headers_mut().drain());
                res.body = std::mem::take(new_res.body_mut());
            }
            Err(infallible) => match infallible {},
        }
    }
}

/// Get router with gRPC service wrapped for Salvo
pub async fn get_router<C: Into<SyncServerConfig>>(
    conn: DatabaseConnection,
    config: C,
) -> Result<Router> {
    let config = config.into();

    // The key-free sync feed (SLICES.md S-C2). Mounted at its explicit service path BEFORE
    // the legacy catch-all so it wins matching.
    let sync_feed = GrpcHandler::new(
        proto::capsule::sync::v1::sync_service_server::SyncServiceServer::new(
            SyncFeedService::new(conn, &config),
        ),
    );

    // LEGACY-PLAINTEXT (frozen): SLICES.md S-G2.
    let service = CapsuleMetadataService::default();
    let grpc_service = PhotoLibraryMetadataServiceServer::new(service);
    let handler = GrpcHandler::new(grpc_service);

    // gRPC routes match the full path including the service name. Salvo wildcards use the
    // `{**rest}` wisp syntax; the sync service path is matched BEFORE the legacy catch-all so
    // it wins. (The tonic server does its own per-method dispatch under this prefix.)
    let router = Router::new()
        .push(Router::with_path("capsule.sync.v1.SyncService/{**rest}").goal(sync_feed))
        .push(Router::with_path("{**rest}").goal(handler));

    Ok(router)
}
