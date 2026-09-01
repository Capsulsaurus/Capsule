use std::convert::Infallible;

use config::SyncServerConfig;
use eyre::Result;
use futures_util::StreamExt;
use http_body_util::BodyStream;
use salvo::cors::{AllowOrigin, Cors};
use salvo::http::{Method, ResBody, StatusError};
use salvo::prelude::*;
use salvo::{BoxedError, hyper};
use sea_orm::DatabaseConnection;
use tower::{Layer, Service};

pub mod config;
pub mod cursor;
pub mod federation;
pub mod feed;

#[cfg(test)]
mod tests;

pub use feed::SyncFeedService;

pub mod proto {
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

/// A Salvo handler that wraps a tonic gRPC service (the key-free `SyncService` rides it).
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

    // The key-free sync feed (SLICES.md S-C2). Mounted at its explicit service path. The
    // `tonic_web::GrpcWebLayer` wrap lets the
    // SAME service also answer browser gRPC-web calls (slice S-D6: the web gateway can only
    // speak gRPC-web, not native gRPC) — a key-free enabling wrap that neither forks the
    // service nor changes what it serves (the manifest/metadata stay opaque). Native gRPC
    // clients (SDK/CLI/federation peers) are unaffected: the layer only translates the
    // grpc-web content-types and passes `application/grpc` straight through.
    let sync_feed = GrpcHandler::new(tonic_web::GrpcWebLayer::new().layer(
        proto::capsule::sync::v1::sync_service_server::SyncServiceServer::new(
            SyncFeedService::new(conn, &config),
        ),
    ));

    // Browser gRPC-web needs CORS: the preflight (`OPTIONS`) and the exposed gRPC-web
    // status/trailer + protocol-advertisement headers. Scoped to the sync feed sub-router
    // only, mirroring the auth/upload routers' permissive-by-default origin handling. This
    // is browser-carriage plumbing; native gRPC clients never send an `Origin`.
    let allow_origin =
        if config.allowed_origins.is_empty() || config.allowed_origins.iter().any(|o| o == "*") {
            AllowOrigin::any()
        } else {
            AllowOrigin::from(&config.allowed_origins)
        };
    let cors = Cors::new()
        .allow_origin(allow_origin)
        .allow_methods(vec![Method::POST, Method::OPTIONS])
        .allow_headers("*")
        .expose_headers(vec![
            "grpc-status",
            "grpc-message",
            "grpc-status-details-bin",
            "x-capsule-error-code",
            "x-capsule-protocol-min",
            "x-capsule-protocol-max",
        ])
        .into_handler();

    // gRPC routes match the full path including the service name. Salvo wildcards use the
    // `{**rest}` wisp syntax; the tonic server does its own per-method dispatch under this
    // prefix.
    let router = Router::new().push(
        Router::with_path("capsule.sync.v1.SyncService/{**rest}")
            .hoop(cors)
            .goal(sync_feed),
    );

    Ok(router)
}
