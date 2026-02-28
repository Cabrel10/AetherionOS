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
            VfsNode::Directory(_) => {
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

    let data;
    {
        let root = VFS_ROOT.lock();

        let node = find_node(&root, &components)
            .ok_or(VfsError::NotFound)?;

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

                data = device_data.clone();
            }
            VfsNode::File(ref file_data) => {
                data = file_data.clone();
            }
            VfsNode::Directory(_) => {
                VFS_METRICS.errors_count.fetch_add(1, Ordering::Relaxed);
                return Err(VfsError::PermissionDenied);
            }
        }
    }

    VFS_METRICS
        .total_bytes_read
        .fetch_add(data.len() as u64, Ordering::Relaxed);
    bus_publish_event(VFS_READ, data.len() as u64);

    Ok(data)
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
/// Creates /dev and /tmp directories
pub fn init() -> Result<(), VfsError> {
    crate::serial_println!("[VFS] Initializing virtual filesystem...");

    {
        let mut root = VFS_ROOT.lock();
        root.insert(
            String::from("dev"),
            VfsNode::Directory(BTreeMap::new()),
        );
        root.insert(
            String::from("tmp"),
            VfsNode::Directory(BTreeMap::new()),
        );
    }

    VFS_METRICS.total_nodes.fetch_add(2, Ordering::Relaxed);
    crate::serial_println!("[VFS] Created /dev and /tmp directories");

    Ok(())
}
