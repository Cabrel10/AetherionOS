// fs/mod.rs - Couche 4: Virtual Filesystem (VFS)
// ACHA-OS Filesystem Abstraction Layer
//
// Architecture:
//   - BTreeMap-based node tree (no_std compatible via alloc)
//   - Device manifests with capability-based security
//   - Cognitive Bus integration (event-driven I/O)
//   - Path traversal protection
//   - Overflow protection
//   - Metrics collection and reporting

pub mod vfs;
pub mod manifest;
