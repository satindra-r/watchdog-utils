use crate::config::Config;
use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
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

/// Fetches and decodes the content of a specific file from the GitHub repo.
/// This is needed for the full sync and potentially by the external caller.
/// Optional (can be removed from here if sync_full_cache is not needed) -- regie
pub async fn fetch_and_decode_file_from_github(
    config: &Config,
    project: &str,
    first_child_directory: &str,
    hash: &str,
    commit_ref: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let path = if project.is_empty() && first_child_directory == "names" {
        format!("names/{hash}")
    } else {
        format!("access/{first_child_directory}/{project}/{hash}")
    };
    let url = format!(
        "{}/contents/{}?ref={}",
        config.keyhouse.base_url, path, commit_ref
    );

    let client = reqwest::Client::new();
    let file_resp = client
        .get(&url)
        .bearer_auth(&config.keyhouse.token)
        .header(USER_AGENT, "scout-cache-util") // Updated user agent
        .header(ACCEPT, "application/vnd.github.v3+json")
        .send()
        .await?;

    if !file_resp.status().is_success() {
        warn!(
            "GitHub API error fetching file at path {}: {}",
            path,
            file_resp.status()
        );
        return Ok(None);
    }
    let file_json = file_resp.json::<serde_json::Value>().await?;
    if let Some(base64_content) = file_json["content"].as_str() {
        let clean_base64 = base64_content.replace('\n', "");
        let decoded = general_purpose::STANDARD.decode(&clean_base64)?;
        let decoded_str = String::from_utf8(decoded)?;
        info!("Decoded file content for path {path}");
        Ok(Some(decoded_str))
    } else {
        warn!("No 'content' field found for file at path {path}");
        Ok(None)
    }
}

/// Updates a single file entry in the local cache based on its status.
/// Called by the external project after detecting a change.
pub fn update_local_cache(
    config: &Config,
    project: &str,
    first_child_directory: &str,
    hash: &str,
    status: &str,
    content: &str, // Username for 'names', ignored for 'access' unless needed later on
) -> Result<(), std::io::Error> {
    let cache_base_path = PathBuf::from(&config.cache_path);

    let cache_file_path = if project.is_empty() && first_child_directory == "names" {
        names_path(&cache_base_path, hash)
    } else {
        access_path(&cache_base_path, first_child_directory, project, hash)
    };

    if status == "added" || status == "modified" {
        if let Some(parent) = cache_file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Write "1" for access files, username for names files
        let content_to_write = if first_child_directory == "names" {
            content
        } else {
            "1"
        };
        fs::write(&cache_file_path, content_to_write)?;
        info!("Cache Updated (Write): {cache_file_path:?}");
    } else if status == "deleted" || status == "deleteduser" {
        let _ = fs::remove_file(&cache_file_path);
        info!("Cache Updated (Remove): {cache_file_path:?}");
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

    let tree_url = format!(
        "{}/git/trees/{}?recursive=1",
        config.keyhouse.base_url, config.branch
    );

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

                    // For access files, just need to create the marker file. Content is "1".
                    update_local_cache(config, project_name, provider_name, hash, "added", "1")
                        .unwrap_or_else(|e| warn!("Failed to update access cache for {hash}: {e}"));
                } else if path.starts_with("names/") && path_parts.len() == 2 {
                    let hash = path_parts[1];
                    // Fetch the actual username content for names files using the moved function
                    if let Some(username) =
                        fetch_and_decode_file_from_github(config, "", "names", hash, &config.branch)
                            .await?
                    {
                        update_local_cache(config, "", "names", hash, "added", &username)
                            .unwrap_or_else(|e| {
                                warn!("Failed to update names cache for {hash}: {e}")
                            });
                    } else {
                        warn!("Could not fetch content for names file: {hash}");
                    }
                }
            }
        }
    }
    info!("Full cache sync completed.");
    Ok(())
}
