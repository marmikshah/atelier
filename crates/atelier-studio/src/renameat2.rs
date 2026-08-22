//! Raw `renameat2(2)` bindings shared by store transactions and archives.
//!
//! Calling the kernel interface directly avoids depending on a glibc-only
//! `renameat2` symbol, which keeps the same operation available in the static
//! musl release image.
//!
//! Linux gives the call a different syscall number on every architecture, so
//! the number is selected per target rather than hardcoded. Targets without a
//! known number — including every non-Linux host, where the crate still has to
//! compile for library development — report `Unsupported` instead of silently
//! degrading to a non-atomic rename.

use std::ffi::CString;
use std::io::{Error, ErrorKind, Result};
use std::os::raw::{c_int, c_long};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Fail rather than replace an existing destination.
pub(crate) const RENAME_NOREPLACE: u32 = 1;
/// Atomically swap the two paths.
pub(crate) const RENAME_EXCHANGE: u32 = 2;

const AT_FDCWD: c_int = -100;

/// `renameat2`'s syscall number on this target.
///
/// Values come from the per-architecture Linux syscall tables; 276 is the
/// `asm-generic/unistd.h` number shared by the newer ports.
const SYS_RENAMEAT2: Option<c_long> = if !cfg!(target_os = "linux") {
    None
} else if cfg!(target_arch = "x86_64") {
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
// mutation, so refuse it here instead. Non-Linux targets keep compiling — the
// library is developed on other hosts — and report `Unsupported` at runtime.
#[cfg(target_os = "linux")]
const _: () = assert!(
    SYS_RENAMEAT2.is_some(),
    "this Linux target's renameat2 syscall number is unknown; add it to SYS_RENAMEAT2"
);

/// Rename `source` to `destination` under `flags`.
pub(crate) fn renameat2(source: &Path, destination: &Path, flags: u32) -> Result<()> {
    let Some(number) = SYS_RENAMEAT2 else {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "this target has no known renameat2 syscall number",
        ));
    };

    let source_c = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "path contains an interior NUL"))?;
    let destination_c = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "path contains an interior NUL"))?;

    unsafe extern "C" {
        fn syscall(number: c_long, ...) -> c_long;
    }

    // SAFETY: both C strings are NUL-terminated and live for the duration of
    // the call, and AT_FDCWD makes each absolute path self-contained.
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
