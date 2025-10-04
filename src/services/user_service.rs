use crate::config::get_log_target;
use log::{error, info};
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Result;
use std::io::Write;
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
    let home_dir = format!("/opt/watchdog/users/{}", user);

    let output = Command::new("sudo")
        .arg("useradd")
        .arg("-m")
        .arg("-d")
        .arg(&home_dir)
        .arg("--skel")
        .arg("/etc/skel")
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

    match update_user_bashrc(user) {
        Ok(_) => {
            info!(target:get_log_target(),"User {} bashrc updated successfully.", user);
        }
        Err(e) => {
            error!(target:get_log_target(), "Failed to update user {} bashrc: {}", user, e);
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
    let output = Command::new("sudo")
        .arg("userdel")
        .arg("-r")
        .arg(user)
        .output()?;

    if output.status.success() {
        info!(target:get_log_target(), "User '{}' deleted successfully.", user);
        Ok(())
    } else {
        error!(target:get_log_target(),
            "Failed to delete user '{}': {}",
            user,
            String::from_utf8_lossy(&output.stderr)
        );
        Err(io::Error::other("Failed to delete user"))
    }
}

pub fn update_user_bashrc(user: &str) -> Result<()> {
    let bashrc_path = format!("opt/watchdog/users/{}/.bashrc", user);
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
