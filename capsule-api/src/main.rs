use environment::Environment;
use eyre::{Result, eyre};
use listenfd::ListenFd;
use migration::{Migrator, MigratorTrait};
use capsule_api::create_router;
use salvo::{conn::tcp::TcpAcceptor, prelude::*};
use sea_orm::Database;
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio::net::TcpListener;
use tracing::{debug, info};
use tracing_subscriber::fmt::format::FmtSpan;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Load environment settings
    let env =
        Environment::load().map_err(|e| eyre!("Failed to load environment settings: {:?}", e))?;

    // Set up logging
    if cfg!(debug_assertions) {
        // Development configuration: Pretty printing with colors
        tracing_subscriber::fmt()
            .with_max_level(env.log_level)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_span_events(FmtSpan::FULL)
            .with_ansi(true)
            .pretty()
            .init();
    } else {
        // Production configuration: JSON format
        tracing_subscriber::fmt()
            .with_max_level(env.log_level)
            .with_target(true)
            .with_file(true)
            .with_span_events(FmtSpan::FULL)
            .json()
            .init();
    }

    debug!("Environment settings loaded: {:?}", env);

    // Initialize database connection
    let conn = Database::connect(env.database.url.clone()).await?;

    // Run auto migration in dev
    #[cfg(debug_assertions)]
    {
        Migrator::up(&conn, None).await?;
    }

    // Build app
    let router = create_router(conn, &env).await?;

    let addr = SocketAddrV4::new(env.server.host.parse::<Ipv4Addr>()?, env.server.port);

    // Start server
    info!(
        "Starting server on http://{}:{}/",
        env.server.host, env.server.port
    );

    // Setup listenfd
    let mut listenfd = ListenFd::from_env();
    let listener = match listenfd.take_tcp_listener(0).unwrap() {
        // if we are given a tcp listener on listen fd 0, we use that one
        Some(listener) => {
            listener.set_nonblocking(true).unwrap();
            TcpListener::from_std(listener).unwrap()
        }
        // otherwise fall back to local listening
        None => TcpListener::bind(addr).await?,
    };
    let acceptor = TcpAcceptor::try_from(listener)?;

    // Serve with HTTP/2 cleartext (h2c) support for gRPC
    // The 'http2' and 'http2-cleartext' features in Cargo.toml enable H2C
    info!("Server listening on {}", addr);
    Server::new(acceptor).serve(router).await;

    Ok(())
}
