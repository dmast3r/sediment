use std::fs::File;
use std::io::Result;
#[allow(unused_imports)] // deliberate decision to prevent failures on non-Mac OS
use std::os::fd::AsRawFd;

#[cfg(target_os = "macos")]
pub(crate) fn durable_sync(file: &File) -> Result<()> {
    // SAFETY: fcntl doesn't have a memory contract nor touches any pointers.
    // Its two parameters are a file descriptor and an operational flag.
    // If the FD is invalid, it returns the EBADF error instead of crashing with UB.
    // The fd is valid because file is alive across the call
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) };

    if rc == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn durable_sync(file: &File) -> Result<()> {
    file.sync_all()?;
    Ok(())
}
