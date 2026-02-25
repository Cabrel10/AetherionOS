// fs/manifest.rs - Device Manifest (Capability-Based Security)
//
// Each mounted device has a manifest describing its capabilities.
// The VFS checks the manifest BEFORE allowing any operation.
// This is the foundation for ACHA Couche 5 (Verifier) integration.

use alloc::string::String;

/// Device capabilities - what operations are allowed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Device allows read operations
    Read,
    /// Device allows write operations
    Write,
    /// Device allows execute operations
    Execute,
    /// Device allows mount/unmount of sub-devices
    Mount,
}

/// Device type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    /// RAM-backed device (volatile)
    RamDisk,
    /// Block device (persistent)
    BlockDevice,
    /// Character device (stream)
    CharDevice,
    /// Virtual device (pseudo-filesystem)
    Virtual,
}

/// Device manifest - security descriptor for a mounted device
///
/// Every device in the VFS MUST have a manifest.
/// Operations are checked against capabilities before execution.
#[derive(Debug, Clone)]
pub struct DeviceManifest {
    /// Human-readable device name
    pub name: String,
    /// Device type
    pub device_type: DeviceType,
    /// Maximum capacity in bytes (0 = unlimited)
    pub capacity: usize,
    /// Allowed capabilities
    pub capabilities: [Option<Capability>; 4],
    /// Number of active capabilities
    pub cap_count: usize,
    /// Device is read-only (shortcut check)
    pub read_only: bool,
}

impl DeviceManifest {
    /// Create a new manifest for a RAM disk
    pub fn ram_disk(name: &str, capacity: usize, writable: bool) -> Self {
        let mut caps = [None; 4];
        let mut count = 0;

        caps[count] = Some(Capability::Read);
        count += 1;

        if writable {
            caps[count] = Some(Capability::Write);
            count += 1;
        }

        Self {
            name: String::from(name),
            device_type: DeviceType::RamDisk,
            capacity,
            capabilities: caps,
            cap_count: count,
            read_only: !writable,
        }
    }

    /// Create a read-only virtual device manifest
    pub fn virtual_readonly(name: &str) -> Self {
        let mut caps = [None; 4];
        caps[0] = Some(Capability::Read);

        Self {
            name: String::from(name),
            device_type: DeviceType::Virtual,
            capacity: 0,
            capabilities: caps,
            cap_count: 1,
            read_only: true,
        }
    }

    /// Check if device has a specific capability
    pub fn can(&self, cap: Capability) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, Some(existing) if *existing == cap))
    }

    /// Validate manifest integrity
    /// Returns true if manifest is internally consistent
    pub fn validate(&self) -> bool {
        // Rule 1: read_only devices must NOT have Write capability
        if self.read_only && self.can(Capability::Write) {
            return false;
        }

        // Rule 2: All devices must have at least Read capability
        if !self.can(Capability::Read) {
            return false;
        }

        // Rule 3: cap_count must match actual capabilities
        let actual_count = self.capabilities.iter().filter(|c| c.is_some()).count();
        if actual_count != self.cap_count {
            return false;
        }

        // Rule 4: capacity must be > 0 for RamDisk and BlockDevice
        if matches!(self.device_type, DeviceType::RamDisk | DeviceType::BlockDevice)
            && self.capacity == 0
        {
            return false;
        }

        true
    }
}
