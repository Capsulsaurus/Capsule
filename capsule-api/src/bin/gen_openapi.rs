use clap::Parser;
use environment::Environment;
use eyre::Result;
use capsule_api::{create_openapi_spec, create_router};
use sea_orm::Database;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Optional output path for the OpenAPI spec
    #[arg(value_name = "FILE")]
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    color_eyre::install()?;

    // Load environment settings
    let env = Environment::load()
        .map_err(|e| eyre::eyre!("Failed to load environment settings: {:?}", e))?;

    // Initialize database connection
    // Note: This requires a running database. For SDK generation in CI/CD,
    // we might need to mock this or ensure DB is present.
    let conn = Database::connect(env.database.url.clone()).await?;

    // Create the app router
    let router = create_router(conn, &env).await?;

    // Build OpenAPI spec by merging with router
    let api = create_openapi_spec().merge_router(&router);

    // Serialize to JSON
    let json = serde_json::to_string_pretty(&api)?;

    // Write to file
    let path = cli.output.unwrap_or_else(|| PathBuf::from("openapi.json"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = File::create(&path)?;
    file.write_all(json.as_bytes())?;

    println!("Generated JSON at: {}", path.display());
    Ok(())
}
