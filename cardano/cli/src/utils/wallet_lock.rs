//! Process-level wallet lock to prevent parallel CLI invocations from
//! selecting the same UTXOs simultaneously.

use anyhow::{Context, Result};
use std::fs::File;
use std::os::unix::io::AsRawFd;

/// RAII guard that holds an exclusive advisory lock on a per-wallet lock file.
///
/// The lock is released when this value is dropped (when the `File` is closed).
/// Backed by `flock(LOCK_EX)`, so it works across independent OS processes.
pub struct WalletLock {
    _file: File,
}

impl WalletLock {
    /// Acquire an exclusive lock for the given wallet address prefix.
    ///
    /// Blocks until the lock is available. The lock file is created at
    /// `/tmp/hyperlane-cli-{addr_prefix}.lock` where `addr_prefix` is the
    /// first 16 characters of `wallet_addr`.
    pub fn acquire(wallet_addr: &str) -> Result<Self> {
        let prefix: String = wallet_addr.chars().take(16).collect();
        let lock_path = format!("/tmp/hyperlane-cli-{}.lock", prefix);

        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("Failed to open lock file: {}", lock_path))?;

        let fd = file.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            return Err(err).with_context(|| format!("Failed to acquire wallet lock: {}", lock_path));
        }

        Ok(Self { _file: file })
    }
}
