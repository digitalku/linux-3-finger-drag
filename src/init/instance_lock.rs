use std::{
    env,
    fs::{File, OpenOptions, TryLockError},
    io::{Error, ErrorKind},
    path::PathBuf
};
use tracing::{debug, error};
use users::get_current_uid;


/// Where the lock lives: the user's runtime directory if there is one
/// (it's private, and cleared on logout), otherwise a per-user file
/// in /tmp.
fn lock_file_path() -> PathBuf {
    match env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => {
            PathBuf::from(dir).join("linux-3-finger-drag.lock")
        },
        _ => PathBuf::from(format!("/tmp/linux-3-finger-drag-{}.lock", get_current_uid()))
    }
}


/// Make sure this is the only instance running for this user.
///
/// Two instances would each create a virtual trackpad and both react
/// to the same gestures. This usually happens when the program is
/// started both by the desktop's Autostart and by the systemd unit.
///
/// The returned file holds the lock, and must be kept alive for as
/// long as the program runs. The kernel releases the lock when the
/// process exits, however it exits, so a crash never leaves a stale
/// lock behind.
pub fn acquire() -> Result<File, Error> {
    let path = lock_file_path();
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;

    match file.try_lock() {
        Ok(()) => {
            debug!("Acquired instance lock at {}", path.display());
            Ok(file)
        },
        Err(TryLockError::WouldBlock) => {
            error!(
                "linux-3-finger-drag is already running, so this instance will exit.\n\
                If you didn't start it twice on purpose, it is probably set to start \
                both from your desktop's Autostart and from the systemd user unit. \
                Keep only one of them (see step 7 in README.md)."
            );
            Err(Error::new(ErrorKind::AlreadyExists, "another instance is already running"))
        },
        Err(TryLockError::Error(e)) => Err(e)
    }
}
