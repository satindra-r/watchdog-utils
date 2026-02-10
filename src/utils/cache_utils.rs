use crate::config::Config;
use crate::services::github_service::fetch_and_decode_file;
use crate::utils::types::DiffAction;
use anyhow::Result;
use log::{info, warn};
use reqwest::header::{ACCEPT, USER_AGENT};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

// -- Helper Functions --
pub fn names_path(base_path: &Path, hash: &str) -> PathBuf {
    base_path.join("names").join(hash)
}

pub fn access_path(base_path: &Path, server: &str, group: &str, hash: &str) -> PathBuf {
    base_path.join("access").join(server).join(group).join(hash)
}

/// Updates a single file entry in the local cache based on its status.
/// Called by the external project after detecting a change.
pub fn update_local_cache(
    config: &Config,
    project: &str,
    first_child_directory: &str,
    hash: &str,
    status: &DiffAction,
    username: &str, // Username for 'names', ignored for 'access' unless needed later on
) -> Result<(), std::io::Error> {
    let cache_base_path = PathBuf::from(&config.cache_path);
    let (cache_file_path, content) = if first_child_directory.is_empty() {
        (names_path(&cache_base_path, hash), username)
    } else {
        (
            access_path(&cache_base_path, first_child_directory, project, hash),
            "1",
        )
    };

    match status {
        DiffAction::AddedGroup | DiffAction::AddedUser => {
            if let Some(parent) = cache_file_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&cache_file_path, content)?;
            info!("Cache Updated (Write): {cache_file_path:?}");
        }
        DiffAction::DeletedGroup | DiffAction::DeletedUser => {
            let _ = fs::remove_file(&cache_file_path);
            info!("Cache Updated (Remove): {cache_file_path:?}");
        }
        DiffAction::ModifiedUser => {
            //TODO handle this
        }
    }

    Ok(())
}

/// Performs a full synchronization, clearing the cache and rebuilding it from GitHub.
/// Called by the external project, perhaps on startup or periodically.
pub async fn sync_full_cache(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let cache_base_path = Path::new(&config.cache_path);
    if cache_base_path.exists() {
        fs::remove_dir_all(cache_base_path)?;
    }
    fs::create_dir_all(cache_base_path)?;
    info!("Cleared and initialized local cache for full sync at {cache_base_path:?}");

    let client = reqwest::Client::new();
    info!("Performing full cache sync. This might take a moment...");

    let tree_url = format!("{}/git/trees/build?recursive=1", config.keyhouse.base_url,);

    let resp: Value = client
        .get(&tree_url)
        .bearer_auth(&config.keyhouse.token)
        .header(USER_AGENT, "scout-server-cache-sync")
        .header(ACCEPT, "application/vnd.github.v3+json")
        .send()
        .await?
        .json()
        .await?;

    if let Some(tree) = resp["tree"].as_array() {
        for item in tree {
            if let (Some(path), Some(item_type)) = (item["path"].as_str(), item["type"].as_str()) {
                if item_type != "blob" {
                    continue;
                } // Skip directories/trees

                let path_parts: Vec<&str> = path.split('/').collect();

                if path.starts_with("access/") && path_parts.len() == 4 {
                    let provider_name = path_parts[1];
                    let project_name = path_parts[2];
                    let hash = path_parts[3];

                    update_local_cache(
                        config,
                        project_name,
                        provider_name,
                        hash,
                        &DiffAction::AddedGroup,
                        "",
                    )
                    .unwrap_or_else(|e| warn!("Failed to update access cache for {hash}: {e}"));
                } else if path.starts_with("names/") && path_parts.len() == 2 {
                    let hash = path_parts[1];
                    let username = fetch_and_decode_file(
                        &config.keyhouse.base_url,
                        &config.keyhouse.token,
                        hash,
                        &DiffAction::AddedUser,
                        "",
                    )
                    .await?;

                    update_local_cache(
                        config,
                        "",
                        "names",
                        hash,
                        &DiffAction::AddedUser,
                        &username,
                    )
                    .unwrap_or_else(|e| warn!("Failed to update names cache for {hash}: {e}"));
                }
            }
        }
    }
    info!("Full cache sync completed.");
    Ok(())
}
