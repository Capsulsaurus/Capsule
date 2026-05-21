use config::SyncServerConfig;
use eyre::Result;
use futures_util::{Stream, StreamExt};
use http_body_util::BodyExt;
use salvo::BoxedError;
use salvo::http::{ResBody, StatusError};
use salvo::hyper;
use salvo::prelude::*;
use sea_orm::DatabaseConnection;
use std::convert::Infallible;
use tonic::{Request, Response, Status};
use tower::Service;

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

pub mod config;

pub mod proto {
    pub mod photolibrary {
        pub mod metadata {
            pub mod v1 {
                tonic::include_proto!("photolibrary.metadata.v1");
            }
        }
    }
}

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

/// A Salvo handler that wraps the gRPC service
#[derive(Clone)]
pub struct GrpcHandler {
    service: PhotoLibraryMetadataServiceServer<CapsuleMetadataService>,
}

impl GrpcHandler {
    pub fn new(service: PhotoLibraryMetadataServiceServer<CapsuleMetadataService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl Handler for GrpcHandler {
    async fn handle(
        &self,
        req: &mut salvo::Request,
        _depot: &mut Depot,
        res: &mut salvo::Response,
        _ctrl: &mut FlowCtrl,
    ) {
        let mut svc = self.service.clone();

        // Convert Salvo request to hyper request
        let hyper_req: hyper::Request<salvo::http::ReqBody> = match req.strip_to_hyper() {
            Ok(r) => r,
            Err(_) => {
                res.render(StatusError::internal_server_error());
                return;
            }
        };

        // Call the gRPC service
        let result: Result<hyper::Response<tonic::body::Body>, Infallible> =
            svc.call(hyper_req).await;
        match result {
            Ok(hyper_res) => {
                // Extract parts and body
                let (parts, body) = hyper_res.into_parts();

                // Convert gRPC body to a stream of BytesFrames for ResBody::Stream
                let stream = body.into_data_stream().map(|result| {
                    result
                        .map(salvo::http::body::BytesFrame::from)
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
    _conn: DatabaseConnection,
    _config: C,
) -> Result<Router> {
    let service = CapsuleMetadataService::default();
    let grpc_service = PhotoLibraryMetadataServiceServer::new(service);
    let handler = GrpcHandler::new(grpc_service);

    // gRPC routes need to match the full path including the service name
    let router = Router::new().push(Router::with_path("<**rest>").goal(handler));

    Ok(router)
}
