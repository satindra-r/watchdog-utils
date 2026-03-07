use crate::config::{Config, get_log_target, init, set_log_target};
use crate::models::commit_info::CommitInfo;
use crate::services::user_service::add_user_to_group;
use crate::services::user_service::delete_user;
use crate::services::user_service::remove_user_from_group;
use crate::utils::cache_utils::{names_path, sync_full_cache, update_local_cache};
use crate::utils::types::{DiffAction, DiffTypes};
use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
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
            fs::write("base_commit.txt", &latest_commit)?;
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

    if !Path::new("base_commit.txt").exists() {
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
        last_commit = fs::read_to_string("base_commit.txt")?;
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
                fs::write("base_commit.txt", &merge_commit)?;
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
    for (cloud_provider, project, hash1, hash2, status) in extract_diff_parts(diff) {
        info!(target:get_log_target(),
            "Parsed diff - Cloud Provider: {}, Project: {}, Hash: {}, Status: {:?}",
            cloud_provider, project, hash1, status
        );

        let username = fetch_and_decode_file(
            &config.keyhouse.base_url,
            &config.keyhouse.token,
            &hash1,
            &status,
            last_commit,
        )
        .await?;

        info!(target:get_log_target(), "Decoded file for hash {}", hash1);

        match status {
            DiffAction::AddedGroup => {
                if cloud_provider == hostname {
                    info!(target:get_log_target(), "Adding user '{}' to group '{}'...", username, project);
                    add_user_to_group(&username, &project).unwrap_or_else(|e| {
                        error!(target:get_log_target(), "Failed to add user: {}", e);
                    });
                    update_local_cache(
                        config,
                        &project,
                        &cloud_provider,
                        &hash1,
                        &status,
                        &username,
                    )
                    .unwrap_or_else(
                        |e| warn!(target:get_log_target(), "Failed to update cache: {}", e),
                    );
                } else {
                    info!(target:get_log_target(), "Change for server '{}', not this server. Skipping system action.", cloud_provider);
                }
            }
            DiffAction::DeletedGroup => {
                if cloud_provider == hostname {
                    info!(target:get_log_target(), "Removing user '{}' from group '{}'...", username, project);
                    remove_user_from_group(&username, &project).unwrap_or_else(|e| {
                        error!(target:get_log_target(), "Failed to remove user: {}", e);
                    });

                    let projects = get_users_projects(config, &hash1).await?;
                    if projects.is_empty() {
                        info!(target:get_log_target(), "No other projects, deleting user '{}'...", username);
                        delete_user(&username).unwrap_or_else(|e| {
                            error!(target:get_log_target(), "Failed to delete user: {}", e);
                        });
                    }
                    update_local_cache(
                        config,
                        &project,
                        &cloud_provider,
                        &hash1,
                        &status,
                        &username,
                    )
                    .unwrap_or_else(
                        |e| warn!(target:get_log_target(), "Failed to update cache: {}", e),
                    );
                } else {
                    info!(target:get_log_target(), "Change for server '{}', not this server. Skipping system action.", cloud_provider);
                }
            }
            DiffAction::DeletedUser => {
                info!(target:get_log_target(), "Deleting user '{}'...", username);
                delete_user(&username).unwrap_or_else(|e| {
                    error!(target:get_log_target(), "Failed to delete user: {}", e);
                });
                update_local_cache(
                    config,
                    &project,
                    &cloud_provider,
                    &hash1,
                    &status,
                    &username,
                )
                .unwrap_or_else(
                    |e| warn!(target:get_log_target(), "Failed to update cache: {}", e),
                );
            }
            DiffAction::ModifiedUser => {
                let projects = get_users_projects(config, &hash2).await?;
                //delete groups for old user
                for del_project in &projects {
                    update_local_cache(
                        config,
                        del_project,
                        &cloud_provider,
                        &hash1,
                        &DiffAction::DeletedGroup,
                        &username,
                    )
                    .unwrap_or_else(
                        |e| warn!(target:get_log_target(), "Failed to update cache: {}", e),
                    );
                }
                //delete old user
                update_local_cache(
                    config,
                    &project,
                    &cloud_provider,
                    &hash1,
                    &DiffAction::DeletedUser,
                    &username,
                )
                .unwrap_or_else(
                    |e| warn!(target:get_log_target(), "Failed to update cache: {}", e),
                );
                //add new user
                update_local_cache(
                    config,
                    &project,
                    &cloud_provider,
                    &hash2,
                    &DiffAction::AddedUser,
                    &username,
                )
                .unwrap_or_else(
                    |e| warn!(target:get_log_target(), "Failed to update cache: {}", e),
                );
                //add groups for new user
                for add_project in &projects {
                    update_local_cache(
                        config,
                        add_project,
                        &cloud_provider,
                        &hash1,
                        &DiffAction::AddedGroup,
                        &username,
                    )
                    .unwrap_or_else(
                        |e| warn!(target:get_log_target(), "Failed to update cache: {}", e),
                    );
                }
            }
            DiffAction::AddedUser => {
                info!(target: get_log_target(), "New key added for user {}", username);
                update_local_cache(
                    config,
                    &project,
                    &cloud_provider,
                    &hash1,
                    &status,
                    &username,
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
        .header(USER_AGENT, "Watchdog Utils")
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

pub async fn fetch_and_decode_file(
    base_url: &str,
    token: &str,
    hash: &str,
    status: &DiffAction,
    base_commit: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let commit_ref = match status {
        DiffAction::DeletedGroup | DiffAction::DeletedUser => base_commit,
        _ => "build",
    };

    let url = format!("{}/contents/names/{}?ref={}", base_url, hash, commit_ref);
    let client = reqwest::Client::new();
    let file_resp = client
        .get(&url)
        .bearer_auth(token)
        .header(USER_AGENT, "Watchdog Utils")
        .header(ACCEPT, "application/vnd.github.v3+json")
        .send()
        .await?;
    if !file_resp.status().is_success() {
        warn!(target:get_log_target(),
            "GitHub API returned error for file at url {}: {}",
            url,
            file_resp.status()
        );
        return Err(Box::new(io::Error::new(
            io::ErrorKind::HostUnreachable,
            "GitHub API returned error",
        )));
    }
    let file_json = file_resp.json::<serde_json::Value>().await?;
    if let Some(base64_content) = file_json["content"].as_str() {
        let clean_base64 = base64_content.replace('\n', "");
        let decoded = general_purpose::STANDARD.decode(&clean_base64)?;
        let decoded_str = String::from_utf8(decoded)?;
        info!(target:get_log_target(), "Decoded file for hash {}", hash);
        Ok(decoded_str)
    } else {
        warn!(target:get_log_target(), "No 'content' field found for file hash {}", hash);
        Err(Box::new(io::Error::new(
            io::ErrorKind::NotFound,
            "No 'content' field found",
        )))
    }
}
pub fn extract_diff_parts(diff_data: &str) -> Vec<(String, String, String, String, DiffAction)> {
    let re_access = Regex::new(
        r"diff --git a/access/([^/]+)/([^/]+)/([\w\d]+) b/access/([^/]+)/([^/]+)/([\w\d]+)",
    )
    .unwrap();
    let re_names = Regex::new(r"diff --git a/names/([\w\d]+) b/names/([\w\d]+)").unwrap();
    let mut parts_with_status = HashMap::new();
    let mut diff_data_lines = diff_data.lines().peekable();
    while let Some(line) = diff_data_lines.next() {
        let next_line = diff_data_lines.peek().unwrap_or(&"");
        if let Some(caps) = re_access.captures(line) {
            let provider1 = &caps[1];
            let project1 = &caps[2];
            let hash1 = &caps[3];
            let provider2 = &caps[4];
            let project2 = &caps[5];
            let hash2 = &caps[6];

            if next_line.contains(format!("{}", DiffTypes::Added).as_str()) {
                parts_with_status
                    .entry((
                        provider1.to_string(),
                        project1.to_string(),
                        hash1.to_string(),
                        "".to_string(),
                    ))
                    .or_insert(DiffAction::AddedGroup);
                info!(target:get_log_target(),
                    "Access file change detected: {}/{}/{}, status: {:?}",
                    provider1, project1, hash1, DiffAction::AddedGroup
                );
            } else if next_line.contains(format!("{}", DiffTypes::Deleted).as_str()) {
                parts_with_status
                    .entry((
                        provider1.to_string(),
                        project1.to_string(),
                        hash1.to_string(),
                        "".to_string(),
                    ))
                    .or_insert(DiffAction::DeletedGroup);
                info!(target:get_log_target(),
                    "Access file change detected: {}/{}/{}, status: {:?}",
                    provider1, project1, hash1, DiffAction::DeletedGroup
                );
            } else if next_line.contains(format!("{}", DiffTypes::Renamed).as_str()) {
                parts_with_status
                    .entry((
                        provider1.to_string(),
                        project1.to_string(),
                        hash1.to_string(),
                        "".to_string(),
                    ))
                    .or_insert(DiffAction::DeletedGroup);
                parts_with_status
                    .entry((
                        provider2.to_string(),
                        project2.to_string(),
                        hash2.to_string(),
                        "".to_string(),
                    ))
                    .or_insert(DiffAction::AddedGroup);
                info!(target:get_log_target(),
                    "Access file change detected: {}/{}/{}, status: {:?}",
                    provider1, project1, hash1, DiffAction::DeletedGroup
                );
                info!(target:get_log_target(),
                    "Access file change detected: {}/{}/{}, status: {:?}",
                    provider2, project2, hash2, DiffAction::AddedGroup
                );
            }
        } else if let Some(caps) = re_names.captures(line) {
            let hash1 = &caps[1];
            let hash2 = &caps[2];
            if next_line.contains(format!("{}", DiffTypes::Added).as_str()) {
                parts_with_status
                    .entry((
                        "".to_string(),
                        "".to_string(),
                        hash1.to_string(),
                        "".to_string(),
                    ))
                    .or_insert(DiffAction::AddedUser);
                info!(target:get_log_target(), "Name file change detected: {}, status: {:?}", hash1,DiffAction::AddedUser);
            } else if next_line.contains(format!("{}", DiffTypes::Deleted).as_str()) {
                parts_with_status
                    .entry((
                        "".to_string(),
                        "".to_string(),
                        hash1.to_string(),
                        "".to_string(),
                    ))
                    .or_insert(DiffAction::DeletedUser);
                info!(target:get_log_target(), "Name file change detected: {}, status: {:?}", hash1, DiffAction::DeletedUser);
            } else if next_line.contains(format!("{}", DiffTypes::Renamed).as_str()) {
                parts_with_status
                    .entry((
                        "".to_string(),
                        "".to_string(),
                        hash1.to_string(),
                        hash2.to_string(),
                    ))
                    .or_insert(DiffAction::ModifiedUser);
                info!(target:get_log_target(), "Name file change detected: {} to {}, status: {:?}", hash1,hash2, DiffAction::ModifiedUser);
            } else {
                info!(target:get_log_target(), "Unknown diff");
            };
        }
    }
    parts_with_status
        .into_iter()
        .map(|((proj, prov, hash1, hash2), status)| (proj, prov, hash1, hash2, status))
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
        .header(USER_AGENT, "Watchdog Utils")
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

pub async fn get_users_projects(
    config: &Config,
    userhash: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();

    let host_url = format!(
        "{}/contents/access/{}?ref=build",
        config.keyhouse.base_url, config.hostname
    );
    info!(target:get_log_target(), "Host URL: {}", host_url);

    let projects: Vec<String> = match client
        .get(&host_url)
        .header(USER_AGENT, "Watchdog Utils")
        .bearer_auth(&config.keyhouse.token)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let contents: Vec<Value> = r.json().await?;
            info!(target:get_log_target(), "Response: {:?}",contents);
            contents
                .into_iter()
                .filter_map(|item| item["name"].as_str().map(String::from))
                .collect()
        }
        Ok(r) => {
            info!(target:get_log_target(), "Failed to fetch names: {}", r.status());
            return Err(format!("HTTP error: {}", r.status()).into());
        }
        Err(e) => {
            info!(target:get_log_target(), "Error fetching names: {:?}", e);
            return Err(e.into());
        }
    };

    info!(target:get_log_target(), "Found projects: {:?} for host {}", projects, config.hostname);
    let mut user_projects: Vec<String> = Vec::new();
    for project in &projects {
        let project_url = format!(
            "{}/contents/access/{}/{}/{}?ref=build",
            config.keyhouse.base_url, config.hostname, project, userhash
        );

        match client
            .get(&project_url)
            .header(USER_AGENT, "Watchdog Utils")
            .bearer_auth(&config.keyhouse.token)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => user_projects.push(project.clone()),
            Ok(_) | Err(_) => continue,
        }
    }
    Ok(user_projects)
}

#[cfg(test)]
mod tests {
    use crate::config::test_init;
    use crate::services::github_service;
    use crate::utils::types::DiffAction;
    use std::collections::HashSet;
    /*
        use this to get diffs from Github
        #[test]
        fn fetch_data_test() {
            let config = test_init("");
            let client = Client::new();
            let clean_base = "https://api.github.com/repos/watchdog-test-org/test";
            let base = "14dc1a625a40dd1effc5e3aa96d5b899efa40a35";
            let merge = "715a387763b7565f2215f70cea02cbd35ed8a2bd";
            let url = format!("{}/compare/{}...{}", clean_base, base, merge);
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let response = client
                    .get(&url)
                    .header(USER_AGENT, "Watchdog Utils")
                    .header(ACCEPT, "application/vnd.github.v3.diff")
                    .bearer_auth(config.keyhouse.token.as_str())
                    .send()
                    .await
                    .unwrap();
                let diff = response.text().await.unwrap();
                println!(">{}<", diff);
            });
        }
    */
    #[test]
    fn extract_diff_parts_add_grp_test() {
        let diff = r#"diff --git a/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef b/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
new file mode 100644
index 0000000..56a6051
--- /dev/null
+++ b/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
@@ -0,0 +1 @@
+1
\ No newline at end of file
"#;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected = (
                "centos".to_string(),
                "proxy".to_string(),
                "6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef".to_string(),
                "".to_string(),
                DiffAction::AddedGroup,
            );
            let result = github_service::extract_diff_parts(diff);
            assert_eq!(result.len(), 1);
            assert_eq!(result[0], expected);
        });
    }

    #[test]
    fn extract_diff_parts_del_grp_test() {
        let diff = r#"diff --git a/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef b/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
deleted file mode 100644
index 56a6051..0000000
--- a/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
+++ /dev/null
@@ -1 +0,0 @@
-1
\ No newline at end of file
"#;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected = (
                "centos".to_string(),
                "proxy".to_string(),
                "6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef".to_string(),
                "".to_string(),
                DiffAction::DeletedGroup,
            );
            let result = github_service::extract_diff_parts(diff);
            assert_eq!(result.len(), 1);
            assert_eq!(result[0], expected);
        });
    }

    #[test]
    fn extract_diff_parts_multi_add_grp_test() {
        let diff = r#"diff --git a/access/centos/broker/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef b/access/centos/broker/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
new file mode 100644
index 0000000..56a6051
--- /dev/null
+++ b/access/centos/broker/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
@@ -0,0 +1 @@
+1
\ No newline at end of file
diff --git a/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef b/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
new file mode 100644
index 0000000..56a6051
--- /dev/null
+++ b/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
@@ -0,0 +1 @@
+1
\ No newline at end of file
"#;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected: HashSet<(String, String, String, String, DiffAction)> =
                HashSet::from_iter([
                    (
                        "centos".to_string(),
                        "proxy".to_string(),
                        "6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"
                            .to_string(),
                        "".to_string(),
                        DiffAction::AddedGroup,
                    ),
                    (
                        "centos".to_string(),
                        "broker".to_string(),
                        "6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"
                            .to_string(),
                        "".to_string(),
                        DiffAction::AddedGroup,
                    ),
                ]);
            let result = github_service::extract_diff_parts(diff);
            let result_set = HashSet::from_iter(result);
            assert_eq!(result_set, expected);
        });
    }
    #[test]
    fn extract_diff_parts_multi_del_grp_test() {
        let diff = r#"diff --git a/access/centos/broker/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef b/access/centos/broker/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
deleted file mode 100644
index 56a6051..0000000
--- a/access/centos/broker/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
+++ /dev/null
@@ -1 +0,0 @@
-1
\ No newline at end of file
diff --git a/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef b/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
deleted file mode 100644
index 56a6051..0000000
--- a/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
+++ /dev/null
@@ -1 +0,0 @@
-1
\ No newline at end of file
"#;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected: HashSet<(String, String, String, String, DiffAction)> =
                HashSet::from_iter([
                    (
                        "centos".to_string(),
                        "proxy".to_string(),
                        "6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"
                            .to_string(),
                        "".to_string(),
                        DiffAction::DeletedGroup,
                    ),
                    (
                        "centos".to_string(),
                        "broker".to_string(),
                        "6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"
                            .to_string(),
                        "".to_string(),
                        DiffAction::DeletedGroup,
                    ),
                ]);
            let result = github_service::extract_diff_parts(diff);
            let result_set = HashSet::from_iter(result);
            assert_eq!(result_set, expected);
        });
    }

    #[test]
    fn extract_diff_parts_add_del_grp_test() {
        let diff = r#"diff --git a/access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef b/access/centos/broker/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
similarity index 100%
rename from access/centos/proxy/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
rename to access/centos/broker/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef
"#;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected: HashSet<(String, String, String, String, DiffAction)> =
                HashSet::from_iter([
                    (
                        "centos".to_string(),
                        "proxy".to_string(),
                        "6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"
                            .to_string(),
                        "".to_string(),
                        DiffAction::DeletedGroup,
                    ),
                    (
                        "centos".to_string(),
                        "broker".to_string(),
                        "6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"
                            .to_string(),
                        "".to_string(),
                        DiffAction::AddedGroup,
                    ),
                ]);
            let result = github_service::extract_diff_parts(diff);
            let result_set = HashSet::from_iter(result);
            assert_eq!(result_set, expected);
        });
    }

    #[test]
    fn extract_diff_parts_add_user_test() {
        let diff = r#"diff --git a/names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0 b/names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0
new file mode 100644
index 0000000..b6955e2
--- /dev/null
+++ b/names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0
@@ -0,0 +1 @@
+uranus
\ No newline at end of file
"#;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected = (
                "".to_string(),
                "".to_string(),
                "8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0".to_string(),
                "".to_string(),
                DiffAction::AddedUser,
            );
            let result = github_service::extract_diff_parts(diff);
            assert_eq!(result.len(), 1);
            assert_eq!(result[0], expected);
        });
    }

    #[test]
    fn extract_diff_parts_del_user_test() {
        let diff = r#"diff --git a/names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0 b/names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0
deleted file mode 100644
index b6955e2..0000000
--- a/names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0
+++ /dev/null
@@ -1 +0,0 @@
-uranus
\ No newline at end of file"#;
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected = (
                "".to_string(),
                "".to_string(),
                "8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0".to_string(),
                "".to_string(),
                DiffAction::DeletedUser,
            );
            let result = github_service::extract_diff_parts(diff);
            assert_eq!(result.len(), 1);
            assert_eq!(result[0], expected);
        });
    }

    #[test]
    fn get_users_projects_test() {
        let config = test_init("");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected = "centos".to_string();
            let result = github_service::get_users_projects(
                &config,
                "8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0",
            )
            .await
            .expect("Github request failed");
            assert_eq!(result.len(), 1);
            assert_eq!(result[0], expected);
        });
    }
    #[test]
    fn get_users_multi_projects_test() {
        let config = test_init("");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let expected: HashSet<String> =
                HashSet::from_iter(["centos".to_string(), "sudo".to_string()]);
            let result = github_service::get_users_projects(
                &config,
                "6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef",
            )
            .await
            .expect("Github request failed");
            let result_set = HashSet::from_iter(result);
            assert_eq!(result_set, expected);
        });
    }
}
