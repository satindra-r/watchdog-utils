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
        .header(USER_AGENT, "Watchdog Utils")
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

                    update_local_cache(config, "", "", hash, &DiffAction::AddedUser, &username)
                        .unwrap_or_else(|e| warn!("Failed to update names cache for {hash}: {e}"));
                }
            }
        }
    }
    info!("Full cache sync completed.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config::test_init;
    use crate::utils::cache_utils::{sync_full_cache, update_local_cache};
    use crate::utils::types::DiffAction;
    use std::fs;

    #[test]
    fn sync_full_cache_test() {
        let config = test_init("./cache_tests/sync_full_cache_test");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            sync_full_cache(&config).await.unwrap();

            let contents1 = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1.is_ok());
            assert_eq!(contents1.unwrap(), "1");
            let contents2 = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2.is_ok());
            assert_eq!(contents2.unwrap(), "1");
            let contents3 = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3.is_ok());
            assert_eq!(contents3.unwrap(), "1");
            let contents4 = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4.is_ok());
            assert_eq!(contents4.unwrap(), "1");
            let contents5 = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5.is_ok());
            assert_eq!(contents5.unwrap(), "saturn");
            let contents6 = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6.is_ok());
            assert_eq!(contents6.unwrap(), "uranus");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

        });
    }

    #[test]
    fn update_local_cache_add_user_test() {
        let config = test_init("./cache_tests/update_local_cache_add_user_test");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            sync_full_cache(&config).await.unwrap();

            let contents1_before = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_before.is_ok());
            assert_eq!(contents1_before.unwrap(), "1");
            let contents2_before = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_before.is_ok());
            assert_eq!(contents2_before.unwrap(), "1");
            let contents3_before = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_before.is_ok());
            assert_eq!(contents3_before.unwrap(), "1");
            let contents4_before = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_before.is_ok());
            assert_eq!(contents4_before.unwrap(), "1");
            let contents5_before = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_before.is_ok());
            assert_eq!(contents5_before.unwrap(), "saturn");
            let contents6_before = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_before.is_ok());
            assert_eq!(contents6_before.unwrap(), "uranus");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

            update_local_cache(&config, "", "", "hash", &DiffAction::AddedUser, "username").unwrap();

            let contents1_after = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_after.is_ok());
            assert_eq!(contents1_after.unwrap(), "1");
            let contents2_after = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_after.is_ok());
            assert_eq!(contents2_after.unwrap(), "1");
            let contents3_after = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_after.is_ok());
            assert_eq!(contents3_after.unwrap(), "1");
            let contents4_after = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_after.is_ok());
            assert_eq!(contents4_after.unwrap(), "1");
            let contents5_after = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_after.is_ok());
            assert_eq!(contents5_after.unwrap(), "saturn");
            let contents6_after = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_after.is_ok());
            assert_eq!(contents6_after.unwrap(), "uranus");

            let contents7 =fs::read_to_string(config.cache_path.join("names/hash"));
            assert!(contents7.is_ok());
            assert_eq!(contents7.unwrap(), "username");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 3);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

        });
    }

    #[test]
    fn update_local_cache_delete_user_test() {
        let config = test_init("./cache_tests/update_local_cache_delete_user_test");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            sync_full_cache(&config).await.unwrap();

            update_local_cache(&config, "", "", "hash", &DiffAction::AddedUser, "username").unwrap();

            let contents1_before = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_before.is_ok());
            assert_eq!(contents1_before.unwrap(), "1");
            let contents2_before = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_before.is_ok());
            assert_eq!(contents2_before.unwrap(), "1");
            let contents3_before = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_before.is_ok());
            assert_eq!(contents3_before.unwrap(), "1");
            let contents4_before = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_before.is_ok());
            assert_eq!(contents4_before.unwrap(), "1");
            let contents5_before = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_before.is_ok());
            assert_eq!(contents5_before.unwrap(), "saturn");
            let contents6_before = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_before.is_ok());
            assert_eq!(contents6_before.unwrap(), "uranus");

            let contents7 =fs::read_to_string(config.cache_path.join("names/hash"));
            assert!(contents7.is_ok());
            assert_eq!(contents7.unwrap(), "username");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 3);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

            update_local_cache(&config, "", "", "hash", &DiffAction::DeletedUser, "username").unwrap();

            let contents1_after = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_after.is_ok());
            assert_eq!(contents1_after.unwrap(), "1");
            let contents2_after = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_after.is_ok());
            assert_eq!(contents2_after.unwrap(), "1");
            let contents3_after = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_after.is_ok());
            assert_eq!(contents3_after.unwrap(), "1");
            let contents4_after = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_after.is_ok());
            assert_eq!(contents4_after.unwrap(), "1");
            let contents5_after = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_after.is_ok());
            assert_eq!(contents5_after.unwrap(), "saturn");
            let contents6_after = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_after.is_ok());
            assert_eq!(contents6_after.unwrap(), "uranus");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

        });
    }

    #[test]
    fn update_local_cache_add_group_test() {
        let config = test_init("./cache_tests/update_local_cache_add_group_test");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            sync_full_cache(&config).await.unwrap();

            update_local_cache(&config, "", "", "hash", &DiffAction::AddedUser, "username").unwrap();

            let contents1_before = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_before.is_ok());
            assert_eq!(contents1_before.unwrap(), "1");
            let contents2_before = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_before.is_ok());
            assert_eq!(contents2_before.unwrap(), "1");
            let contents3_before = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_before.is_ok());
            assert_eq!(contents3_before.unwrap(), "1");
            let contents4_before = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_before.is_ok());
            assert_eq!(contents4_before.unwrap(), "1");
            let contents5_before = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_before.is_ok());
            assert_eq!(contents5_before.unwrap(), "saturn");
            let contents6_before = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_before.is_ok());
            assert_eq!(contents6_before.unwrap(), "uranus");
            let contents7_before =fs::read_to_string(config.cache_path.join("names/hash"));
            assert!(contents7_before.is_ok());
            assert_eq!(contents7_before.unwrap(), "username");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 3);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

            update_local_cache(&config, "sudo", "centos", "hash", &DiffAction::AddedGroup, "username").unwrap();

            let contents1_after = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_after.is_ok());
            assert_eq!(contents1_after.unwrap(), "1");
            let contents2_after = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_after.is_ok());
            assert_eq!(contents2_after.unwrap(), "1");
            let contents3_after = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_after.is_ok());
            assert_eq!(contents3_after.unwrap(), "1");
            let contents4_after = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_after.is_ok());
            assert_eq!(contents4_after.unwrap(), "1");
            let contents5_after = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_after.is_ok());
            assert_eq!(contents5_after.unwrap(), "saturn");
            let contents6_after = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_after.is_ok());
            assert_eq!(contents6_after.unwrap(), "uranus");
            let contents7_after =fs::read_to_string(config.cache_path.join("names/hash"));
            assert!(contents7_after.is_ok());
            assert_eq!(contents7_after.unwrap(), "username");

            let contents8 = fs::read_to_string(config.cache_path.join("access/centos/sudo/hash"));
            assert!(contents8.is_ok());
            assert_eq!(contents8.unwrap(), "1");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 3);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

        });
    }

    #[test]
    fn update_local_cache_add_new_group_test() {
        let config = test_init("./cache_tests/update_local_cache_add_new_group_test");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            sync_full_cache(&config).await.unwrap();

            update_local_cache(&config, "", "", "hash", &DiffAction::AddedUser, "username").unwrap();

            let contents1_before = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_before.is_ok());
            assert_eq!(contents1_before.unwrap(), "1");
            let contents2_before = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_before.is_ok());
            assert_eq!(contents2_before.unwrap(), "1");
            let contents3_before = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_before.is_ok());
            assert_eq!(contents3_before.unwrap(), "1");
            let contents4_before = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_before.is_ok());
            assert_eq!(contents4_before.unwrap(), "1");
            let contents5_before = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_before.is_ok());
            assert_eq!(contents5_before.unwrap(), "saturn");
            let contents6_before = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_before.is_ok());
            assert_eq!(contents6_before.unwrap(), "uranus");
            let contents7_before =fs::read_to_string(config.cache_path.join("names/hash"));
            assert!(contents7_before.is_ok());
            assert_eq!(contents7_before.unwrap(), "username");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 3);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

            update_local_cache(&config, "proxy", "centos", "hash", &DiffAction::AddedGroup, "username").unwrap();

            let contents1_after = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_after.is_ok());
            assert_eq!(contents1_after.unwrap(), "1");
            let contents2_after = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_after.is_ok());
            assert_eq!(contents2_after.unwrap(), "1");
            let contents3_after = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_after.is_ok());
            assert_eq!(contents3_after.unwrap(), "1");
            let contents4_after = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_after.is_ok());
            assert_eq!(contents4_after.unwrap(), "1");
            let contents5_after = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_after.is_ok());
            assert_eq!(contents5_after.unwrap(), "saturn");
            let contents6_after = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_after.is_ok());
            assert_eq!(contents6_after.unwrap(), "uranus");
            let contents7_after =fs::read_to_string(config.cache_path.join("names/hash"));
            assert!(contents7_after.is_ok());
            assert_eq!(contents7_after.unwrap(), "username");

            let content8 = fs::read_to_string(config.cache_path.join("access/centos/proxy/hash"));
            assert!(content8.is_ok());
            assert_eq!(content8.unwrap(), "1");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 3);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 3);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/proxy")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

        });
    }

    #[test]
    fn update_local_cache_del_group_test() {
        let config = test_init("./cache_tests/update_local_cache_del_group_test");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            sync_full_cache(&config).await.unwrap();

            update_local_cache(&config, "", "", "hash", &DiffAction::AddedUser, "username").unwrap();
            update_local_cache(&config, "sudo", "centos", "hash", &DiffAction::AddedGroup, "username").unwrap();

            let contents1_before = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_before.is_ok());
            assert_eq!(contents1_before.unwrap(), "1");
            let contents2_before = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_before.is_ok());
            assert_eq!(contents2_before.unwrap(), "1");
            let contents3_before = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_before.is_ok());
            assert_eq!(contents3_before.unwrap(), "1");
            let contents4_before = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_before.is_ok());
            assert_eq!(contents4_before.unwrap(), "1");
            let contents5_before = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_before.is_ok());
            assert_eq!(contents5_before.unwrap(), "saturn");
            let contents6_before = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_before.is_ok());
            assert_eq!(contents6_before.unwrap(), "uranus");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 3);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);

            update_local_cache(&config, "sudo", "centos", "hash", &DiffAction::DeletedGroup, "username").unwrap();

            let contents1_after = fs::read_to_string(config.cache_path.join("access/centos/centos/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents1_after.is_ok());
            assert_eq!(contents1_after.unwrap(), "1");
            let contents2_after = fs::read_to_string(config.cache_path.join("access/centos/centos/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents2_after.is_ok());
            assert_eq!(contents2_after.unwrap(), "1");
            let contents3_after = fs::read_to_string(config.cache_path.join("access/centos/sudo/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents3_after.is_ok());
            assert_eq!(contents3_after.unwrap(), "1");
            let contents4_after = fs::read_to_string(config.cache_path.join("access/pa-002/quizio/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents4_after.is_ok());
            assert_eq!(contents4_after.unwrap(), "1");
            let contents5_after = fs::read_to_string(config.cache_path.join("names/6807cfc9f4a951d37cb9097bcc2e5081dad331243b00501d3e9d87423d58f6ef"));
            assert!(contents5_after.is_ok());
            assert_eq!(contents5_after.unwrap(), "saturn");
            let contents6_after = fs::read_to_string(config.cache_path.join("names/8151cfbed99dd9e208eaf3d83e7586eb52d192d4bfbf671a4ca51641eac38df0"));
            assert!(contents6_after.is_ok());
            assert_eq!(contents6_after.unwrap(), "uranus");

            let contents7 =fs::read_to_string(config.cache_path.join("names/hash"));
            assert!(contents7.is_ok());
            assert_eq!(contents7.unwrap(), "username");

            assert_eq!(fs::read_dir(&config.cache_path).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("names")).unwrap().count(), 3);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/centos")).unwrap().count(), 2);
            assert_eq!(fs::read_dir(config.cache_path.join("access/centos/sudo")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002")).unwrap().count(), 1);
            assert_eq!(fs::read_dir(config.cache_path.join("access/pa-002/quizio")).unwrap().count(), 1);
        });
    }
}
