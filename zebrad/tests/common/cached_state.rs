//! Utility functions for tests that used cached Zebra state.
//!
//! Note: we allow dead code in this module, because it is mainly used by the gRPC tests,
//! which are optional.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use color_eyre::eyre::{eyre, Result};
use tempfile::TempDir;
use tokio::fs;
use tower::{util::BoxService, Service};

use zebra_chain::{
    block::{self, Height},
    chain_tip::ChainTip,
    parameters::Network,
};
use zebra_state::{ChainTipChange, LatestChainTip};

use crate::common::config::testdir;

/// Path to a directory containing a cached Zebra state.
pub const ZEBRA_CACHED_STATE_DIR: &str = "ZEBRA_CACHED_STATE_DIR";

/// Type alias for a boxed state service.
pub type BoxStateService =
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>;

/// Starts a state service using the provided `cache_dir` as the directory with the chain state.
#[tracing::instrument(skip(cache_dir))]
pub async fn start_state_service_with_cache_dir(
    network: Network,
    cache_dir: impl Into<PathBuf>,
) -> Result<(
    BoxStateService,
    impl Service<
        zebra_state::ReadRequest,
        Response = zebra_state::ReadResponse,
        Error = zebra_state::BoxError,
    >,
    LatestChainTip,
    ChainTipChange,
)> {
    let config = zebra_state::Config {
        cache_dir: cache_dir.into(),
        ..zebra_state::Config::default()
    };

    // These tests don't need UTXOs to be verified efficiently, because they use cached states.
    Ok(zebra_state::init(config, network, Height::MAX, 0))
}

/// Loads the chain tip height from the state stored in a specified directory.
#[tracing::instrument]
pub async fn load_tip_height_from_state_directory(
    network: Network,
    state_path: &Path,
) -> Result<block::Height> {
    let (_state_service, _read_state_service, latest_chain_tip, _chain_tip_change) =
        start_state_service_with_cache_dir(network, state_path).await?;

    let chain_tip_height = latest_chain_tip
        .best_tip_height()
        .ok_or_else(|| eyre!("State directory doesn't have a chain tip block"))?;

    Ok(chain_tip_height)
}

/// Recursively copy a chain state database directory into a new temporary directory.
pub async fn copy_state_directory(network: Network, source: impl AsRef<Path>) -> Result<TempDir> {
    // Copy the database files for this state and network, excluding testnet and other state versions
    let source = source.as_ref();
    let state_config = zebra_state::Config {
        cache_dir: source.into(),
        ..Default::default()
    };
    let source_net_dir = state_config.db_path(network);
    let source_net_dir = source_net_dir.as_path();
    let state_suffix = source_net_dir
        .strip_prefix(source)
        .expect("db_path() is a subdirectory");

    let destination = testdir()?;
    let destination_net_dir = destination.path().join(state_suffix);

    tracing::info!(
        ?source,
        ?source_net_dir,
        ?state_suffix,
        ?destination,
        ?destination_net_dir,
        "copying cached state files (this may take some time)...",
    );

    let mut remaining_directories = vec![PathBuf::from(source_net_dir)];

    while let Some(directory) = remaining_directories.pop() {
        let sub_directories =
            copy_directory(&directory, source_net_dir, destination_net_dir.as_ref()).await?;

        remaining_directories.extend(sub_directories);
    }

    Ok(destination)
}

/// Copy the contents of a directory, and return the sub-directories it contains.
///
/// Copies all files from the `directory` into the destination specified by the concatenation of
/// the `base_destination_path` and `directory` stripped of its `prefix`.
#[tracing::instrument]
async fn copy_directory(
    directory: &Path,
    prefix: &Path,
    base_destination_path: &Path,
) -> Result<Vec<PathBuf>> {
    let mut sub_directories = Vec::new();
    let mut entries = fs::read_dir(directory).await?;

    let destination =
        base_destination_path.join(directory.strip_prefix(prefix).expect("Invalid path prefix"));

    fs::create_dir_all(&destination).await?;

    while let Some(entry) = entries.next_entry().await? {
        let entry_path = entry.path();
        let file_type = entry.file_type().await?;

        if file_type.is_file() {
            let file_name = entry_path.file_name().expect("Missing file name");
            let destination_path = destination.join(file_name);

            fs::copy(&entry_path, destination_path).await?;
        } else if file_type.is_dir() {
            sub_directories.push(entry_path);
        } else if file_type.is_symlink() {
            unimplemented!("Symbolic link support is currently not necessary");
        } else {
            panic!("Unknown file type");
        }
    }

    Ok(sub_directories)
}
