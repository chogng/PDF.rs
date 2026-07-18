//! Unix-only owned-FD transport primitives.
//!
//! The record body stays on the private socket stream. A fixed one-byte marker
//! immediately before it carries the SCM_RIGHTS descriptors named by the
//! record's authenticated capability table. This keeps descriptor association
//! explicit and lets the receiver reject a record before it is dispatched.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::mem::MaybeUninit;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fd::{AsFd, OwnedFd};
use rustix::fs::{Mode, SeekFrom, ftruncate, seek};
use rustix::io::Errno;
use rustix::io::{IoSlice, IoSliceMut, pread, pwrite};
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use rustix::shm;

use crate::{DesktopIpcError, DesktopIpcErrorCode, DesktopIpcLimits, error::error};

const CAPABILITY_MARKER: u8 = 0xa9;
const MAX_FDS_PER_RECORD: usize = 64;
static NEXT_SHARED_MEMORY_NAME: AtomicU64 = AtomicU64::new(1);

fn private_name(prefix: &str, sequence: u64) -> Result<String, DesktopIpcError> {
    let mut random = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!(
        "{prefix}-{}-{sequence}-{suffix}",
        std::process::id()
    ))
}

struct ShmNameGuard {
    name: String,
    linked: bool,
}

impl ShmNameGuard {
    fn new(name: String) -> Self {
        Self { name, linked: true }
    }
    fn unlink(&mut self) -> Result<(), DesktopIpcError> {
        shm::unlink(self.name.as_str()).map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        self.linked = false;
        Ok(())
    }
}

impl Drop for ShmNameGuard {
    fn drop(&mut self) {
        if self.linked {
            let _ = shm::unlink(self.name.as_str());
        }
    }
}

struct TempPathGuard {
    path: std::path::PathBuf,
    linked: bool,
}

impl TempPathGuard {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, linked: true }
    }
    fn unlink(&mut self) -> Result<(), DesktopIpcError> {
        std::fs::remove_file(&self.path).map_err(|_| error(DesktopIpcErrorCode::Lifecycle))?;
        self.linked = false;
        Ok(())
    }
}

impl Drop for TempPathGuard {
    fn drop(&mut self) {
        if self.linked {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Read-only POSIX shared memory created by the Host and safe to pass once.
pub struct ReadOnlySharedRegion {
    fd: OwnedFd,
    length: u64,
}

impl ReadOnlySharedRegion {
    /// Copies immutable Host bytes into a newly unlinked, read-only shared object.
    pub fn from_bytes(bytes: &[u8], limits: DesktopIpcLimits) -> Result<Self, DesktopIpcError> {
        if bytes.is_empty() || bytes.len() > limits.max_capability_bytes() {
            return Err(error(DesktopIpcErrorCode::ResourceLimit));
        }
        let mut writable = None;
        let mut name = None;
        for _ in 0..16 {
            let sequence = NEXT_SHARED_MEMORY_NAME.fetch_add(1, Ordering::Relaxed);
            let candidate = format!("/{}", private_name("pdf-rs", sequence)?);
            match shm::open(
                candidate.as_str(),
                shm::OFlags::CREATE | shm::OFlags::EXCL | shm::OFlags::RDWR,
                Mode::RUSR | Mode::WUSR,
            ) {
                Ok(fd) => {
                    writable = Some(fd);
                    name = Some(candidate);
                    break;
                }
                Err(_) => continue,
            }
        }
        let Some(writable) = writable else {
            // Some macOS sandbox profiles deny POSIX `shm_open`.  Preserve the
            // same Host-only, unlinked, independently reopened read-only FD
            // invariant using a private filesystem object in that environment.
            return private_unlinked_region(bytes);
        };
        let mut name =
            ShmNameGuard::new(name.ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?);
        let length =
            u64::try_from(bytes.len()).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        ftruncate(&writable, length).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
        let mut offset = 0_usize;
        while offset < bytes.len() {
            let wrote = pwrite(
                &writable,
                &bytes[offset..],
                u64::try_from(offset).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?,
            )
            .map_err(|_| error(DesktopIpcErrorCode::Source))?;
            if wrote == 0 {
                return Err(error(DesktopIpcErrorCode::Disconnected));
            }
            offset = offset
                .checked_add(wrote)
                .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
        }
        let readable = shm::open(name.name.as_str(), shm::OFlags::RDONLY, Mode::empty())
            .map_err(|_| error(DesktopIpcErrorCode::Capability))?;
        name.unlink()?;
        drop(writable);
        Ok(Self {
            fd: readable,
            length,
        })
    }

    /// Returns the exact immutable object extent.
    pub const fn byte_length(&self) -> u64 {
        self.length
    }

    /// Returns the owned read-only descriptor for one SCM_RIGHTS transfer.
    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }
}

fn private_unlinked_region(bytes: &[u8]) -> Result<ReadOnlySharedRegion, DesktopIpcError> {
    let mut path = None;
    let mut writable = None;
    for _ in 0..16 {
        let sequence = NEXT_SHARED_MEMORY_NAME.fetch_add(1, Ordering::Relaxed);
        let candidate = std::env::temp_dir().join(private_name("pdf-rs", sequence)?);
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                path = Some(candidate);
                writable = Some(file);
                break;
            }
            Err(_) => continue,
        }
    }
    let mut path = TempPathGuard::new(path.ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?);
    let mut writable = writable.ok_or_else(|| error(DesktopIpcErrorCode::Lifecycle))?;
    writable
        .write_all(bytes)
        .map_err(|_| error(DesktopIpcErrorCode::Source))?;
    writable
        .flush()
        .map_err(|_| error(DesktopIpcErrorCode::Source))?;
    let readable = OpenOptions::new()
        .read(true)
        .open(&path.path)
        .map_err(|_| error(DesktopIpcErrorCode::Capability))?;
    path.unlink()?;
    drop(writable);
    let fd: OwnedFd = readable.into();
    Ok(ReadOnlySharedRegion {
        fd,
        length: u64::try_from(bytes.len())
            .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?,
    })
}

/// Sends one descriptor marker and the handles for the immediately following record.
pub fn send_capability_fds(
    socket: impl AsFd,
    fds: &[OwnedFd],
    limits: DesktopIpcLimits,
) -> Result<(), DesktopIpcError> {
    if fds.len() > limits.max_capabilities() || fds.len() > MAX_FDS_PER_RECORD {
        return Err(error(DesktopIpcErrorCode::ResourceLimit));
    }
    let mut borrowed = Vec::new();
    borrowed
        .try_reserve_exact(fds.len())
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    borrowed.extend(fds.iter().map(AsFd::as_fd));
    let mut space = ancillary_space(fds.len())?;
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !borrowed.is_empty() && !control.push(SendAncillaryMessage::ScmRights(&borrowed)) {
        return Err(error(DesktopIpcErrorCode::ResourceLimit));
    }
    let sent = sendmsg(
        socket,
        &[IoSlice::new(&[CAPABILITY_MARKER])],
        &mut control,
        SendFlags::empty(),
    )
    .map_err(|_| error(DesktopIpcErrorCode::Disconnected))?;
    if sent != 1 {
        return Err(error(DesktopIpcErrorCode::Disconnected));
    }
    Ok(())
}

/// Receives exactly one descriptor table and rejects missing, surplus, or malformed handles.
pub fn receive_capability_fds(
    socket: impl AsFd,
    limits: DesktopIpcLimits,
) -> Result<Vec<OwnedFd>, DesktopIpcError> {
    receive_capability_fds_with_flags(socket, limits, RecvFlags::empty())?
        .ok_or_else(|| error(DesktopIpcErrorCode::Disconnected))
}

/// Attempts one descriptor marker without blocking between bounded Native actor turns.
pub(crate) fn try_receive_capability_fds(
    socket: impl AsFd,
    limits: DesktopIpcLimits,
) -> Result<Option<Vec<OwnedFd>>, DesktopIpcError> {
    receive_capability_fds_with_flags(socket, limits, RecvFlags::DONTWAIT)
}

/// Waits up to the socket's configured record timeout for one descriptor marker.
pub(crate) fn wait_receive_capability_fds(
    socket: impl AsFd,
    limits: DesktopIpcLimits,
) -> Result<Option<Vec<OwnedFd>>, DesktopIpcError> {
    receive_capability_fds_with_flags(socket, limits, RecvFlags::empty())
}

fn receive_capability_fds_with_flags(
    socket: impl AsFd,
    limits: DesktopIpcLimits,
    flags: RecvFlags,
) -> Result<Option<Vec<OwnedFd>>, DesktopIpcError> {
    let mut marker = [0_u8; 1];
    let mut iov = [IoSliceMut::new(&mut marker)];
    let mut space = ancillary_space(limits.max_capabilities())?;
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let message = match recvmsg(socket, &mut iov, &mut control, flags) {
        Ok(message) => message,
        Err(Errno::AGAIN) => return Ok(None),
        Err(_) => return Err(error(DesktopIpcErrorCode::Disconnected)),
    };
    if message.bytes == 0 {
        return Err(error(DesktopIpcErrorCode::Disconnected));
    }
    if message.bytes != 1
        || message
            .flags
            .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
        || marker[0] != CAPABILITY_MARKER
    {
        return Err(error(DesktopIpcErrorCode::InvalidFrame));
    }
    let mut fds = Vec::new();
    fds.try_reserve_exact(limits.max_capabilities())
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    for ancillary in control.drain() {
        match ancillary {
            RecvAncillaryMessage::ScmRights(descriptors) => {
                for fd in descriptors {
                    if fds.len() == limits.max_capabilities() {
                        return Err(error(DesktopIpcErrorCode::ResourceLimit));
                    }
                    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)
                        .map_err(|_| error(DesktopIpcErrorCode::Capability))?;
                    fds.push(fd);
                }
            }
            _ => return Err(error(DesktopIpcErrorCode::Capability)),
        }
    }
    Ok(Some(fds))
}

fn ancillary_space(count: usize) -> Result<Vec<MaybeUninit<u8>>, DesktopIpcError> {
    if count > MAX_FDS_PER_RECORD {
        return Err(error(DesktopIpcErrorCode::ResourceLimit));
    }
    let bytes = rustix::cmsg_space!(ScmRights(count));
    let mut space = Vec::new();
    space
        .try_reserve_exact(bytes)
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    space.resize_with(bytes, MaybeUninit::uninit);
    Ok(space)
}

/// Verifies an imported descriptor is read-only and has exactly the advertised extent.
pub fn validate_read_only_fd(fd: impl AsFd, byte_length: u64) -> Result<(), DesktopIpcError> {
    let flags = rustix::fs::fcntl_getfl(&fd).map_err(|_| error(DesktopIpcErrorCode::Capability))?;
    if flags & rustix::fs::OFlags::RWMODE != rustix::fs::OFlags::RDONLY {
        return Err(error(DesktopIpcErrorCode::Capability));
    }
    let extent = seek(&fd, SeekFrom::End(0)).map_err(|_| error(DesktopIpcErrorCode::Capability))?;
    seek(&fd, SeekFrom::Start(0)).map_err(|_| error(DesktopIpcErrorCode::Capability))?;
    if extent != byte_length {
        return Err(error(DesktopIpcErrorCode::Capability));
    }
    Ok(())
}

/// Copies one already-validated immutable descriptor through bounded positional reads.
pub(crate) fn read_read_only_fd(
    fd: impl AsFd,
    byte_length: u64,
    limits: DesktopIpcLimits,
) -> Result<Vec<u8>, DesktopIpcError> {
    validate_read_only_fd(&fd, byte_length)?;
    let length =
        usize::try_from(byte_length).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    if length == 0 || length > limits.max_capability_bytes() {
        return Err(error(DesktopIpcErrorCode::ResourceLimit));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?;
    if bytes.capacity() > limits.max_capability_bytes() {
        return Err(error(DesktopIpcErrorCode::ResourceLimit));
    }
    bytes.resize(length, 0);
    let mut offset = 0_usize;
    while offset < length {
        let read = pread(
            &fd,
            &mut bytes[offset..],
            u64::try_from(offset).map_err(|_| error(DesktopIpcErrorCode::ResourceLimit))?,
        )
        .map_err(|_| error(DesktopIpcErrorCode::Capability))?;
        if read == 0 {
            return Err(error(DesktopIpcErrorCode::Capability));
        }
        offset = offset
            .checked_add(read)
            .ok_or_else(|| error(DesktopIpcErrorCode::ResourceLimit))?;
    }
    Ok(bytes)
}
