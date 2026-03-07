use crate::config::{Config, get_log_target, init, set_log_target};
use crate::models::commit_info::CommitInfo;
use crate::services::user_service::add_user_to_group;
use crate::services::user_service::delete_user;
use crate::services::user_service::remove_user_from_group;
use crate::utils::cache_utils::{names_path, sync_full_cache, update_local_cache};
use anyhow::{Result, anyhow};
use log::{error, info, warn};
use regex::Regex;
use reqwest::Client;
use reqwest::header::{ACCEPT, USER_AGENT};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::{fs, io};

pub async fn process_full_update_request(
    config: Config,
    update_log_target: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    set_log_target(update_log_target.to_string());

    let _ = init(&config);

    let base_url = &config.keyhouse.base_url;
    let token = &config.keyhouse.token;
    let hostname = &config.hostname;

    info!(target:get_log_target(), "Starting Full Sync...");
    sync_full_cache(&config).await?;
    update_all_users_from_cache(hostname, &config).await?;

    match fetch_latest_commit(base_url, token).await {
        Ok(latest_commit) => {
            fs::write("/opt/watchdog/bin/base_commit.txt", &latest_commit)?;
        }
        Err(e) => {
            warn!(target:get_log_target(), "Failed to fetch latest commit: {}. Using cache for updates.", e);
            return Ok(());
        }
    }
    Ok(())
}

pub async fn process_update_request(
    config: Config,
    update_log_target: &str,
    allow_fallback: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    set_log_target(update_log_target.to_string());

    let _ = init(&config);

    let base_url = &config.keyhouse.base_url;
    let token = &config.keyhouse.token;
    let hostname = &config.hostname;

    let last_commit;

    if !Path::new("/opt/watchdog/bin/base_commit.txt").exists() {
        error!(target:get_log_target(), "Last commit not found.");
        return if allow_fallback {
            process_full_update_request(config, update_log_target).await
        } else {
            Err(Box::new(io::Error::new(
                io::ErrorKind::NotFound,
                "Last commit not found. Please provide a base_commit.txt or run with --all",
            )))
        };
    } else {
        last_commit = fs::read_to_string("/opt/watchdog/bin/base_commit.txt")?;
        if last_commit.trim().is_empty() {
            error!(target:get_log_target(), "Last commit is empty.");
            return if allow_fallback {
                process_full_update_request(config, update_log_target).await
            } else {
                Err(Box::new(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Last commit is empty. Please provide a valid base_commit.txt or run with --all",
                )))
            };
        }
    }

    // Try to fetch from GitHub, fall back to cache on auth failure
    match fetch_recent_commit(base_url, token).await {
        Ok(merge_commit) => match fetch_diff(base_url, &last_commit, &merge_commit, token).await {
            Ok(diff) => {
                info!(target:get_log_target(), "Fetched diff from GitHub");
                process_diff(&diff, hostname, &config, &last_commit, &merge_commit).await?;
                fs::write("/opt/watchdog/bin/base_commit.txt", &merge_commit)?;
            }
            Err(e) => {
                warn!(target:get_log_target(), "Failed to fetch diff: {}. Updating from cache.", e);
                update_all_users_from_cache(hostname, &config).await?;
            }
        },
        Err(e) => {
            warn!(target:get_log_target(), "Failed to fetch recent commit: {}. Updating from cache.", e);
            update_all_users_from_cache(hostname, &config).await?;
        }
    }

    info!(target:get_log_target(), "Update process completed.");
    Ok(())
}

async fn process_diff(
    diff: &str,
    hostname: &str,
    config: &Config,
    last_commit: &str,
    _merge_commit: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for (cloud_provider, project, hash, status) in extract_diff_parts(diff) {
        info!(target:get_log_target(),
            "Parsed diff - Project: {}, Cloud Provider: {}, Hash: {}, Status: {}",
            project, cloud_provider, hash, status
        );

        let mut content_for_cache: Option<String> = None;
        let mut username_for_action: Option<String> = None;
        if let Some(decoded_str) = fetch_and_decode_file(
            &config.keyhouse.base_url,
            &config.keyhouse.token,
            &hash,
            &status,
            last_commit,
        )
        .await?
        {
            info!(target:get_log_target(), "Decoded file for hash {}", hash);
            username_for_action = Some(decoded_str.clone());

            if cloud_provider == "names" {
                content_for_cache = Some(decoded_str);
            } else {
                content_for_cache = Some("1".to_string());
            }
        }

        if let Some(username) = username_for_action {
            if cloud_provider == hostname || status == "deleteduser" {
                if status == "added" && cloud_provider != "names" {
                    info!(target:get_log_target(), "Adding user '{}' to group '{}'...", username, project);
                    add_user_to_group(&username, &project).unwrap_or_else(|e| {
                        error!(target:get_log_target(), "Failed to add user: {}", e);
                    });
                } else if status == "deleted" && cloud_provider != "names" {
                    info!(target:get_log_target(), "Removing user '{}' from group '{}'...", username, project);
                    remove_user_from_group(&username, &project).unwrap_or_else(|e| {
                        error!(target:get_log_target(), "Failed to remove user: {}", e);
                    });
                } else if status == "deleteduser" {
                    info!(target:get_log_target(), "Deleting user '{}'...", username);
                    delete_user(&username).unwrap_or_else(|e| {
                        error!(target:get_log_target(), "Failed to delete user: {}", e);
                    });
                }
            } else if cloud_provider != "names" {
                info!(target:get_log_target(), "Change for server '{}', not this server. Skipping system action.", cloud_provider);
            }

            let (cache_provider, cache_project) = if cloud_provider == "names" {
                ("names", "")
            } else {
                (cloud_provider.as_str(), project.as_str())
            };

            if let Some(content) = content_for_cache {
                update_local_cache(
                    config,
                    cache_project,
                    cache_provider,
                    &hash,
                    &status,
                    &content,
                )
                .unwrap_or_else(
                    |e| warn!(target:get_log_target(), "Failed to update cache: {}", e),
                );
            }
        }
    }

    Ok(())
}
pub async fn fetch_recent_commit(
    base_url: &str,
    token: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();

    let url = format!("{}/commits?sha=build&per_page=1", base_url);
    let commits: Vec<CommitInfo> = client
        .get(&url)
        .bearer_auth(token)
        .header(USER_AGENT, "rust-webhook-server")
        .header(ACCEPT, "application/vnd.github.v3+json")
        .send()
        .await?
        .json()
        .await?;
    if let Some(commit) = commits.first() {
        info!(target:get_log_target(), "Fetched latest commit: {}", commit.sha);
        Ok(commit.sha.clone())
    } else {
        error!(target:get_log_target(), "No commits found on build branch",);
        Err("No commits found".into())
    }
}
use base64::{Engine as _, engine::general_purpose};
pub async fn fetch_and_decode_file(
    base_url: &str,
    token: &str,
    hash: &str,
    status: &str,
    base_commit: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let commit_ref = if status == "deleted" || status == "deleteduser" {
        base_commit
    } else {
        "build"
    };

    let url = format!("{}/contents/names/{}?ref={}", base_url, hash, commit_ref);
    let client = reqwest::Client::new();
    let file_resp = client
        .get(&url)
        .bearer_auth(token)
        .header(USER_AGENT, "rust-webhook-server")
        .header(ACCEPT, "application/vnd.github.v3+json")
        .send()
        .await?;
    if !file_resp.status().is_success() {
        warn!(target:get_log_target(),
            "GitHub API returned error for file at url {}: {}",
            url,
            file_resp.status()
        );
        return Ok(None);
    }
    let file_json = file_resp.json::<serde_json::Value>().await?;
    if let Some(base64_content) = file_json["content"].as_str() {
        let clean_base64 = base64_content.replace('\n', "");
        let decoded = general_purpose::STANDARD.decode(&clean_base64)?;
        let decoded_str = String::from_utf8(decoded)?;
        info!(target:get_log_target(), "Decoded file for hash {}", hash);
        Ok(Some(decoded_str))
    } else {
        warn!(target:get_log_target(), "No 'content' field found for file hash {}", hash);
        Ok(None)
    }
}
pub fn extract_diff_parts(diff_data: &str) -> Vec<(String, String, String, String)> {
    let re_access = Regex::new(r"diff --git a/(access/([^/]+)/([^/]+)/([\w\d]+))").unwrap();
    let re_names = Regex::new(r"diff --git a/(names/([\w\d]+))").unwrap();
    let mut parts_with_status = HashMap::new();
    for line in diff_data.lines() {
        if let Some(caps) = re_access.captures(line) {
            let full_path = &caps[1];
            let project = &caps[2];
            let provider = &caps[3];
            let hash = &caps[4];
            let status = if diff_data.contains("new file mode") && line.contains(full_path) {
                "added"
            } else if diff_data.contains("deleted file mode") && line.contains(full_path) {
                "deleted"
            } else {
                "modified"
            };
            info!(target:get_log_target(),
                "Access file change detected: {}/{}/{}, status: {}",
                project, provider, hash, status
            );
            parts_with_status
                .entry((project.to_string(), provider.to_string(), hash.to_string()))
                .or_insert(status.to_string());
        } else if let Some(caps) = re_names.captures(line) {
            let full_path = &caps[1];
            let hash = &caps[2];
            let status = if diff_data.contains("deleted file mode") && line.contains(full_path) {
                "deleteduser"
            } else {
                "modifieduser"
            };
            info!(target:get_log_target(), "Name file change detected: {}, status: {}", hash, status);
            parts_with_status
                .entry(("".to_string(), "names".to_string(), hash.to_string()))
                .or_insert(status.to_string());
        }
    }
    parts_with_status
        .into_iter()
        .map(|((proj, prov, hash), status)| (proj, prov, hash, status))
        .collect()
}
pub async fn fetch_diff(
    base_url: &str,
    base: &str,
    merge: &str,
    token: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let url = format!("{}/compare/{}...{}", base_url, base, merge);

    info!(target:get_log_target(), "Fetching diff from GitHub: {}", url);
    let response = client
        .get(&url)
        .header(USER_AGENT, "rust-webhook-server")
        .header(ACCEPT, "application/vnd.github.v3.diff")
        .bearer_auth(token)
        .send()
        .await?;

    let diff = response.text().await?;
    info!(target:get_log_target(), "Fetched diff between {} and {}", base, merge);
    Ok(diff)
}

/// This function iterates through a specific directory structure to reconstruct user-to-group mappings. It expects a cache structure where:
/// 1.) `access/<server>/<group_name>/<user_hash>` exists to link groups to user hashes.
/// 2.) `names/<user_hash>` exists and contains the plaintext username.
pub async fn update_all_users_from_cache(
    server: &str,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let cache_base_path = Path::new(&config.cache_path);
    info!(target:get_log_target(), "Provisioning users based on full sync cache at {:?}...", cache_base_path);

    let access_path = cache_base_path.join("access").join(server);

    // Early exit if the cache for this server doesn't exist
    if !access_path.exists() {
        warn!(target:get_log_target(), "Access directory for this server '{}' not found in cache.", server);
        return Ok(());
    }

    // 1. Iterate over directories representing Groups
    for group_entry in fs::read_dir(access_path)? {
        let group_entry = group_entry?;
        if group_entry.file_type()?.is_dir() {
            let group_path = group_entry.path();
            if let Some(group_name) = group_path.file_name().and_then(|n| n.to_str()) {
                // 2. Iterate over files representing User Hashes
                for hash_entry in fs::read_dir(&group_path)? {
                    let hash_entry = hash_entry?;
                    if hash_entry.file_type()?.is_file()
                        && let Some(hash) = hash_entry.file_name().to_str()
                    {
                        let name_path = names_path(cache_base_path, hash);

                        // 3. Resolve Hash -> Username
                        if let Ok(username) = fs::read_to_string(name_path) {
                            let trimmed_username = username.trim();
                            if !trimmed_username.is_empty() {
                                info!(target:get_log_target(), "Sync: Adding user '{}' to group '{}'", trimmed_username, group_name);

                                // 4. Execute Provisioning
                                add_user_to_group(trimmed_username, group_name).unwrap_or_else(
                                        |e| error!(target:get_log_target(), "Failed to add user during sync: {}", e),
                                    );
                            }
                        }
                    }
                }
            }
        }
    }
    info!(target:get_log_target(), "User provisioning completed.");
    Ok(())
}

pub async fn fetch_latest_commit(base_url: &str, token: &str) -> Result<String> {
    let url = format!("{}/commits/build", base_url);

    let client = Client::new();
    let response = client
        .get(&url)
        .header("Authorization", format!("token {}", token))
        .header("User-Agent", "scout-bot")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to fetch latest commit. Status: {}",
            response.status()
        ));
    }

    let json: Value = response.json().await?;
    if let Some(sha) = json["sha"].as_str() {
        Ok(sha.to_string())
    } else {
        Err(anyhow!("SHA not found in commit response"))
    }
}
