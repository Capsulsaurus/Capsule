pub mod commands;

use clap::Parser;
pub use commands::*;

#[derive(Parser, Debug)]
#[command(name = "capsule")]
#[command(about = "A command line interface for Capsule - the photo management platform")]
#[command(
    long_about = "Capsule CLI provides tools for managing your photos and albums:\n• Authentication management\n• Import photos from local directories\n• Sync local and remote data\n• Check status and list files\n• Manage albums and collections"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}
