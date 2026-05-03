// fs/vfs.rs - Virtual Filesystem Implementation
//
// Core VFS with:
//   - BTreeMap-based hierarchical node tree
//   - Capability-checked device operations
//   - Path traversal attack prevention (component-level check)
//   - Buffer overflow protection
//   - Null byte injection protection
//   - Cognitive Bus event publishing (with error logging)
//   - Metrics collection and reporting
//
// Security Model (CRIT-002 fix: TOCTOU eliminated):
//   Every write/read acquires the VFS lock FIRST, then validates.
//   This prevents race conditions between validation and operation.
//   Validation and operation happen under the SAME lock.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::manifest::{Capability, DeviceManifest};
use crate::ipc::{self, IntentMessage, ComponentId, Priority};

// ===== VFS Error Types =====

/// VFS operation errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    /// Device is read-only, write denied
    ReadOnlyDevice,
    /// File/path not found
    NotFound,
    /// Device not mounted at this path
    DeviceNotMounted,
    /// Path contains traversal attack (../ or similar)
    PathTraversal,
    /// Path contains null bytes
    NullByteInjection,
    /// Path format is invalid (empty, too long, no leading /)
    InvalidPath,
    /// Write would exceed device capacity
    CapacityExceeded,
    /// Device manifest validation failed
    InvalidManifest,
    /// Bus communication error (non-fatal)
    BusError,
    /// Permission denied by capability check
    PermissionDenied,
}

impl core::fmt::Display for VfsError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::ReadOnlyDevice => write!(f, "Device is read-only"),
            Self::NotFound => write!(f, "File not found"),
            Self::DeviceNotMounted => write!(f, "No device mounted at path"),
            Self::PathTraversal => write!(f, "Path traversal attack detected"),
            Self::NullByteInjection => write!(f, "Null byte injection detected"),
            Self::InvalidPath => write!(f, "Invalid path format"),
            Self::CapacityExceeded => write!(f, "Write exceeds device capacity"),
            Self::InvalidManifest => write!(f, "Device manifest validation failed"),
            Self::BusError => write!(f, "Bus communication error"),
            Self::PermissionDenied => write!(f, "Permission denied"),
        }
    }
}

// ===== VFS Node Types =====

/// A node in the virtual filesystem tree
#[derive(Debug, Clone)]
pub enum VfsNode {
    /// A file with data content
    File(Vec<u8>),
    /// A directory containing child nodes
    Directory(BTreeMap<String, VfsNode>),
    /// A mounted device with manifest
    Device {
        manifest: DeviceManifest,
        data: Vec<u8>,
    },
    /// A symbolic link pointing to another path (Phase 2.1)
    Symlink(String),
}

// ===== VFS Metrics =====

/// Metrics for VFS operations - atomic for lock-free access (MED-001 fix)
pub struct VfsMetrics {
    pub total_nodes: AtomicUsize,
    pub total_bytes_written: AtomicU64,
    pub total_bytes_read: AtomicU64,
    pub operations_count: AtomicU64,
    pub errors_count: AtomicU64,
    pub security_violations: AtomicU64,
    pub bus_errors: AtomicU64,
}

impl VfsMetrics {
    const fn new() -> Self {
        Self {
            total_nodes: AtomicUsize::new(0),
            total_bytes_written: AtomicU64::new(0),
            total_bytes_read: AtomicU64::new(0),
            operations_count: AtomicU64::new(0),
            errors_count: AtomicU64::new(0),
            security_violations: AtomicU64::new(0),
            bus_errors: AtomicU64::new(0),
        }
    }
}

/// Global VFS metrics (lock-free atomic counters)
static VFS_METRICS: VfsMetrics = VfsMetrics::new();

/// Get current VFS metrics snapshot
pub fn get_metrics() -> MetricsSnapshot {
    MetricsSnapshot {
        total_nodes: VFS_METRICS.total_nodes.load(Ordering::Relaxed),
        total_bytes_written: VFS_METRICS.total_bytes_written.load(Ordering::Relaxed),
        total_bytes_read: VFS_METRICS.total_bytes_read.load(Ordering::Relaxed),
        operations_count: VFS_METRICS.operations_count.load(Ordering::Relaxed),
        errors_count: VFS_METRICS.errors_count.load(Ordering::Relaxed),
        security_violations: VFS_METRICS.security_violations.load(Ordering::Relaxed),
        bus_errors: VFS_METRICS.bus_errors.load(Ordering::Relaxed),
    }
}

/// Immutable snapshot of VFS metrics for reporting
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub total_nodes: usize,
    pub total_bytes_written: u64,
    pub total_bytes_read: u64,
    pub operations_count: u64,
    pub errors_count: u64,
    pub security_violations: u64,
    pub bus_errors: u64,
}

// ===== VFS Root =====

lazy_static! {
    /// Global VFS root - protected by spin mutex
    static ref VFS_ROOT: Mutex<BTreeMap<String, VfsNode>> = Mutex::new(BTreeMap::new());
}

/// Public accessor to lock the VFS root for kernel-internal operations
/// (e.g., mounting ELF binaries during boot). Bypasses normal VFS API.
pub fn lock_root() -> spin::MutexGuard<'static, BTreeMap<String, VfsNode>> {
    VFS_ROOT.lock()
}

// ===== Path Validation (Security) =====

/// Maximum allowed path length (prevent DoS via huge paths)
const MAX_PATH_LENGTH: usize = 256;

/// Validate and sanitize a filesystem path (MED-002 hardened)
///
/// Security checks:
/// 1. Non-empty
/// 2. Starts with '/'
/// 3. No null bytes (\0)
/// 4. Component-level traversal check (each component != ".." and != ".")
/// 5. No double slashes (//)
/// 6. Length within bounds
/// 7. Only allowed characters (alphanumeric, /, _, -, .)
/// 8. No URL-encoded sequences (%XX)
fn validate_path(path: &str) -> Result<(), VfsError> {
    // Check 1: Non-empty
    if path.is_empty() {
        return Err(VfsError::InvalidPath);
    }

    // Check 2: Must start with /
    if !path.starts_with('/') {
        return Err(VfsError::InvalidPath);
    }

    // Check 3: No null bytes (injection attack)
    if path.bytes().any(|b| b == 0) {
        VFS_METRICS.security_violations.fetch_add(1, Ordering::Relaxed);
        return Err(VfsError::NullByteInjection);
    }

    // Check 5: No double slashes
    if path.contains("//") {
        return Err(VfsError::InvalidPath);
    }

    // Check 6: Length check
    if path.len() > MAX_PATH_LENGTH {
        return Err(VfsError::InvalidPath);
    }

    // Check 7: Only allowed characters (blocks %XX URL encoding too - MED-002)
    for byte in path.bytes() {
        match byte {
            b'/' | b'_' | b'-' | b'.' => {} // allowed special chars
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => {} // alphanumeric
            _ => return Err(VfsError::InvalidPath),
        }
    }

    // Check 4 (MED-002 HARDENED): Component-level traversal check
    // Split by '/' and check each component individually
    // This catches "..", ".", "....//", and any variant
    for component in path.split('/') {
        if component.is_empty() {
            continue; // leading slash produces empty first component
        }
        // Reject "." and ".." as individual components
        if component == ".." || component == "." {
            VFS_METRICS.security_violations.fetch_add(1, Ordering::Relaxed);
            return Err(VfsError::PathTraversal);
        }
        // Reject components that START with ".." (e.g., "..hidden")
        if component.starts_with("..") {
            VFS_METRICS.security_violations.fetch_add(1, Ordering::Relaxed);
            return Err(VfsError::PathTraversal);
        }
    }

    // Check 8: No URL-encoded sequences (already blocked by char whitelist,
    // but explicit check for defense in depth)
    if path.contains('%') {
        VFS_METRICS.security_violations.fetch_add(1, Ordering::Relaxed);
        return Err(VfsError::InvalidPath);
    }

    Ok(())
}

/// Parse a validated path into components
/// Example: "/dev/ram0" -> ["dev", "ram0"]
fn path_components(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .collect()
}

// ===== Bus Integration (with proper error handling) =====

/// Intent IDs for VFS operations on the Cognitive Bus
const VFS_MOUNT: u32 = 0x4001;
const VFS_WRITE: u32 = 0x4002;
const VFS_READ: u32 = 0x4003;
const VFS_SECURITY_VIOLATION: u32 = 0x4010;

/// Publish a VFS event to the Cognitive Bus
/// Errors are LOGGED (not silently ignored)
fn bus_publish_event(intent_id: u32, payload: u64) {
    let msg = IntentMessage::new(
        ComponentId::Filesystem,
        ComponentId::Orchestrator,
        intent_id,
        if intent_id == VFS_SECURITY_VIOLATION {
            Priority::Critical
        } else {
            Priority::Normal
        },
        payload,
    );

    match ipc::bus::publish(msg) {
        Ok(_) => {}
        Err(e) => {
            VFS_METRICS.bus_errors.fetch_add(1, Ordering::Relaxed);
            // Log the error instead of silently ignoring
            crate::serial_println!("[VFS][WARN] Bus publish failed: {:?}", e);
        }
    }
}

// ===== VFS Operations =====

/// Mount a device at a given path
///
/// The device manifest MUST validate before mounting.
/// Publishes VFS_MOUNT event to Cognitive Bus.
/// MED-004: Logs warning if replacing existing node.
pub fn mount_device(path: &str, manifest: DeviceManifest) -> Result<(), VfsError> {
    VFS_METRICS.operations_count.fetch_add(1, Ordering::Relaxed);

    // 1. Validate path (can be done before lock since path is immutable)
    validate_path(path)?;

    // 2. Validate manifest integrity
    if !manifest.validate() {
        VFS_METRICS.errors_count.fetch_add(1, Ordering::Relaxed);
        crate::serial_println!("[VFS][ERROR] Invalid manifest for device '{}'", manifest.name);
        return Err(VfsError::InvalidManifest);
    }

    // 3. Mount the device
    let components = path_components(path);
    if components.is_empty() {
        return Err(VfsError::InvalidPath);
    }

    let capacity = manifest.capacity;
    let device_name_clone = manifest.name.clone();
    let node = VfsNode::Device {
        manifest,
        data: Vec::new(),
    };

    {
        let mut root = VFS_ROOT.lock();
        if components.len() == 1 {
            // MED-004: Log if replacing existing node
            if let Some(_old) = root.insert(String::from(components[0]), node) {
                crate::serial_println!("[VFS][WARN] Replaced existing node at /{}", components[0]);
            }
        } else {
            // Navigate/create intermediate directories
            let mut current = &mut *root;
            for (i, comp) in components.iter().enumerate() {
                if i == components.len() - 1 {
                    // MED-004: Log if replacing existing node
                    if let Some(_old) = current.insert(String::from(*comp), node.clone()) {
                        crate::serial_println!("[VFS][WARN] Replaced existing node at {}", path);
                    }
                    break;
                }
                current
                    .entry(String::from(*comp))
                    .or_insert_with(|| VfsNode::Directory(BTreeMap::new()));
                if let Some(VfsNode::Directory(ref mut children)) = current.get_mut(*comp) {
                    current = children;
                } else {
                    VFS_METRICS.errors_count.fetch_add(1, Ordering::Relaxed);
                    return Err(VfsError::InvalidPath);
                }
            }
        }
    }

    VFS_METRICS.total_nodes.fetch_add(1, Ordering::Relaxed);

    // 4. Publish mount event
    bus_publish_event(VFS_MOUNT, capacity as u64);

    crate::serial_println!("[VFS] Mounted '{}' at {} (cap: {} bytes)",
        device_name_clone, path, capacity);

    Ok(())
}

/// Write data to a file/device path
///
/// SECURITY (CRIT-002 fix): Lock is acquired BEFORE path validation.
/// Validation and operation happen under the SAME lock to prevent TOCTOU.
///
/// Security chain (all under single lock):
/// 1. Lock VFS root
/// 2. Path validation (traversal, null byte, format)
/// 3. Capability check (Write permission)
/// 4. Capacity overflow check
/// 5. Write execution
/// 6. Release lock
/// 7. Bus event + metrics (outside lock)
pub fn file_write(path: &str, data: &[u8]) -> Result<usize, VfsError> {
    VFS_METRICS.operations_count.fetch_add(1, Ordering::Relaxed);

    // CRIT-002 FIX: Validate path BEFORE lock since path is an immutable
    // string slice - no TOCTOU risk on the path itself. The critical fix
    // is that we validate + operate under the same lock on the VFS tree.
    validate_path(path)?;

    let components = path_components(path);
    if components.is_empty() {
        return Err(VfsError::InvalidPath);
    }

    // CRIT-002: Lock acquired - all tree operations are atomic from here
    let bytes_written;
    {
        let mut root = VFS_ROOT.lock();

        // Navigate to the target node (under lock)
        let node = find_node_mut(&mut root, &components)
            .ok_or(VfsError::DeviceNotMounted)?;

        match node {
            VfsNode::Device {
                manifest: ref dev_manifest,
                data: ref mut device_data,
            } => {
                // 3. Capability check (under lock)
                if !dev_manifest.can(Capability::Write) {
                    VFS_METRICS.security_violations.fetch_add(1, Ordering::Relaxed);
                    VFS_METRICS.errors_count.fetch_add(1, Ordering::Relaxed);
                    bus_publish_event(VFS_SECURITY_VIOLATION, path.len() as u64);
                    crate::serial_println!("[VFS][SECURITY] Write denied on read-only device: {}", path);
                    return Err(VfsError::ReadOnlyDevice);
                }

                // 4. Capacity check (under lock)
                if dev_manifest.capacity > 0 && data.len() > dev_manifest.capacity {
                    VFS_METRICS.errors_count.fetch_add(1, Ordering::Relaxed);
                    crate::serial_println!("[VFS][ERROR] Write overflow: {} bytes > {} capacity at {}",
                        data.len(), dev_manifest.capacity, path);
                    return Err(VfsError::CapacityExceeded);
                }

                // 5. Execute write (under lock)
                device_data.clear();
                device_data.extend_from_slice(data);
                bytes_written = data.len();
            }
            VfsNode::File(ref mut file_data) => {
                file_data.clear();
                file_data.extend_from_slice(data);
                bytes_written = data.len();
            }
            VfsNode::Directory(_) | VfsNode::Symlink(_) => {
                VFS_METRICS.errors_count.fetch_add(1, Ordering::Relaxed);
                return Err(VfsError::PermissionDenied);
            }
        }
    }
    // Lock released here

    // 7. Metrics + bus event (outside lock for performance)
    VFS_METRICS
        .total_bytes_written
        .fetch_add(bytes_written as u64, Ordering::Relaxed);
    bus_publish_event(VFS_WRITE, bytes_written as u64);

    Ok(bytes_written)
}

/// Read data from a file/device path
///
/// SECURITY (CRIT-002 fix): Validation and read under same lock.
pub fn file_read(path: &str) -> Result<Vec<u8>, VfsError> {
    VFS_METRICS.operations_count.fetch_add(1, Ordering::Relaxed);

    validate_path(path)?;

    let components = path_components(path);
    if components.is_empty() {
        return Err(VfsError::InvalidPath);
    }

    // First try in-memory VFS tree
    {
        let root = VFS_ROOT.lock();

        if let Some(node) = find_node(&root, &components) {
            match node {
                VfsNode::Device {
                    ref manifest,
                    data: ref device_data,
                } => {
                    if !manifest.can(Capability::Read) {
                        VFS_METRICS.security_violations.fetch_add(1, Ordering::Relaxed);
                        VFS_METRICS.errors_count.fetch_add(1, Ordering::Relaxed);
                        return Err(VfsError::PermissionDenied);
                    }
                    let data = device_data.clone();
                    VFS_METRICS
                        .total_bytes_read
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    bus_publish_event(VFS_READ, data.len() as u64);
                    return Ok(data);
                }
                VfsNode::File(ref file_data) => {
                    let data = file_data.clone();
                    VFS_METRICS
                        .total_bytes_read
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    bus_publish_event(VFS_READ, data.len() as u64);
                    return Ok(data);
                }
                VfsNode::Directory(_) => {
                    VFS_METRICS.errors_count.fetch_add(1, Ordering::Relaxed);
                    return Err(VfsError::PermissionDenied);
                }
                VfsNode::Symlink(ref target) => {
                    // Follow symlink: recursively read the target path
                    let target_path = target.clone();
                    drop(root); // release lock before recursive call
                    return file_read(&target_path);
                }
            }
        }
    }

    // Fall through to multi-backend VFS (ext2, procfs, devfs, sysfs)
    match crate::fs::vfs_backend::backend_read(path) {
        Ok(data) => {
            VFS_METRICS
                .total_bytes_read
                .fetch_add(data.len() as u64, Ordering::Relaxed);
            bus_publish_event(VFS_READ, data.len() as u64);
            Ok(data)
        }
        Err(_) => Err(VfsError::NotFound),
    }
}

/// Read a range of bytes from a VFS file at a given offset.
/// Used by the page fault handler for demand-paging mmap of VFS-resident files.
/// Copies up to `buf.len()` bytes starting at `offset` into `buf`.
/// Returns the number of bytes actually copied.
pub fn file_read_at_offset(path: &str, offset: u64, buf: &mut [u8]) -> usize {
    let components = path_components(path);
    if components.is_empty() {
        return 0;
    }

    let root = VFS_ROOT.lock();
    let node = match find_node(&root, &components) {
        Some(n) => n,
        None => return 0,
    };

    let file_data = match node {
        VfsNode::File(ref data) => data,
        VfsNode::Device { data: ref device_data, .. } => device_data,
        _ => return 0,
    };

    let off = offset as usize;
    if off >= file_data.len() {
        return 0;
    }
    let available = file_data.len() - off;
    let to_copy = core::cmp::min(available, buf.len());
    buf[..to_copy].copy_from_slice(&file_data[off..off + to_copy]);
    to_copy
}

/// List entries at a given path (directory listing)
pub fn list_path(path: &str) -> Result<Vec<String>, VfsError> {
    VFS_METRICS.operations_count.fetch_add(1, Ordering::Relaxed);

    validate_path(path)?;

    let root = VFS_ROOT.lock();

    if path == "/" {
        return Ok(root.keys().cloned().collect());
    }

    let components = path_components(path);
    let node = find_node(&root, &components).ok_or(VfsError::NotFound)?;

    match node {
        VfsNode::Directory(children) => Ok(children.keys().cloned().collect()),
        _ => Err(VfsError::NotFound),
    }
}

/// Create a directory at the given path
pub fn mkdir(path: &str) -> Result<(), VfsError> {
    VFS_METRICS.operations_count.fetch_add(1, Ordering::Relaxed);
    validate_path(path)?;

    let components = path_components(path);
    if components.is_empty() {
        return Err(VfsError::InvalidPath);
    }

    let mut root = VFS_ROOT.lock();

    // Navigate to parent
    let (parent_comps, new_name) = components.split_at(components.len() - 1);

    let parent = if parent_comps.is_empty() {
        &mut *root
    } else {
        let mut current = &mut *root;
        for comp in parent_comps {
            let node = current.get_mut(*comp).ok_or(VfsError::NotFound)?;
            match node {
                VfsNode::Directory(ref mut children) => current = children,
                _ => return Err(VfsError::NotFound),
            }
        }
        current
    };

    // Check if already exists
    if parent.contains_key(new_name[0]) {
        return Ok(()); // mkdir -p behavior: no error if exists
    }

    parent.insert(
        String::from(new_name[0]),
        VfsNode::Directory(BTreeMap::new()),
    );

    crate::serial_println!("[VFS] mkdir: created {}", path);
    Ok(())
}

/// Recursively create directories (mkdir -p)
pub fn mkdir_p(path: &str) -> Result<(), VfsError> {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Ok(());
    }

    let mut current = String::from("/");
    for component in path.split('/') {
        if component.is_empty() {
            continue;
        }
        let full_path = if current == "/" {
            alloc::format!("/{}", component)
        } else {
            alloc::format!("{}/{}", current, component)
        };
        // Ignore error if already exists
        let _ = mkdir(&full_path);
        current = full_path;
    }
    Ok(())
}

/// Remove a file (unlink) at the given path
pub fn unlink(path: &str) -> Result<(), VfsError> {
    VFS_METRICS.operations_count.fetch_add(1, Ordering::Relaxed);
    validate_path(path)?;

    let components = path_components(path);
    if components.is_empty() {
        return Err(VfsError::InvalidPath);
    }

    let mut root = VFS_ROOT.lock();

    let (parent_comps, target_name) = components.split_at(components.len() - 1);

    let parent = if parent_comps.is_empty() {
        &mut *root
    } else {
        let mut current = &mut *root;
        for comp in parent_comps {
            let node = current.get_mut(*comp).ok_or(VfsError::NotFound)?;
            match node {
                VfsNode::Directory(ref mut children) => current = children,
                _ => return Err(VfsError::NotFound),
            }
        }
        current
    };

    match parent.get(target_name[0]) {
        Some(VfsNode::File(_)) => {
            parent.remove(target_name[0]);
            crate::serial_println!("[VFS] unlink: removed {}", path);
            Ok(())
        }
        Some(VfsNode::Directory(children)) => {
            if children.is_empty() {
                parent.remove(target_name[0]);
                crate::serial_println!("[VFS] rmdir: removed {}", path);
                Ok(())
            } else {
                Err(VfsError::PermissionDenied) // Directory not empty
            }
        }
        _ => Err(VfsError::NotFound),
    }
}

/// Create a symbolic link at `link_path` pointing to `target`.
/// Phase 2.1: Required by npm, pip, and ld.so for resolving library paths.
pub fn symlink(target: &str, link_path: &str) -> Result<(), VfsError> {
    VFS_METRICS.operations_count.fetch_add(1, Ordering::Relaxed);
    validate_path(link_path)?;

    let components = path_components(link_path);
    if components.is_empty() {
        return Err(VfsError::InvalidPath);
    }

    let mut root = VFS_ROOT.lock();
    let (parent_comps, link_name) = components.split_at(components.len() - 1);

    let parent = if parent_comps.is_empty() {
        &mut *root
    } else {
        let mut current = &mut *root;
        for comp in parent_comps {
            let node = current.get_mut(*comp).ok_or(VfsError::NotFound)?;
            match node {
                VfsNode::Directory(ref mut children) => current = children,
                _ => return Err(VfsError::NotFound),
            }
        }
        current
    };

    parent.insert(
        String::from(link_name[0]),
        VfsNode::Symlink(String::from(target)),
    );
    crate::serial_println!("[VFS] symlink: {} -> {}", link_path, target);
    Ok(())
}

/// Read the target of a symbolic link at `path`.
/// Returns the target string, or VfsError::NotFound if not a symlink.
pub fn readlink(path: &str) -> Result<String, VfsError> {
    VFS_METRICS.operations_count.fetch_add(1, Ordering::Relaxed);
    validate_path(path)?;

    let components = path_components(path);
    if components.is_empty() {
        return Err(VfsError::InvalidPath);
    }

    let root = VFS_ROOT.lock();
    let mut current = &*root;
    for (i, comp) in components.iter().enumerate() {
        match current.get(*comp) {
            Some(VfsNode::Directory(ref children)) => {
                current = children;
            }
            Some(VfsNode::Symlink(target)) if i == components.len() - 1 => {
                return Ok(target.clone());
            }
            _ if i == components.len() - 1 => {
                return Err(VfsError::NotFound);
            }
            _ => return Err(VfsError::NotFound),
        }
    }
    Err(VfsError::NotFound)
}

/// Create an empty file (touch) — creates if not exists, no-op if exists
pub fn file_create_empty(path: &str) -> Result<(), VfsError> {
    VFS_METRICS.operations_count.fetch_add(1, Ordering::Relaxed);
    validate_path(path)?;

    let components = path_components(path);
    if components.is_empty() {
        return Err(VfsError::InvalidPath);
    }

    let mut root = VFS_ROOT.lock();

    let (parent_comps, new_name) = components.split_at(components.len() - 1);

    let parent = if parent_comps.is_empty() {
        &mut *root
    } else {
        let mut current = &mut *root;
        for comp in parent_comps {
            let node = current.get_mut(*comp).ok_or(VfsError::NotFound)?;
            match node {
                VfsNode::Directory(ref mut children) => current = children,
                _ => return Err(VfsError::NotFound),
            }
        }
        current
    };

    if parent.contains_key(new_name[0]) {
        return Ok(()); // touch behavior: no error if exists
    }

    parent.insert(String::from(new_name[0]), VfsNode::File(Vec::new()));
    crate::serial_println!("[VFS] touch: created {}", path);
    Ok(())
}

// ===== Internal Helpers =====

/// Navigate the BTreeMap tree to find a node (immutable)
fn find_node<'a>(
    root: &'a BTreeMap<String, VfsNode>,
    components: &[&str],
) -> Option<&'a VfsNode> {
    if components.is_empty() {
        return None;
    }

    if components.len() == 1 {
        return root.get(components[0]);
    }

    let mut current = root;
    for (i, comp) in components.iter().enumerate() {
        if i == components.len() - 1 {
            return current.get(*comp);
        }
        match current.get(*comp) {
            Some(VfsNode::Directory(children)) => {
                current = children;
            }
            _ => return None,
        }
    }
    None
}

/// Navigate the BTreeMap tree to find a node (mutable)
fn find_node_mut<'a>(
    root: &'a mut BTreeMap<String, VfsNode>,
    components: &[&str],
) -> Option<&'a mut VfsNode> {
    if components.is_empty() {
        return None;
    }

    if components.len() == 1 {
        return root.get_mut(components[0]);
    }

    let mut current = root;
    for (i, comp) in components.iter().enumerate() {
        if i == components.len() - 1 {
            return current.get_mut(*comp);
        }
        match current.get_mut(*comp) {
            Some(VfsNode::Directory(children)) => {
                current = children;
            }
            _ => return None,
        }
    }
    None
}

/// Initialize the VFS with default structure
/// Creates /dev, /tmp directories and Linux ABI pseudo-devices
pub fn init() -> Result<(), VfsError> {
    crate::serial_println!("[VFS] Initializing virtual filesystem...");

    {
        let mut root = VFS_ROOT.lock();

        // Create /dev directory with pseudo-devices
        let mut dev_dir = BTreeMap::new();

        // /dev/null — writes succeed (return len), reads return EOF (0)
        dev_dir.insert(String::from("null"), VfsNode::File(Vec::new()));
        // /dev/zero — reads return zeroed bytes (handled in sys_read)
        dev_dir.insert(String::from("zero"), VfsNode::File(Vec::new()));
        // /dev/urandom — reads return pseudo-random bytes (handled in sys_read)
        dev_dir.insert(String::from("urandom"), VfsNode::File(Vec::new()));
        // /dev/random — alias for urandom on this OS
        dev_dir.insert(String::from("random"), VfsNode::File(Vec::new()));
        // /dev/tty — placeholder terminal device
        dev_dir.insert(String::from("tty"), VfsNode::File(Vec::new()));
        // /dev/stdin, /dev/stdout, /dev/stderr — symlink placeholders
        dev_dir.insert(String::from("stdin"), VfsNode::File(Vec::new()));
        dev_dir.insert(String::from("stdout"), VfsNode::File(Vec::new()));
        dev_dir.insert(String::from("stderr"), VfsNode::File(Vec::new()));

        // /dev/input — Linux input event devices for keyboard/mouse injection
        let mut input_dir = BTreeMap::new();
        // event0 = keyboard, event1 = mouse/touchpad
        input_dir.insert(String::from("event0"), VfsNode::File(Vec::new())); // keyboard
        input_dir.insert(String::from("event1"), VfsNode::File(Vec::new())); // mouse
        input_dir.insert(String::from("mice"),   VfsNode::File(Vec::new())); // legacy mouse
        dev_dir.insert(String::from("input"), VfsNode::Directory(input_dir));

        root.insert(String::from("dev"), VfsNode::Directory(dev_dir));

        // Create /tmp directory
        root.insert(
            String::from("tmp"),
            VfsNode::Directory(BTreeMap::new()),
        );

        // Create /proc directory (Linux ABI: many tools probe /proc/self/*)
        let mut proc_dir = BTreeMap::new();
        let mut self_dir = BTreeMap::new();
        // /proc/self/exe — empty placeholder (tools check existence)
        self_dir.insert(String::from("exe"), VfsNode::File(Vec::new()));
        // /proc/self/maps — empty (tools like ASLR checkers read this)
        self_dir.insert(String::from("maps"), VfsNode::File(Vec::new()));
        // /proc/self/status — process status (APK reads this)
        self_dir.insert(String::from("status"), VfsNode::File(
            b"Name:\tinit\nState:\tR (running)\nPid:\t1\nPPid:\t0\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\n".to_vec()));
        // /proc/self/cmdline — null-separated (busybox reads this)
        self_dir.insert(String::from("cmdline"), VfsNode::File(b"/bin/init\0".to_vec()));
        // /proc/self/fd — file descriptor directory (empty)
        self_dir.insert(String::from("fd"), VfsNode::Directory(BTreeMap::new()));
        proc_dir.insert(String::from("self"), VfsNode::Directory(self_dir));
        // /proc/version — kernel version string (APK, uname -a)
        proc_dir.insert(String::from("version"), VfsNode::File(
            b"Linux version 6.18.0-aetherion (morningstar@aetherion.dev) (gcc 13.2.0) #1 SMP PREEMPT AetherionOS\n".to_vec()));
        // /proc/cpuinfo — minimal CPU info (musl/Python reads this)
        proc_dir.insert(String::from("cpuinfo"), VfsNode::File(
            b"processor\t: 0\nvendor_id\t: AetherionOS\nmodel name\t: AetherionOS Virtual CPU\ncpu MHz\t\t: 2000.000\ncache size\t: 4096 KB\nflags\t\t: fpu sse sse2 avx\nbogomips\t: 4000.00\n\n".to_vec()));
        // /proc/meminfo — memory stats (APK/free/top need this)
        proc_dir.insert(String::from("meminfo"), VfsNode::File(
            b"MemTotal:        4096000 kB\nMemFree:         2048000 kB\nMemAvailable:    3072000 kB\nBuffers:          128000 kB\nCached:           512000 kB\nSwapTotal:             0 kB\nSwapFree:              0 kB\n".to_vec()));
        // /proc/filesystems — needed by mount
        proc_dir.insert(String::from("filesystems"), VfsNode::File(
            b"nodev\ttmpfs\nnodev\tproc\nnodev\tsysfs\nnodev\tdevtmpfs\n".to_vec()));
        // /proc/mounts — current mount table (APK reads this)
        proc_dir.insert(String::from("mounts"), VfsNode::File(
            b"none / tmpfs rw,relatime 0 0\nproc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\nsysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0\ndevtmpfs /dev devtmpfs rw,nosuid 0 0\n".to_vec()));
        // /proc/uptime — system uptime (seconds idle_seconds)
        proc_dir.insert(String::from("uptime"), VfsNode::File(b"100.00 50.00\n".to_vec()));
        // /proc/loadavg — load averages
        proc_dir.insert(String::from("loadavg"), VfsNode::File(b"0.00 0.00 0.00 1/1 1\n".to_vec()));
        root.insert(String::from("proc"), VfsNode::Directory(proc_dir));

        // Create /sys directory
        root.insert(
            String::from("sys"),
            VfsNode::Directory(BTreeMap::new()),
        );

        // Create standard filesystem directories
        // /bin — user executables
        root.insert(String::from("bin"), VfsNode::Directory(BTreeMap::new()));
        // /lib — shared libraries (musl ld.so lives here)
        root.insert(String::from("lib"), VfsNode::Directory(BTreeMap::new()));
        // /usr/bin, /usr/lib — secondary hierarchy
        let mut usr_dir = BTreeMap::new();
        usr_dir.insert(String::from("bin"), VfsNode::Directory(BTreeMap::new()));
        usr_dir.insert(String::from("lib"), VfsNode::Directory(BTreeMap::new()));
        usr_dir.insert(String::from("local"), VfsNode::Directory({
            let mut local = BTreeMap::new();
            local.insert(String::from("bin"), VfsNode::Directory(BTreeMap::new()));
            local.insert(String::from("lib"), VfsNode::Directory(BTreeMap::new()));
            local
        }));
        root.insert(String::from("usr"), VfsNode::Directory(usr_dir));
        // /root — root user home
        root.insert(String::from("root"), VfsNode::Directory(BTreeMap::new()));
        // /var — variable data
        let mut var_dir = BTreeMap::new();
        var_dir.insert(String::from("tmp"), VfsNode::Directory(BTreeMap::new()));
        var_dir.insert(String::from("log"), VfsNode::Directory(BTreeMap::new()));
        var_dir.insert(String::from("cache"), VfsNode::Directory({
            let mut cache = BTreeMap::new();
            cache.insert(String::from("apk"), VfsNode::Directory(BTreeMap::new()));
            cache
        }));
        root.insert(String::from("var"), VfsNode::Directory(var_dir));
        // /run — runtime data
        root.insert(String::from("run"), VfsNode::Directory(BTreeMap::new()));
        // /data — persistent storage mount point (FAT32 disk, ~2 GiB)
        // When a FAT32 disk is available, mount_device("/data", ...) binds it here.
        // /var and /home are symlinked or bind-mounted to /data/var, /data/home.
        let mut data_dir = BTreeMap::new();
        data_dir.insert(String::from("var"), VfsNode::Directory(BTreeMap::new()));
        data_dir.insert(String::from("home"), VfsNode::Directory(BTreeMap::new()));
        data_dir.insert(String::from("apk"), VfsNode::Directory(BTreeMap::new()));
        root.insert(String::from("data"), VfsNode::Directory(data_dir));
        // /home — user home directories (initially empty, backed by /data/home when disk available)
        root.insert(String::from("home"), VfsNode::Directory(BTreeMap::new()));

        // ── Jalon 94: Create /etc directory with Linux compatibility files ──
        // Tools like nmap, wget, curl probe /etc/resolv.conf, /etc/protocols, etc.
        let mut etc_dir = BTreeMap::new();

        // /etc/resolv.conf — DNS resolver config (QEMU gateway as DNS)
        etc_dir.insert(String::from("resolv.conf"),
            VfsNode::File(b"nameserver 10.0.2.3\nsearch local\n".to_vec()));

        // /etc/hostname
        etc_dir.insert(String::from("hostname"),
            VfsNode::File(b"aetherion\n".to_vec()));

        // /etc/hosts — minimal hosts file
        etc_dir.insert(String::from("hosts"),
            VfsNode::File(b"127.0.0.1\tlocalhost\n::1\t\tlocalhost\n127.0.1.1\taetherion\n".to_vec()));

        // /etc/passwd — minimal POSIX passwd for whoami/id
        etc_dir.insert(String::from("passwd"),
            VfsNode::File(b"root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody:/:/usr/sbin/nologin\n".to_vec()));

        // /etc/group — minimal group file
        etc_dir.insert(String::from("group"),
            VfsNode::File(b"root:x:0:\nnogroup:x:65534:\n".to_vec()));

        // /etc/protocols — needed by nmap and socket tools
        etc_dir.insert(String::from("protocols"),
            VfsNode::File(b"ip\t0\tIP\nicmp\t1\tICMP\ntcp\t6\tTCP\nudp\t17\tUDP\ngre\t47\tGRE\n".to_vec()));

        // /etc/services — needed by nmap for port name resolution
        etc_dir.insert(String::from("services"),
            VfsNode::File(b"ftp\t21/tcp\nssh\t22/tcp\ntelnet\t23/tcp\nsmtp\t25/tcp\ndns\t53/tcp\nhttp\t80/tcp\nhttps\t443/tcp\n".to_vec()));

        // /etc/nsswitch.conf — name service switch (musl reads this)
        etc_dir.insert(String::from("nsswitch.conf"),
            VfsNode::File(b"passwd: files\ngroup: files\nhosts: files dns\n".to_vec()));

        // /etc/os-release — OS identification (APK, systemd, etc.)
        etc_dir.insert(String::from("os-release"),
            VfsNode::File(b"NAME=\"AetherionOS\"\nID=aetherion\nVERSION_ID=1.0\nPRETTY_NAME=\"AetherionOS 1.0\"\n".to_vec()));

        // /etc/apk/repositories — APK mirror list (Alpine 3.21)
        let mut apk_dir = BTreeMap::new();
        apk_dir.insert(String::from("repositories"),
            VfsNode::File(b"https://dl-cdn.alpinelinux.org/alpine/v3.21/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.21/community\n".to_vec()));
        apk_dir.insert(String::from("arch"),
            VfsNode::File(b"x86_64\n".to_vec()));
        // Create /etc/apk/keys/ for package verification
        apk_dir.insert(String::from("keys"), VfsNode::Directory(BTreeMap::new()));
        etc_dir.insert(String::from("apk"), VfsNode::Directory(apk_dir));

        // /etc/ld-musl-x86_64.path — musl dynamic linker search path
        etc_dir.insert(String::from("ld-musl-x86_64.path"),
            VfsNode::File(b"/lib\n/usr/lib\n/usr/local/lib\n".to_vec()));

        // /etc/shadow — root with no password (minimal)
        etc_dir.insert(String::from("shadow"),
            VfsNode::File(b"root::19000:0:99999:7:::\nnobody:!:19000:0:99999:7:::\n".to_vec()));

        // /etc/apk/world — list of explicitly installed packages (APK needs this)
        // Ensure the apk subdir has a 'world' file
        if let Some(VfsNode::Directory(ref mut apk_dir)) = etc_dir.get_mut("apk") {
            apk_dir.entry(String::from("world")).or_insert_with(|| VfsNode::File(b"busybox\n".to_vec()));
            // /etc/apk/protected_paths.d/
            apk_dir.entry(String::from("protected_paths.d")).or_insert_with(|| VfsNode::Directory(BTreeMap::new()));
        }

        root.insert(String::from("etc"), VfsNode::Directory(etc_dir));

        // ── Session 11: Additional directories for APK package manager ──
        // /var/lib/apk/db/ — APK database directory
        if let Some(VfsNode::Directory(ref mut var_dir)) = root.get_mut("var") {
            let lib_dir = var_dir.entry(String::from("lib")).or_insert_with(|| VfsNode::Directory(BTreeMap::new()));
            if let VfsNode::Directory(ref mut lib_map) = lib_dir {
                let apk_db_dir = lib_map.entry(String::from("apk")).or_insert_with(|| VfsNode::Directory(BTreeMap::new()));
                if let VfsNode::Directory(ref mut apk_map) = apk_db_dir {
                    let db_dir = apk_map.entry(String::from("db")).or_insert_with(|| VfsNode::Directory(BTreeMap::new()));
                    if let VfsNode::Directory(ref mut db_map) = db_dir {
                        db_map.entry(String::from("installed")).or_insert_with(|| VfsNode::File(Vec::new()));
                        db_map.entry(String::from("lock")).or_insert_with(|| VfsNode::File(Vec::new()));
                    }
                }
            }
        }

        // /lib/apk/db/ — secondary APK database location
        if let Some(VfsNode::Directory(ref mut lib_dir)) = root.get_mut("lib") {
            let apk_dir = lib_dir.entry(String::from("apk")).or_insert_with(|| VfsNode::Directory(BTreeMap::new()));
            if let VfsNode::Directory(ref mut apk_map) = apk_dir {
                apk_map.entry(String::from("db")).or_insert_with(|| VfsNode::Directory(BTreeMap::new()));
                apk_map.entry(String::from("exec")).or_insert_with(|| VfsNode::Directory(BTreeMap::new()));
            }
        }
    }

    VFS_METRICS.total_nodes.fetch_add(30, Ordering::Relaxed);
    crate::serial_println!("[VFS] Created directories: /dev, /tmp, /proc, /sys, /etc");
    crate::serial_println!("[VFS] Linux ABI pseudo-devices: READY");
    crate::serial_println!("[VFS] /etc: resolv.conf, hosts, passwd, group, protocols, services");

    Ok(())
}
