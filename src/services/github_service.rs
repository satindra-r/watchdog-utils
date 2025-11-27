use crate::config::{KeyhouseConf, get_log_target, set_log_target};
use crate::models::commit_info::CommitInfo;
use crate::models::github_content::GitHubContent;
use crate::services::user_service::add_user_to_group;
use crate::services::user_service::delete_user;
use crate::services::user_service::remove_user_from_group;
use anyhow::{Result, anyhow};
use log::{error, info, warn};
use regex::Regex;
use reqwest::Client;
use reqwest::header::{ACCEPT, USER_AGENT};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub async fn process_update_request(
    keyhouse_config: KeyhouseConf,
    update_log_target: &str,
    hostname: String,
) -> Result<(), Box<dyn std::error::Error>> {
    set_log_target(update_log_target.to_string());
    let base_url = keyhouse_config.base_url.clone();
    let token = keyhouse_config.token.clone();
    let mut should_update_all_users = false;
    let mut last_commit = String::new();
    if !Path::new("base_commit.txt").exists() {
        should_update_all_users = true;
    } else {
        last_commit = fs::read_to_string("base_commit.txt")?;
        if last_commit.trim().is_empty() {
            should_update_all_users = true;
        }
    }
    if should_update_all_users {
        info!(target:get_log_target(), "No valid last commit found, updating all users...");
        let _ = update_all_users(&base_url, &token).await;
        let latest_commit = fetch_latest_commit(&base_url, &token).await?;
        fs::write("base_commit.txt", &latest_commit)?;
        return Ok(());
    }
    let merge_commit = fetch_recent_commit(&base_url, &token).await?;
    let diff = fetch_diff(&base_url, &last_commit, &merge_commit, &token).await?;
    info!(target:get_log_target(), "Fetched diff from GitHub");
    for (cloud_provider, project, hash, status) in extract_diff_parts(&diff) {
        info!(target:get_log_target(),
            "Parsed diff - Project: {}, Cloud Provider: {}, Hash: {}, Status: {}",
            project, cloud_provider, hash, status
        );
        if let Some(decoded_str) =
            fetch_and_decode_file(&base_url, &token, &hash, &status, &last_commit).await?
        {
            info!(target:get_log_target(), "Decoded file for hash {}", hash);
            if cloud_provider != hostname {
                info!(target:get_log_target(), "not this server, skipping...");
                continue;
            }
            if status == "added" {
                info!(target:get_log_target(), "Adding user to group...");
                add_user_to_group(&decoded_str, &project).unwrap_or_else(|e| {
                    error!(target:get_log_target(), "Failed to add user to group: {}", e);
                });
            } else if status == "deleted" {
                info!(target:get_log_target(), "Removing user from group...");
                remove_user_from_group(&decoded_str, &project).unwrap_or_else(|e| {
                    error!(target:get_log_target(), "Failed to remove user from group: {}", e);
                });
            } else if status == "deleteduser" {
                info!(target:get_log_target(), "Deleting user...");
                delete_user(&decoded_str).unwrap_or_else(|e| {
                    error!(target:get_log_target(), "Failed to delete user: {}", e);
                });
            }
        }
    }
    info!(target:get_log_target(), "Processed diff successfully.");
    std::fs::write("base_commit.txt", &merge_commit)?;

    Ok(())
}
pub async fn fetch_recent_commit(
    base_url: &str,
    token: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();

    let clean_base: &str = base_url.trim_end_matches("/contents");
    let url = format!("{}/commits?sha=build&per_page=1", clean_base);
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

    let url = format!("{}/names/{}?ref={}", base_url, hash, commit_ref);
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
            "GitHub API returned error for file at hash {}: {}",
            hash,
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
    let clean_base: &str = base_url.trim_end_matches("/contents");
    let url = format!("{}/compare/{}...{}", clean_base, base, merge);

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

pub async fn update_all_users(
    base_url: &str,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();

    let url = format!("{}/access?ref=build", base_url);

    let providers_resp = client
        .get(&url)
        .bearer_auth(token)
        .header(USER_AGENT, "rust-webhook-server")
        .header(ACCEPT, "application/vnd.github.v3+json")
        .send()
        .await?;

    let providers: Vec<Value> = providers_resp.json().await?;
    let mut cloud_providers = vec![];

    for provider in &providers {
        if let Some(name) = provider["name"].as_str() {
            cloud_providers.push(name.to_string());
        }
    }

    for provider in cloud_providers {
        let provider_url = format!("{}/access/{}?ref=build", base_url, provider);

        let projects_resp = client
            .get(&provider_url)
            .bearer_auth(token)
            .header(USER_AGENT, "rust-webhook-server")
            .header(ACCEPT, "application/vnd.github.v3+json")
            .send()
            .await?;

        let projects: Vec<Value> = projects_resp.json().await?;

        for project in &projects {
            if let Some(project_name) = project["name"].as_str() {
                let url = format!(
                    "{}/access/{}/{}?ref=build",
                    base_url, provider, project_name
                );

                let response = client
                    .get(&url)
                    .bearer_auth(token)
                    .header(ACCEPT, "application/vnd.github.v3+json")
                    .header(USER_AGENT, "rust-webhook-server")
                    .send()
                    .await?;

                if response.status().is_success() {
                    let files: Vec<GitHubContent> = response.json().await?;

                    for file in files {
                        let hash = &file.name;

                        if let Some(decoded_str) =
                            fetch_and_decode_file(base_url, token, hash, "added", "").await?
                        {
                            info!(target:get_log_target(),
                                "Adding user to group for project {}: {}",
                                project_name, decoded_str
                            );
                            add_user_to_group(&decoded_str, project_name).unwrap_or_else(|e| {
                                error!(target:get_log_target(), "Failed to add user in update_all_users: {}", e);
                            });
                        }
                    }
                } else {
                    error!(target:get_log_target(),
                        "Failed to fetch content for project {}. Status: {}",
                        project_name,
                        response.status()
                    );
                }
            }
        }
    }

    Ok(())
}

pub async fn fetch_latest_commit(base_url: &str, token: &str) -> Result<String> {
    let clean_base: &str = base_url.trim_end_matches("/contents");
    let url = format!("{}/commits/build", clean_base);

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
