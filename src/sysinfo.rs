// vim: ts=4 et

//! The "actual system name" cryo.md's `cryo:origin` needs -- not a
//! DNS name or IP. `uname -n` reports the kernel's nodename directly
//! and is portable to both Linux and FreeBSD, unlike reading
//! Linux-only paths such as /proc/sys/kernel/hostname.

use std::process::Command;

use crate::error::{CryoError, Result};

pub fn hostname() -> Result<String> {
    let output = Command::new("uname").arg("-n").output()?;

    if !output.status.success() {
        return Err(CryoError::Command("uname -n failed".to_string()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
