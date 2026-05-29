use crate::config::get_log_target;
use log::{error, info};
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Result;
use std::io::Write;
use std::path::Path;
use std::process::Command;

pub fn user_exists(username: &str) -> io::Result<bool> {
    let output = Command::new("id").arg(username).output()?;
    Ok(output.status.success())
}

pub fn group_exists(group: &str) -> bool {
    fs::read_to_string("/etc/group")
        .map(|contents| {
            contents
                .lines()
                .any(|line| line.starts_with(&format!("{}:", group)))
        })
        .unwrap_or(false)
}

pub fn create_user(user: &str) -> io::Result<()> {
    let output = Command::new("sudo")
        .arg("useradd")
        .arg("-M")
        .arg("-s")
        .arg("/bin/bash")
        .arg(user)
        .output()?;

    if !output.status.success() {
        error!(target:get_log_target(),
            "Failed to create user '{}': {}",
            user,
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(io::Error::other("Failed to create user"));
    }

    match create_user_directory(user) {
        Ok(msg) => {
            info!(target:get_log_target(),"User {} directory {} successfully.", user, msg);
            if msg == "created" {
                match update_user_bashrc(user) {
                    Ok(_) => {
                        info!(target:get_log_target(),"User {} bashrc updated successfully.", user);
                    }
                    Err(e) => {
                        error!(target:get_log_target(), "Failed to update user {} bashrc: {}", user, e);
                    }
                }
            }
        }
        Err(e) => {
            error!(target:get_log_target(),
                "Failed to create/archive user directory'{}': {}",
                user,
                e
            );
            return Err(io::Error::other("Failed to create/archive user directory"));
        }
    }
    Ok(())
}

pub fn add_user_to_group(user: &str, group: &str) -> io::Result<()> {
    if !user_exists(user)? {
        info!(target:get_log_target(), "User '{}' does not exist. Creating user...", user);
        create_user(user)?;
    }

    let group_to_add = if group == "sudo" {
        if group_exists("sudo") {
            "sudo"
        } else if group_exists("wheel") {
            "wheel"
        } else {
            error!(target:get_log_target(), "Neither 'sudo' nor 'wheel' group exists.");
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No admin group ('sudo' or 'wheel') found",
            ));
        }
    } else if group_exists(group) {
        group
    } else {
        error!(target:get_log_target(), "Group '{}' does not exist.", group);
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Group '{}' not found", group),
        ));
    };

    let output = Command::new("sudo")
        .arg("usermod")
        .arg("-aG")
        .arg(group_to_add)
        .arg(user)
        .output()?;

    if output.status.success() {
        info!(target:get_log_target(), "User '{}' added to group '{}'.", user, group_to_add);
        Ok(())
    } else {
        error!(target:get_log_target(),
            "Failed to add user '{}' to group '{}': {}",
            user,
            group_to_add,
            String::from_utf8_lossy(&output.stderr)
        );
        Err(io::Error::other("Failed to add user to group"))
    }
}

pub fn remove_user_from_group(user: &str, group: &str) -> io::Result<()> {
    let output = Command::new("sudo")
        .arg("gpasswd")
        .arg("-d")
        .arg(user)
        .arg(group)
        .output()?;

    if output.status.success() {
        info!(target:get_log_target(), "User '{}' removed from group '{}'.", user, group);
        Ok(())
    } else {
        error!(target:get_log_target(),
            "Failed to remove user '{}' from group '{}': {}",
            user,
            group,
            String::from_utf8_lossy(&output.stderr)
        );
        Err(io::Error::other("Failed to remove user from group"))
    }
}

pub fn delete_user(user: &str) -> io::Result<()> {
    let output = Command::new("sudo").arg("userdel").arg(user).output()?;

    if output.status.success() {
        info!(target:get_log_target(), "User '{}' deleted successfully.", user);
    } else {
        error!(target:get_log_target(),
            "Failed to delete user '{}': {}",
            user,
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(io::Error::other("Failed to delete user"));
    }

    match archive_user_directory(user) {
        Ok(_) => {
            info!(target:get_log_target(), "User '{}' directory archived successfully.", user);
            Ok(())
        }
        Err(e) => {
            error!(target:get_log_target(),
            "Failed to delete user '{}': {}",
            user,e);
            Err(io::Error::other("Failed to archive user directory"))
        }
    }
}

pub fn create_user_directory(user: &str) -> Result<&'static str> {
    let home_path = format!("/opt/watchdog/users/{}", user);
    if Path::new(&home_path).exists() {
        let output = Command::new("sudo")
            .arg("chown")
            .arg("-R")
            .arg(format!("{}:{}", user, user))
            .arg(&home_path)
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(String::from_utf8_lossy(&output.stderr)));
        }
        Ok("unarchived")
    } else {
        fs::create_dir_all(&home_path)?;
        let output1 = Command::new("sudo")
            .arg("cp")
            .arg("-r")
            .arg("/etc/skel/.") // The /. ensures hidden files are copied
            .arg(&home_path)
            .output()?;
        if !output1.status.success() {
            return Err(io::Error::other(String::from_utf8_lossy(&output1.stderr)));
        }

        let output2 = Command::new("sudo")
            .arg("chown")
            .arg("-R")
            .arg(format!("{}:{}", user, user))
            .arg(&home_path)
            .output()?;
        if !output2.status.success() {
            return Err(io::Error::other(String::from_utf8_lossy(&output2.stderr)));
        }

        let output3 = Command::new("sudo")
            .arg("chmod")
            .arg("-R")
            .arg("700")
            .arg(&home_path)
            .output()?;
        if !output3.status.success() {
            return Err(io::Error::other(String::from_utf8_lossy(&output3.stderr)));
        }

        let output4 = Command::new("sudo")
            .arg("usermod")
            .arg("-d")
            .arg(&home_path)
            .arg(user)
            .output()?;
        if !output4.status.success() {
            return Err(io::Error::other(String::from_utf8_lossy(&output4.stderr)));
        }
        Ok("created")
    }
}

pub fn archive_user_directory(user: &str) -> Result<()> {
    let home_path = format!("/opt/watchdog/users/{}", user);
    let output = Command::new("sudo")
        .arg("chown")
        .arg("-R")
        .arg("root:root")
        .arg(&home_path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr)));
    }
    Ok(())
}

pub fn update_user_bashrc(user: &str) -> Result<()> {
    let bashrc_path = format!("/opt/watchdog/users/{}/.bashrc", user);
    let bashrc_lines = r#"
# Load group-specific config if present
for group in $(id -nG "$USER"); do
    group_bashrc="/home/$group/.bashrc"
    [ -f "$group_bashrc" ] && source "$group_bashrc"
done
"#;
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&bashrc_path)?;
    file.write_all(bashrc_lines.as_bytes())?;
    info!(target:get_log_target(), "Appended group-config loader to '{}'.", bashrc_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::services::user_service::{
        add_user_to_group, create_user, delete_user, group_exists, remove_user_from_group,
        user_exists,
    };
    use std::process::Command;

    #[test]
    #[ignore]
    pub fn create_user_docker_test() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            assert!(
                !user_exists("testUserCreate").unwrap(),
                "\"testUserCreate\" already exists, can't proceed with test"
            );
            create_user("testUserCreate").expect("can't create user");
            assert!(user_exists("testUserCreate").unwrap());
        });
    }

    #[test]
    #[ignore]
    pub fn delete_user_docker_test() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_user("testUserDelete").expect("can't create user");
            assert!(
                user_exists("testUserDelete").unwrap(),
                "unable to create \"testUserDelete\", can't proceed with test"
            );
            delete_user("testUserDelete").expect("can't delete user");

            assert!(!user_exists("testUserDelete").unwrap());
        });
    }

    #[test]
    #[ignore]
    pub fn add_user_to_group_docker_test() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_user("testUserGroupAdd").expect("can't create user");
            assert!(
                user_exists("testUserGroupAdd").unwrap(),
                "unable to create \"testUserGroupAdd\" can't proceed with test"
            );
            assert!(
                group_exists("sudo"),
                "\"sudo\" group doesn't exit, can't proceed with test"
            );

            let output_before = String::from_utf8(
                Command::new("groups")
                    .arg("testUserGroupAdd")
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap();
            assert!(!output_before.contains("sudo"));

            add_user_to_group("testUserGroupAdd", "sudo").expect("can't add user to group");
            let output_after = String::from_utf8(
                Command::new("groups")
                    .arg("testUserGroupAdd")
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap();
            assert!(output_after.contains("sudo"));
        });
    }

    #[test]
    #[ignore]
    pub fn remove_user_from_group_docker_test() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_user("testUserGroupRem").expect("can't create user");
            assert!(
                user_exists("testUserGroupRem").unwrap(),
                "unable to create \"testUserGroupRem\" can't proceed with test"
            );
            assert!(
                group_exists("sudo"),
                "\"sudo\" group doesn't exit, can't proceed with test"
            );
            add_user_to_group("testUserGroupRem", "sudo").expect("can't add user to group");
            let output_before = String::from_utf8(
                Command::new("groups")
                    .arg("testUserGroupRem")
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap();
            assert!(output_before.contains("sudo"));
            remove_user_from_group("testUserGroupRem", "sudo")
                .expect("can't remove user from group");
            let output_after = String::from_utf8(
                Command::new("groups")
                    .arg("testUserGroupRem")
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap();
            assert!(!output_after.contains("sudo"));
        });
    }
}
