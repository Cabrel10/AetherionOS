// kernel/src/fs/vfs_backend.rs — Trait-Based Multi-Backend VFS for AetherionOS
//
// Implements a unified filesystem interface supporting multiple backends:
//   - RamFs: in-memory filesystem (default, replaces old BTreeMap VFS)
//   - ProcFs: /proc virtual filesystem (process info, kernel stats)
//   - DevFs: /dev device filesystem (null, zero, urandom, ptmx, pts/*)
//   - SysFs: /sys kernel object filesystem (CPU info, memory, devices)
//
// Architecture:
//   Each mount point maps to a boxed FsBackend trait object.
//   File operations route through the mount table to the correct backend.
//   This enables `mount -t proc proc /proc` style operations.
//
// SAFETY: All mutable statics are behind spin::Mutex.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;

// ═══════════════════════════════════════════════════════════
// VFS Backend Trait
// ═══════════════════════════════════════════════════════════

/// Metadata for a filesystem entry
#[derive(Debug, Clone)]
pub struct FsEntry {
    /// Entry type
    pub entry_type: FsEntryType,
    /// File size in bytes (0 for directories/special)
    pub size: u64,
    /// Unix mode (permissions + type bits)
    pub mode: u32,
    /// Device number (for device nodes)
    pub rdev: u64,
    /// Owner UID
    pub uid: u32,
    /// Owner GID
    pub gid: u32,
    /// Number of hard links
    pub nlink: u64,
}

/// File entry type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsEntryType {
    RegularFile,
    Directory,
    CharDevice,
    BlockDevice,
    Symlink,
    Pipe,
    Socket,
}

/// Trait for filesystem backends
pub trait FsBackend: Send + Sync {
    /// Get the filesystem type name (e.g., "ramfs", "procfs")
    fn fs_type(&self) -> &str;

    /// Read a file's contents. `path` is relative to mount point.
    fn read(&self, path: &str) -> Result<Vec<u8>, FsError>;

    /// Write data to a file. Returns bytes written.
    fn write(&self, path: &str, data: &[u8]) -> Result<usize, FsError>;

    /// Get metadata for a path
    fn stat(&self, path: &str) -> Result<FsEntry, FsError>;

    /// List directory entries (names only)
    fn readdir(&self, path: &str) -> Result<Vec<String>, FsError>;

    /// Create a directory
    fn mkdir(&self, path: &str, mode: u32) -> Result<(), FsError>;

    /// Remove a file
    fn unlink(&self, path: &str) -> Result<(), FsError>;

    /// Create a symbolic link
    fn symlink(&self, target: &str, linkpath: &str) -> Result<(), FsError>;

    /// Read a symbolic link target
    fn readlink(&self, path: &str) -> Result<String, FsError>;

    /// Check if a path exists
    fn exists(&self, path: &str) -> bool {
        self.stat(path).is_ok()
    }
}

/// Filesystem errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    PermissionDenied,
    ReadOnly,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    NoSpace,
    InvalidPath,
    IoError,
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::NotFound => write!(f, "not found"),
            Self::PermissionDenied => write!(f, "permission denied"),
            Self::ReadOnly => write!(f, "read-only filesystem"),
            Self::AlreadyExists => write!(f, "already exists"),
            Self::NotADirectory => write!(f, "not a directory"),
            Self::IsADirectory => write!(f, "is a directory"),
            Self::NoSpace => write!(f, "no space left"),
            Self::InvalidPath => write!(f, "invalid path"),
            Self::IoError => write!(f, "I/O error"),
        }
    }
}

// ═══════════════════════════════════════════════════════════
// Mount Table
// ═══════════════════════════════════════════════════════════

struct MountEntry {
    mount_point: String,
    backend: Box<dyn FsBackend>,
}

lazy_static! {
    static ref MOUNT_TABLE: Mutex<Vec<MountEntry>> = Mutex::new(Vec::new());
}

/// Mount a filesystem backend at a given path.
/// Longer mount paths take precedence (e.g., /proc/self over /proc).
pub fn mount(mount_point: &str, backend: Box<dyn FsBackend>) {
    let fs_type = alloc::string::String::from(backend.fs_type());
    crate::serial_println!(
        "[VFS-BACKEND] Mounting {} at {}", fs_type, mount_point
    );
    let mut table = MOUNT_TABLE.lock();
    // Check for duplicate mount
    for entry in table.iter() {
        if entry.mount_point == mount_point {
            crate::serial_println!(
                "[VFS-BACKEND] WARNING: replacing existing mount at {}", mount_point
            );
            break;
        }
    }
    // Remove existing mount at same point
    table.retain(|e| e.mount_point != mount_point);
    table.push(MountEntry {
        mount_point: String::from(mount_point),
        backend,
    });
    // Sort by mount_point length descending for longest-prefix matching
    table.sort_by(|a, b| b.mount_point.len().cmp(&a.mount_point.len()));
}

/// Unmount the filesystem at a given path.
pub fn umount(mount_point: &str) -> bool {
    let mut table = MOUNT_TABLE.lock();
    let before = table.len();
    table.retain(|e| e.mount_point != mount_point);
    table.len() < before
}

/// Find the backend for a path, returning (backend_ref, relative_path).
fn find_backend<'a, 'b>(
    table: &'a [MountEntry],
    path: &'b str,
) -> Option<(&'a dyn FsBackend, &'b str)> {
    for entry in table.iter() {
        if path == entry.mount_point {
            return Some((&*entry.backend, "/"));
        }
        // Special case: root mount at "/" matches all absolute paths
        if entry.mount_point == "/" {
            return Some((&*entry.backend, path));
        }
        let prefix = alloc::format!("{}/", entry.mount_point);
        if path.starts_with(&prefix) {
            let rel = &path[entry.mount_point.len()..];
            return Some((&*entry.backend, rel));
        }
    }
    None
}

/// Read a file via the backend mount table.
pub fn backend_read(path: &str) -> Result<Vec<u8>, FsError> {
    let table = MOUNT_TABLE.lock();
    if let Some((backend, rel)) = find_backend(&table, path) {
        backend.read(rel)
    } else {
        Err(FsError::NotFound)
    }
}

/// Stat a file via the backend mount table.
pub fn backend_stat(path: &str) -> Result<FsEntry, FsError> {
    let table = MOUNT_TABLE.lock();
    if let Some((backend, rel)) = find_backend(&table, path) {
        backend.stat(rel)
    } else {
        Err(FsError::NotFound)
    }
}

/// Readdir via the backend mount table.
pub fn backend_readdir(path: &str) -> Result<Vec<String>, FsError> {
    let table = MOUNT_TABLE.lock();
    if let Some((backend, rel)) = find_backend(&table, path) {
        backend.readdir(rel)
    } else {
        Err(FsError::NotFound)
    }
}

// ═══════════════════════════════════════════════════════════
// ProcFs — /proc Virtual Filesystem
// ═══════════════════════════════════════════════════════════

pub struct ProcFs;

impl FsBackend for ProcFs {
    fn fs_type(&self) -> &str { "proc" }

    fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let content = match path {
            "/meminfo" | "meminfo" => generate_meminfo(),
            "/cpuinfo" | "cpuinfo" => generate_cpuinfo(),
            "/version" | "version" => generate_version(),
            "/uptime" | "uptime" => alloc::format!("100.00 80.00\n"),
            "/loadavg" | "loadavg" => alloc::format!("0.01 0.05 0.10 1/50 100\n"),
            "/stat" | "stat" => generate_proc_stat(),
            "/mounts" | "mounts" => generate_mounts(),
            "/filesystems" | "filesystems" => alloc::format!("nodev\tproc\nnodev\tsysfs\nnodev\tdevtmpfs\n\text4\n\tfat32\n"),
            "/cmdline" | "cmdline" => alloc::format!("console=ttyS0 root=/dev/vda\n"),
            p if p.starts_with("/self/") => generate_proc_self(&p[6..]),
            p if p.starts_with("self/") => generate_proc_self(&p[5..]),
            _ => return Err(FsError::NotFound),
        };
        Ok(content.into_bytes())
    }

    fn write(&self, _path: &str, _data: &[u8]) -> Result<usize, FsError> {
        Err(FsError::ReadOnly)
    }

    fn stat(&self, path: &str) -> Result<FsEntry, FsError> {
        let p = path.trim_start_matches('/');
        if p.is_empty() || p == "." {
            return Ok(FsEntry {
                entry_type: FsEntryType::Directory,
                size: 0, mode: 0o40555, rdev: 0, uid: 0, gid: 0, nlink: 2,
            });
        }
        match p {
            "meminfo" | "cpuinfo" | "version" | "uptime" | "loadavg" | "stat"
            | "mounts" | "filesystems" | "cmdline" => Ok(FsEntry {
                entry_type: FsEntryType::RegularFile,
                size: 4096, mode: 0o100444, rdev: 0, uid: 0, gid: 0, nlink: 1,
            }),
            "self" | "self/" => Ok(FsEntry {
                entry_type: FsEntryType::Directory,
                size: 0, mode: 0o40555, rdev: 0, uid: 0, gid: 0, nlink: 2,
            }),
            _ if p.starts_with("self/") => Ok(FsEntry {
                entry_type: FsEntryType::RegularFile,
                size: 4096, mode: 0o100444, rdev: 0, uid: 0, gid: 0, nlink: 1,
            }),
            _ => Err(FsError::NotFound),
        }
    }

    fn readdir(&self, path: &str) -> Result<Vec<String>, FsError> {
        let p = path.trim_start_matches('/');
        if p.is_empty() || p == "." {
            return Ok(alloc::vec![
                String::from("meminfo"), String::from("cpuinfo"),
                String::from("version"), String::from("uptime"),
                String::from("loadavg"), String::from("stat"),
                String::from("mounts"), String::from("filesystems"),
                String::from("cmdline"), String::from("self"),
            ]);
        }
        if p == "self" {
            return Ok(alloc::vec![
                String::from("status"), String::from("maps"),
                String::from("cmdline"), String::from("exe"),
                String::from("fd"), String::from("environ"),
            ]);
        }
        Err(FsError::NotADirectory)
    }

    fn mkdir(&self, _path: &str, _mode: u32) -> Result<(), FsError> { Err(FsError::ReadOnly) }
    fn unlink(&self, _path: &str) -> Result<(), FsError> { Err(FsError::ReadOnly) }
    fn symlink(&self, _target: &str, _linkpath: &str) -> Result<(), FsError> { Err(FsError::ReadOnly) }
    fn readlink(&self, path: &str) -> Result<String, FsError> {
        let p = path.trim_start_matches('/');
        if p == "self/exe" {
            Ok(String::from("/bin/busybox"))
        } else {
            Err(FsError::NotFound)
        }
    }
}

// ProcFs content generators
fn generate_meminfo() -> String {
    alloc::format!(
        "MemTotal:        1048576 kB\nMemFree:          524288 kB\nMemAvailable:     786432 kB\n\
         Buffers:           32768 kB\nCached:           131072 kB\nSwapTotal:             0 kB\n\
         SwapFree:              0 kB\nDirty:                 0 kB\n"
    )
}

fn generate_cpuinfo() -> String {
    alloc::format!(
        "processor\t: 0\nvendor_id\t: AetherionOS\ncpu family\t: 6\n\
         model\t\t: 142\nmodel name\t: AetherionOS Virtual CPU\nstepping\t: 10\n\
         cpu MHz\t\t: 2400.000\ncache size\t: 8192 KB\nphysical id\t: 0\n\
         siblings\t: 2\ncpu cores\t: 2\nflags\t\t: fpu vme de pse tsc msr pae mce \
         cx8 apic sep mtrr pge mca cmov pat pse36 clflush mmx fxsr sse sse2 \
         ss ht syscall nx rdtscp lm avx avx2\nbogomips\t: 4800.00\n\n"
    )
}

fn generate_version() -> String {
    alloc::format!(
        "Linux version 6.18.0-aetherion (builder@aetherionos) \
         (gcc (Alpine 13.2.1) 13.2.1, GNU ld (GNU Binutils) 2.41) \
         #1 SMP PREEMPT_DYNAMIC AetherionOS\n"
    )
}

fn generate_proc_stat() -> String {
    alloc::format!(
        "cpu  100 0 50 1000 0 0 0 0 0 0\ncpu0 50 0 25 500 0 0 0 0 0 0\n\
         cpu1 50 0 25 500 0 0 0 0 0 0\nintr 10000 0\nctxt 50000\n\
         btime 1700000000\nprocesses 100\nprocs_running 1\nprocs_blocked 0\n"
    )
}

fn generate_mounts() -> String {
    let mut s = String::from("rootfs / ramfs rw,relatime 0 0\n");
    s.push_str("proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n");
    s.push_str("sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0\n");
    s.push_str("devtmpfs /dev devtmpfs rw,nosuid,relatime 0 0\n");
    s.push_str("/dev/vda /disk ext4 rw,relatime 0 0\n");
    s
}

fn generate_proc_self(subpath: &str) -> String {
    let pid = crate::scheduler::current_pid();
    match subpath {
        "status" => alloc::format!(
            "Name:\tprocess\nUmask:\t0022\nState:\tR (running)\n\
             Tgid:\t{p}\nNgid:\t0\nPid:\t{p}\nPPid:\t1\nTracerPid:\t0\n\
             Uid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\n\
             VmPeak:\t    8192 kB\nVmSize:\t    4096 kB\n\
             VmRSS:\t     2048 kB\nVmData:\t    1024 kB\n\
             VmStk:\t      512 kB\nVmExe:\t      512 kB\n\
             Threads:\t1\n",
            p = pid
        ),
        "maps" => {
            // Generate a realistic /proc/self/maps showing key memory regions
            // musl reads this to discover address space layout
            let mut maps = String::new();
            // Main binary (PIE at 0x400000)
            maps.push_str("00400000-00600000 r-xp 00000000 00:01 1  /bin/busybox\n");
            maps.push_str("00600000-00700000 rw-p 00200000 00:01 1  /bin/busybox\n");
            // Heap (brk region)
            maps.push_str("30000000000-30001000000 rw-p 00000000 00:00 0  [heap]\n");
            // mmap region
            maps.push_str("40000000000-40001000000 rw-p 00000000 00:00 0\n");
            // TLS region
            maps.push_str("7fb000000000-7fb000010000 rw-p 00000000 00:00 0\n");
            // Interpreter (ld-musl)
            maps.push_str("7fc000000000-7fc000100000 r-xp 00000000 00:01 2  /lib/ld-musl-x86_64.so.1\n");
            maps.push_str("7fc000100000-7fc000120000 rw-p 00100000 00:01 2  /lib/ld-musl-x86_64.so.1\n");
            // Stack
            maps.push_str("7fffffffe000-7ffffffff000 rw-p 00000000 00:00 0  [stack]\n");
            // VDSO/VVAR (fake but expected by musl)
            maps.push_str("ffffffffff600000-ffffffffff601000 r-xp 00000000 00:00 0  [vsyscall]\n");
            maps
        }
        "cmdline" => alloc::format!("busybox\x00sh\x00"),
        "environ" => alloc::format!("PATH=/usr/bin:/bin\x00HOME=/root\x00TERM=linux\x00"),
        "exe" => String::from("/bin/busybox"),
        _ => alloc::format!(""),
    }
}

// ═══════════════════════════════════════════════════════════
// DevFs — /dev Device Filesystem
// ═══════════════════════════════════════════════════════════

pub struct DevFs;

impl FsBackend for DevFs {
    fn fs_type(&self) -> &str { "devtmpfs" }

    fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let p = path.trim_start_matches('/');
        match p {
            "null" => Ok(Vec::new()), // EOF
            "zero" => Ok(alloc::vec![0u8; 4096]),
            "urandom" | "random" => {
                let mut buf = alloc::vec![0u8; 256];
                let seed = unsafe {
                    let tsc: u64;
                    core::arch::asm!("rdtsc", "shl rdx, 32", "or rax, rdx",
                        out("rax") tsc, out("rdx") _, options(nomem, nostack));
                    tsc
                };
                let mut s = seed;
                for b in buf.iter_mut() {
                    s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    *b = (s >> 33) as u8;
                }
                Ok(buf)
            }
            _ => Err(FsError::NotFound),
        }
    }

    fn write(&self, path: &str, data: &[u8]) -> Result<usize, FsError> {
        let p = path.trim_start_matches('/');
        match p {
            "null" => Ok(data.len()), // discard
            _ => Err(FsError::PermissionDenied),
        }
    }

    fn stat(&self, path: &str) -> Result<FsEntry, FsError> {
        let p = path.trim_start_matches('/');
        if p.is_empty() || p == "." {
            return Ok(FsEntry {
                entry_type: FsEntryType::Directory,
                size: 0, mode: 0o40755, rdev: 0, uid: 0, gid: 0, nlink: 10,
            });
        }
        match p {
            "null" => Ok(FsEntry {
                entry_type: FsEntryType::CharDevice,
                size: 0, mode: 0o20666, rdev: 0x0103, uid: 0, gid: 0, nlink: 1,
            }),
            "zero" => Ok(FsEntry {
                entry_type: FsEntryType::CharDevice,
                size: 0, mode: 0o20666, rdev: 0x0105, uid: 0, gid: 0, nlink: 1,
            }),
            "urandom" | "random" => Ok(FsEntry {
                entry_type: FsEntryType::CharDevice,
                size: 0, mode: 0o20666, rdev: 0x0109, uid: 0, gid: 0, nlink: 1,
            }),
            "tty" | "console" => Ok(FsEntry {
                entry_type: FsEntryType::CharDevice,
                size: 0, mode: 0o20620, rdev: 0x0500, uid: 0, gid: 0, nlink: 1,
            }),
            "ptmx" => Ok(FsEntry {
                entry_type: FsEntryType::CharDevice,
                size: 0, mode: 0o20666, rdev: 0x0502, uid: 0, gid: 0, nlink: 1,
            }),
            "pts" | "pts/" => Ok(FsEntry {
                entry_type: FsEntryType::Directory,
                size: 0, mode: 0o40755, rdev: 0, uid: 0, gid: 0, nlink: 2,
            }),
            "stdin" => Ok(FsEntry {
                entry_type: FsEntryType::Symlink,
                size: 0, mode: 0o120777, rdev: 0, uid: 0, gid: 0, nlink: 1,
            }),
            "stdout" => Ok(FsEntry {
                entry_type: FsEntryType::Symlink,
                size: 0, mode: 0o120777, rdev: 0, uid: 0, gid: 0, nlink: 1,
            }),
            "stderr" => Ok(FsEntry {
                entry_type: FsEntryType::Symlink,
                size: 0, mode: 0o120777, rdev: 0, uid: 0, gid: 0, nlink: 1,
            }),
            "fd" | "fd/" => Ok(FsEntry {
                entry_type: FsEntryType::Directory,
                size: 0, mode: 0o40555, rdev: 0, uid: 0, gid: 0, nlink: 2,
            }),
            _ if p.starts_with("pts/") => {
                // /dev/pts/N — check if PTY exists
                if let Ok(id) = p[4..].parse::<u32>() {
                    if crate::drivers::pty::pty_exists(id) {
                        return Ok(FsEntry {
                            entry_type: FsEntryType::CharDevice,
                            size: 0, mode: 0o20620,
                            rdev: (136u64 << 8) | id as u64,
                            uid: 0, gid: 0, nlink: 1,
                        });
                    }
                }
                Err(FsError::NotFound)
            }
            _ => Err(FsError::NotFound),
        }
    }

    fn readdir(&self, path: &str) -> Result<Vec<String>, FsError> {
        let p = path.trim_start_matches('/');
        if p.is_empty() || p == "." {
            return Ok(alloc::vec![
                String::from("null"), String::from("zero"),
                String::from("urandom"), String::from("random"),
                String::from("tty"), String::from("console"),
                String::from("ptmx"), String::from("pts"),
                String::from("stdin"), String::from("stdout"),
                String::from("stderr"), String::from("fd"),
            ]);
        }
        if p == "pts" {
            // List active PTY slaves
            let count = crate::drivers::pty::pty_count();
            let mut entries = Vec::new();
            for i in 0..count as u32 {
                if crate::drivers::pty::pty_exists(i) {
                    entries.push(alloc::format!("{}", i));
                }
            }
            return Ok(entries);
        }
        Err(FsError::NotADirectory)
    }

    fn mkdir(&self, _path: &str, _mode: u32) -> Result<(), FsError> { Err(FsError::ReadOnly) }
    fn unlink(&self, _path: &str) -> Result<(), FsError> { Err(FsError::ReadOnly) }
    fn symlink(&self, _target: &str, _linkpath: &str) -> Result<(), FsError> { Err(FsError::ReadOnly) }
    fn readlink(&self, path: &str) -> Result<String, FsError> {
        let p = path.trim_start_matches('/');
        match p {
            "stdin" => Ok(String::from("/proc/self/fd/0")),
            "stdout" => Ok(String::from("/proc/self/fd/1")),
            "stderr" => Ok(String::from("/proc/self/fd/2")),
            _ => Err(FsError::NotFound),
        }
    }
}

// ═══════════════════════════════════════════════════════════
// SysFs — /sys Kernel Object Filesystem
// ═══════════════════════════════════════════════════════════

pub struct SysFs;

impl FsBackend for SysFs {
    fn fs_type(&self) -> &str { "sysfs" }

    fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let p = path.trim_start_matches('/');
        let content = match p {
            "kernel/hostname" => alloc::format!("aetherionos\n"),
            "kernel/ostype" => alloc::format!("Linux\n"),
            "kernel/osrelease" => alloc::format!("6.18.0-aetherion\n"),
            "kernel/version" => alloc::format!("#1 SMP PREEMPT_DYNAMIC AetherionOS\n"),
            "devices/system/cpu/online" => alloc::format!("0-1\n"),
            "devices/system/cpu/possible" => alloc::format!("0-1\n"),
            "devices/system/cpu/present" => alloc::format!("0-1\n"),
            "devices/system/cpu/cpu0/topology/core_id" => alloc::format!("0\n"),
            "devices/system/cpu/cpu0/cpufreq/scaling_cur_freq" => alloc::format!("2400000\n"),
            "devices/system/memory/block_size_bytes" => alloc::format!("8000000\n"),
            _ => return Err(FsError::NotFound),
        };
        Ok(content.into_bytes())
    }

    fn write(&self, _path: &str, _data: &[u8]) -> Result<usize, FsError> { Err(FsError::ReadOnly) }

    fn stat(&self, path: &str) -> Result<FsEntry, FsError> {
        let p = path.trim_start_matches('/');
        if p.is_empty() || p == "." {
            return Ok(FsEntry {
                entry_type: FsEntryType::Directory,
                size: 0, mode: 0o40555, rdev: 0, uid: 0, gid: 0, nlink: 5,
            });
        }
        // Check if it's a known directory prefix
        for prefix in &["kernel", "devices", "devices/system", "devices/system/cpu",
                        "devices/system/cpu/cpu0", "devices/system/cpu/cpu0/topology",
                        "devices/system/cpu/cpu0/cpufreq", "devices/system/memory"] {
            if p == *prefix {
                return Ok(FsEntry {
                    entry_type: FsEntryType::Directory,
                    size: 0, mode: 0o40555, rdev: 0, uid: 0, gid: 0, nlink: 2,
                });
            }
        }
        // Otherwise assume it's a readable file
        if self.read(path).is_ok() {
            return Ok(FsEntry {
                entry_type: FsEntryType::RegularFile,
                size: 64, mode: 0o100444, rdev: 0, uid: 0, gid: 0, nlink: 1,
            });
        }
        Err(FsError::NotFound)
    }

    fn readdir(&self, path: &str) -> Result<Vec<String>, FsError> {
        let p = path.trim_start_matches('/');
        match p {
            "" | "." => Ok(alloc::vec![
                String::from("kernel"), String::from("devices"),
            ]),
            "kernel" => Ok(alloc::vec![
                String::from("hostname"), String::from("ostype"),
                String::from("osrelease"), String::from("version"),
            ]),
            "devices" => Ok(alloc::vec![String::from("system")]),
            "devices/system" => Ok(alloc::vec![
                String::from("cpu"), String::from("memory"),
            ]),
            "devices/system/cpu" => Ok(alloc::vec![
                String::from("online"), String::from("possible"),
                String::from("present"), String::from("cpu0"),
            ]),
            _ => Err(FsError::NotADirectory),
        }
    }

    fn mkdir(&self, _path: &str, _mode: u32) -> Result<(), FsError> { Err(FsError::ReadOnly) }
    fn unlink(&self, _path: &str) -> Result<(), FsError> { Err(FsError::ReadOnly) }
    fn symlink(&self, _target: &str, _linkpath: &str) -> Result<(), FsError> { Err(FsError::ReadOnly) }
    fn readlink(&self, _path: &str) -> Result<String, FsError> { Err(FsError::NotFound) }
}

// ═══════════════════════════════════════════════════════════
// Initialization — mount standard virtual filesystems
// ═══════════════════════════════════════════════════════════

/// Initialize the standard virtual filesystem mounts.
/// Called during kernel boot after heap is available.
pub fn init_virtual_filesystems() {
    crate::serial_println!("[VFS-BACKEND] Initializing virtual filesystems...");
    mount("/proc", Box::new(ProcFs));
    mount("/dev", Box::new(DevFs));
    mount("/sys", Box::new(SysFs));
    crate::serial_println!("[VFS-BACKEND] Mounted: /proc (procfs), /dev (devtmpfs), /sys (sysfs)");
}

/// Mount the ext2 filesystem at root `/` so all Alpine rootfs files are
/// transparently accessible through the VFS multi-backend system.
/// Must be called AFTER ext2::init() succeeds.
pub fn mount_ext2_root() {
    if crate::fs::ext2::is_mounted() {
        mount("/", Box::new(Ext2Backend));
        crate::serial_println!("[VFS-BACKEND] Mounted ext2 at / (Alpine rootfs via VirtIO-BLK)");
    }
}

// ═══════════════════════════════════════════════════════════
// Ext2Backend — persistent Alpine rootfs via VirtIO-Block
// ═══════════════════════════════════════════════════════════

/// Filesystem backend that delegates all operations to the ext2 driver.
/// Provides transparent access to the Alpine rootfs from the VFS layer.
struct Ext2Backend;

impl FsBackend for Ext2Backend {
    fn fs_type(&self) -> &str { "ext2" }

    fn read(&self, path: &str) -> Result<Vec<u8>, FsError> {
        crate::fs::ext2::read_file_path(path)
            .ok_or(FsError::NotFound)
    }

    fn write(&self, path: &str, data: &[u8]) -> Result<usize, FsError> {
        if crate::fs::ext2::write_file_path(path, data).is_some() {
            Ok(data.len())
        } else {
            Err(FsError::IoError)
        }
    }

    fn stat(&self, path: &str) -> Result<FsEntry, FsError> {
        let ino = crate::fs::ext2::lookup_path(path)
            .ok_or(FsError::NotFound)?;
        let inode = crate::fs::ext2::read_inode(ino)
            .ok_or(FsError::IoError)?;
        let entry_type = if (inode.i_mode & 0xF000) == 0x4000 {
            FsEntryType::Directory
        } else if (inode.i_mode & 0xF000) == 0xA000 {
            FsEntryType::Symlink
        } else {
            FsEntryType::RegularFile
        };
        Ok(FsEntry {
            entry_type,
            size: inode.i_size as u64,
            mode: inode.i_mode as u32,
            rdev: 0,
            uid: inode.i_uid as u32,
            gid: inode.i_gid as u32,
            nlink: inode.i_links_count as u64,
        })
    }

    fn readdir(&self, path: &str) -> Result<Vec<String>, FsError> {
        if let Some(entries) = crate::fs::ext2::list_dir(path) {
            Ok(entries.into_iter()
                .filter(|(n, _, _)| n != "." && n != "..")
                .map(|(n, _, _)| n)
                .collect())
        } else {
            Err(FsError::NotFound)
        }
    }

    fn mkdir(&self, path: &str, _mode: u32) -> Result<(), FsError> {
        // Split path into parent directory + name
        let path = path.trim_start_matches('/');
        if let Some(pos) = path.rfind('/') {
            let parent = &path[..pos];
            let name = &path[pos+1..];
            let parent_path = alloc::format!("/{}", parent);
            if crate::fs::ext2::create_dir(&parent_path, name, 0o755).is_some() {
                Ok(())
            } else {
                Err(FsError::IoError)
            }
        } else {
            // Creating in root
            if crate::fs::ext2::create_dir("/", path, 0o755).is_some() {
                Ok(())
            } else {
                Err(FsError::IoError)
            }
        }
    }

    fn unlink(&self, _path: &str) -> Result<(), FsError> {
        Err(FsError::ReadOnly) // TODO: implement ext2 unlink
    }

    fn symlink(&self, target: &str, linkpath: &str) -> Result<(), FsError> {
        let linkpath = linkpath.trim_start_matches('/');
        if let Some(pos) = linkpath.rfind('/') {
            let parent = &linkpath[..pos];
            let name = &linkpath[pos+1..];
            let parent_path = alloc::format!("/{}", parent);
            if crate::fs::ext2::create_symlink(&parent_path, name, target).is_some() {
                Ok(())
            } else {
                Err(FsError::IoError)
            }
        } else {
            if crate::fs::ext2::create_symlink("/", linkpath, target).is_some() {
                Ok(())
            } else {
                Err(FsError::IoError)
            }
        }
    }

    fn readlink(&self, path: &str) -> Result<String, FsError> {
        let ino = crate::fs::ext2::lookup_path(path)
            .ok_or(FsError::NotFound)?;
        crate::fs::ext2::read_symlink(ino)
            .ok_or(FsError::IoError)
    }
}
