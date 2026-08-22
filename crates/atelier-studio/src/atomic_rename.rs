//! Atomic rename primitives shared by store transactions and archives.
//!
//! The store publishes a generation by swapping two directories, and archives
//! publish by creating a path only if nothing is there. Both need the kernel to
//! do it in one step; a plain `rename` cannot express either.
//!
//! Every supported platform has both operations, under different names:
//!
//! | operation      | Linux                      | macOS                    |
//! |----------------|----------------------------|--------------------------|
//! | swap two paths | `renameat2 RENAME_EXCHANGE`| `renamex_np RENAME_SWAP` |
//! | create or fail | `renameat2 RENAME_NOREPLACE`| `renamex_np RENAME_EXCL`|
//!
//! This module exposes the two operations rather than the flags, so callers do
//! not carry platform detail. Targets without both report `Unsupported` instead
//! of silently degrading to a non-atomic rename, which would let a crash leave
//! a half-published document behind.

use std::ffi::CString;
use std::io::{Error, ErrorKind, Result};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Atomically swap `left` and `right`. Both must exist.
pub(crate) fn exchange(left: &Path, right: &Path) -> Result<()> {
    imp::rename(left, right, imp::EXCHANGE)
}

/// Rename `source` to `destination`, failing if `destination` exists.
pub(crate) fn rename_no_replace(source: &Path, destination: &Path) -> Result<()> {
    imp::rename(source, destination, imp::NO_REPLACE)
}

fn to_c(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "path contains an interior NUL"))
}

#[cfg(target_os = "linux")]
mod imp {
    use std::os::raw::{c_int, c_long};

    use super::*;

    pub(super) const NO_REPLACE: u32 = 1;
    pub(super) const EXCHANGE: u32 = 2;

    const AT_FDCWD: c_int = -100;

    /// `renameat2`'s syscall number on this target.
    ///
    /// Calling the kernel interface directly avoids depending on a glibc-only
    /// `renameat2` symbol, which keeps the operation available in the static
    /// musl release image. Linux numbers the call differently on every
    /// architecture; 276 is the `asm-generic/unistd.h` number shared by the
    /// newer ports.
    const SYS_RENAMEAT2: Option<c_long> = if cfg!(target_arch = "x86_64") {
        Some(316)
    } else if cfg!(any(
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64",
    )) {
        Some(276)
    } else if cfg!(any(target_arch = "powerpc", target_arch = "powerpc64")) {
        Some(357)
    } else if cfg!(target_arch = "s390x") {
        Some(347)
    } else if cfg!(target_arch = "arm") {
        Some(382)
    } else if cfg!(target_arch = "x86") {
        Some(353)
    } else {
        None
    };

    // A Linux build without a number would compile and then fail on the first
    // mutation, so refuse it here instead.
    const _: () = assert!(
        SYS_RENAMEAT2.is_some(),
        "this Linux target's renameat2 syscall number is unknown; add it to SYS_RENAMEAT2"
    );

    pub(super) fn rename(source: &Path, destination: &Path, flags: u32) -> Result<()> {
        let Some(number) = SYS_RENAMEAT2 else {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "this target has no known renameat2 syscall number",
            ));
        };
        let source_c = to_c(source)?;
        let destination_c = to_c(destination)?;

        unsafe extern "C" {
            fn syscall(number: c_long, ...) -> c_long;
        }

        // SAFETY: both C strings are NUL-terminated and live for the duration
        // of the call, and AT_FDCWD makes each absolute path self-contained.
        let result = unsafe {
            syscall(
                number,
                AT_FDCWD,
                source_c.as_ptr(),
                AT_FDCWD,
                destination_c.as_ptr(),
                flags,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(Error::last_os_error())
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::os::raw::{c_char, c_int, c_uint};

    use super::*;

    // <sys/stdio.h>: RENAME_SWAP 0x2, RENAME_EXCL 0x4.
    pub(super) const EXCHANGE: u32 = 0x2;
    pub(super) const NO_REPLACE: u32 = 0x4;

    pub(super) fn rename(source: &Path, destination: &Path, flags: u32) -> Result<()> {
        let source_c = to_c(source)?;
        let destination_c = to_c(destination)?;

        // Available since macOS 10.12; a libc entry point rather than a raw
        // syscall, so no per-architecture number is needed.
        unsafe extern "C" {
            fn renamex_np(from: *const c_char, to: *const c_char, flags: c_uint) -> c_int;
        }

        // SAFETY: both C strings are NUL-terminated and outlive the call.
        let result =
            unsafe { renamex_np(source_c.as_ptr(), destination_c.as_ptr(), flags as c_uint) };
        if result == 0 {
            Ok(())
        } else {
            Err(Error::last_os_error())
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod imp {
    use super::*;

    pub(super) const EXCHANGE: u32 = 0;
    pub(super) const NO_REPLACE: u32 = 0;

    pub(super) fn rename(_source: &Path, _destination: &Path, _flags: u32) -> Result<()> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "this platform has no atomic exchange or create-only rename",
        ))
    }
}
