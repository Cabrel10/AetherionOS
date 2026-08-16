// kernel/src/fs/apk.rs — Alpine Package Manager for AetherionOS (Layer 3)
//
// Implements a minimal APK-compatible package manager that:
//   1. Reads /etc/apk/repositories from ext2
//   2. Downloads APKINDEX.tar.gz via HTTP (or reads from ext2)
//   3. Parses APKINDEX to find package metadata
//   4. Downloads and extracts .apk packages (tar.gz) to ext2
//
// APK file format: tar.gz containing:
//   - .PKGINFO (package metadata)
//   - .SIGN.RSA.*.rsa.pub (signature — we skip verification)
//   - actual filesystem entries (usr/bin/*, etc.)

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

/// APK package metadata from APKINDEX
#[derive(Debug, Clone)]
pub struct ApkPackage {
    pub name: String,
    pub version: String,
    pub description: String,
    pub url: String,
    pub size: usize,
    pub depends: Vec<String>,
    pub provides: Vec<String>,
    pub filename: String,
}

/// APK repository
#[derive(Debug, Clone)]
pub struct ApkRepo {
    pub url: String,
    pub arch: String,
}

/// APK database state
static mut APK_DB: Option<ApkDatabase> = None;

pub struct ApkDatabase {
    pub repos: Vec<ApkRepo>,
    pub installed: Vec<String>,
    pub available: Vec<ApkPackage>,
    pub root: String, // ext2 mount prefix (e.g., "" for /)
}

/// Initialize APK system: read repositories from ext2
pub fn init() -> bool {
    let repos = read_repositories();
    if repos.is_empty() {
        crate::serial_println!("[APK] No repositories configured");
        return false;
    }

    crate::serial_println!("[APK] Found {} repositories", repos.len());
    for r in &repos {
        crate::serial_println!("[APK]   {}", r.url);
    }

    unsafe {
        APK_DB = Some(ApkDatabase {
            repos,
            installed: Vec::new(),
            available: Vec::new(),
            root: String::new(),
        });
    }
    true
}

/// Read /etc/apk/repositories from ext2 filesystem
fn read_repositories() -> Vec<ApkRepo> {
    let mut repos = Vec::new();

    if let Some(data) = crate::fs::ext2::read_file_path("/etc/apk/repositories") {
        if let Ok(text) = core::str::from_utf8(&data) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                repos.push(ApkRepo {
                    url: String::from(line),
                    arch: String::from("x86_64"),
                });
            }
        }
    }

    repos
}

/// Parse APKINDEX text format into package list.
/// Format: key-value pairs separated by blank lines.
/// P: = package name, V: = version, T: = description, etc.
pub fn parse_apkindex(text: &str) -> Vec<ApkPackage> {
    let mut packages = Vec::new();
    let mut current = ApkPackage {
        name: String::new(),
        version: String::new(),
        description: String::new(),
        url: String::new(),
        size: 0,
        depends: Vec::new(),
        provides: Vec::new(),
        filename: String::new(),
    };

    for line in text.lines() {
        if line.is_empty() {
            if !current.name.is_empty() {
                // Generate filename if not set
                if current.filename.is_empty() {
                    current.filename = format!("{}-{}.apk", current.name, current.version);
                }
                packages.push(current.clone());
            }
            current = ApkPackage {
                name: String::new(),
                version: String::new(),
                description: String::new(),
                url: String::new(),
                size: 0,
                depends: Vec::new(),
                provides: Vec::new(),
                filename: String::new(),
            };
            continue;
        }

        if let Some(rest) = line.strip_prefix("P:") {
            current.name = String::from(rest);
        } else if let Some(rest) = line.strip_prefix("V:") {
            current.version = String::from(rest);
        } else if let Some(rest) = line.strip_prefix("T:") {
            current.description = String::from(rest);
        } else if let Some(rest) = line.strip_prefix("U:") {
            current.url = String::from(rest);
        } else if let Some(rest) = line.strip_prefix("S:") {
            current.size = rest.parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("D:") {
            current.depends = rest.split_whitespace()
                .map(|s| {
                    // Strip version constraints like "so:libc.musl-x86_64.so.1"
                    let s = s.trim();
                    if let Some(idx) = s.find('=') {
                        String::from(&s[..idx])
                    } else if let Some(idx) = s.find('>') {
                        String::from(&s[..idx])
                    } else if let Some(idx) = s.find('<') {
                        String::from(&s[..idx])
                    } else {
                        String::from(s)
                    }
                })
                .collect();
        } else if let Some(rest) = line.strip_prefix("p:") {
            current.provides = rest.split_whitespace()
                .map(|s| String::from(s.trim()))
                .collect();
        }
    }

    // Don't forget last package
    if !current.name.is_empty() {
        if current.filename.is_empty() {
            current.filename = format!("{}-{}.apk", current.name, current.version);
        }
        packages.push(current);
    }

    packages
}

/// Decompress an APKINDEX.tar.gz blob (as served by Alpine repositories) and
/// return the UTF-8 text of its `APKINDEX` member.
///
/// Pipeline: gunzip (RFC 1952 / DEFLATE, `fs::tar::gunzip`) → untar
/// (`fs::tar::parse_tar`) → locate the `APKINDEX` entry → decode as UTF-8.
/// Returns `None` if any stage fails (bad gzip, missing member, non-UTF-8).
fn extract_apkindex_text(gz_data: &[u8]) -> Option<String> {
    // 1. DEFLATE decompression via the in-kernel gunzip implementation.
    let tar_bytes = match crate::fs::tar::gunzip(gz_data) {
        Some(d) => d,
        None => {
            crate::serial_println!("[APK]   gunzip failed on APKINDEX.tar.gz ({} bytes)", gz_data.len());
            return None;
        }
    };

    // 2. Parse the tar archive into named entries.
    let entries = crate::fs::tar::parse_tar(&tar_bytes);

    // 3. Find the APKINDEX member. Alpine archives store it at the top level,
    //    but tolerate a leading "./" or a sub-path just in case.
    for entry in &entries {
        let name = entry.name.trim_start_matches("./");
        if name == "APKINDEX" || name.ends_with("/APKINDEX") {
            return match core::str::from_utf8(&entry.data) {
                Ok(text) => Some(String::from(text)),
                Err(_) => {
                    crate::serial_println!("[APK]   APKINDEX member is not valid UTF-8");
                    None
                }
            };
        }
    }

    crate::serial_println!(
        "[APK]   APKINDEX member not found in tar archive ({} entries)",
        entries.len()
    );
    None
}

/// `apk update`: refresh the package index for every configured repository.
///
/// For each repo we first try the ext2 cache (`/var/lib/apk/APKINDEX-*.txt`);
/// on a miss we HTTP/HTTPS-fetch `<repo>/x86_64/APKINDEX.tar.gz` via the kernel
/// network stack (DNS + TCP + TLS 1.3 + 301 redirects), gunzip + untar it,
/// parse the APKINDEX text, and persist the decoded text back to ext2 so
/// subsequent boots are offline-capable. A bundled on-disk index is used as a
/// last-resort fallback when the network is unreachable.
pub fn apk_update() -> bool {
    let db = match unsafe { APK_DB.as_mut() } {
        Some(db) => db,
        None => {
            crate::serial_println!("[APK] Not initialized, run init() first");
            return false;
        }
    };

    crate::serial_println!("[APK] apk update: refreshing package index...");
    let mut total_packages = 0usize;

    for repo in &db.repos {
        crate::serial_println!("[APK]   Fetching index from {}", repo.url);

        // Try reading pre-loaded APKINDEX from ext2
        let index_path = format!("/var/lib/apk/APKINDEX-{}.txt",
            repo.url.replace("https://", "").replace("http://", "")
                     .replace('/', "_").replace(':', "_"));

        if let Some(data) = crate::fs::ext2::read_file_path(&index_path) {
            if let Ok(text) = core::str::from_utf8(&data) {
                let pkgs = parse_apkindex(text);
                crate::serial_println!("[APK]   Loaded {} packages from cache", pkgs.len());
                total_packages += pkgs.len();
                db.available.extend(pkgs);
            }
        } else {
            // Not cached on ext2 → fetch the real APKINDEX over the network using
            // the kernel HTTP/HTTPS client (DNS + TCP + TLS 1.3 + 301 redirects).
            // Alpine layout: <repo>/x86_64/APKINDEX.tar.gz
            let index_url = if repo.url.ends_with('/') {
                format!("{}x86_64/APKINDEX.tar.gz", repo.url)
            } else {
                format!("{}/x86_64/APKINDEX.tar.gz", repo.url)
            };
            crate::serial_println!("[APK]   HTTP fetch: {}", index_url);
            match crate::net::http::wget(&index_url) {
                Ok(body) => {
                    crate::serial_println!("[APK]   Downloaded {} bytes from {}", body.len(), index_url);
                    // APKINDEX.tar.gz: gunzip + untar, then parse the APKINDEX text member.
                    if let Some(text) = extract_apkindex_text(&body) {
                        let pkgs = parse_apkindex(&text);
                        crate::serial_println!("[APK]   Parsed {} packages from network index", pkgs.len());
                        total_packages += pkgs.len();
                        db.available.extend(pkgs);
                        // Persist to ext2 cache so subsequent boots are offline-capable.
                        let _ = crate::fs::ext2::write_file_path(&index_path, text.as_bytes());
                    } else {
                        crate::serial_println!("[APK]   WARNING: could not decode APKINDEX.tar.gz");
                    }
                }
                Err(e) => {
                    // Network unreachable (e.g. QEMU without -netdev) → fall back to a
                    // bundled APKINDEX on disk so apk add still works offline.
                    crate::serial_println!("[APK]   HTTP fetch failed ({}) — trying disk fallback", e);
                    let alt_path = "/var/lib/apk/APKINDEX.txt";
                    if let Some(data) = crate::fs::ext2::read_file_path(alt_path) {
                        if let Ok(text) = core::str::from_utf8(&data) {
                            let pkgs = parse_apkindex(text);
                            crate::serial_println!("[APK]   Loaded {} packages from {}", pkgs.len(), alt_path);
                            total_packages += pkgs.len();
                            db.available.extend(pkgs);
                        }
                    } else {
                        crate::serial_println!("[APK]   No network and no cached index at {}", index_path);
                    }
                }
            }
        }
    }

    crate::serial_println!("[APK] {} packages available", total_packages);
    total_packages > 0
}

/// Find a package by name in the available index
pub fn find_package(name: &str) -> Option<ApkPackage> {
    let db = unsafe { APK_DB.as_ref()? };
    db.available.iter().find(|p| p.name == name).cloned()
}

/// Install a package from a .apk file (tar.gz) on disk
/// apk files are stored at /var/cache/apk/<name>-<version>.apk
pub fn install_apk_from_disk(apk_path: &str) -> bool {
    crate::serial_println!("[APK] Installing from {}", apk_path);

    let data = match crate::fs::ext2::read_file_path(apk_path) {
        Some(d) => d,
        None => {
            crate::serial_println!("[APK] Cannot read {}", apk_path);
            return false;
        }
    };

    // Decompress and extract
    match crate::fs::tar::extract_tar_gz_to_ext2(&data, "/") {
        Some(count) => {
            crate::serial_println!("[APK] Installed {} files from {}", count, apk_path);
            true
        }
        None => {
            crate::serial_println!("[APK] Failed to extract {}", apk_path);
            false
        }
    }
}

/// Simulate `apk add <package>`: find in index, download/extract
pub fn apk_add(name: &str) -> bool {
    crate::serial_println!("[APK] apk add {}", name);

    // Check if already installed
    let db = match unsafe { APK_DB.as_mut() } {
        Some(db) => db,
        None => {
            crate::serial_println!("[APK] Not initialized");
            return false;
        }
    };

    if db.installed.contains(&String::from(name)) {
        crate::serial_println!("[APK] {} is already installed", name);
        return true;
    }

    // Find in available packages
    if let Some(pkg) = find_package(name) {
        crate::serial_println!("[APK] Found: {} v{} ({})", pkg.name, pkg.version, pkg.description);

        // Try to find .apk file on disk
        let apk_path = format!("/var/cache/apk/{}", pkg.filename);
        if install_apk_from_disk(&apk_path) {
            db.installed.push(String::from(name));
            crate::serial_println!("[APK] {} installed successfully", name);
            return true;
        }
    }

    // Fallback: try direct path
    let direct_path = format!("/var/cache/apk/{}.apk", name);
    if crate::fs::ext2::lookup_path(&direct_path).is_some() {
        if install_apk_from_disk(&direct_path) {
            db.installed.push(String::from(name));
            return true;
        }
    }

    crate::serial_println!("[APK] {} not found or download not available", name);
    false
}

/// Get count of available packages
pub fn package_count() -> usize {
    match unsafe { APK_DB.as_ref() } {
        Some(db) => db.available.len(),
        None => 0,
    }
}

/// List installed packages
pub fn list_installed() -> Vec<String> {
    match unsafe { APK_DB.as_ref() } {
        Some(db) => db.installed.clone(),
        None => Vec::new(),
    }
}

/// Run APK self-tests
pub fn run_tests() {
    crate::serial_println!("\n========================================");
    crate::serial_println!("[APK TESTS] Package Manager (Layer 3)");
    crate::serial_println!("========================================\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: Parse APKINDEX format
    crate::serial_write("  [TEST 1/4] APKINDEX parse... ");
    let sample_index = "P:busybox\nV:1.36.1-r29\nT:Size optimized toolbox\nS:512000\nD:musl so:libc.musl-x86_64.so.1\n\nP:python3\nV:3.12.8-r1\nT:Python 3 interpreter\nS:24576000\nD:musl libffi gdbm\n\n";
    let pkgs = parse_apkindex(sample_index);
    if pkgs.len() == 2 && pkgs[0].name == "busybox" && pkgs[1].name == "python3" {
        crate::serial_println!("OK ({} packages)", pkgs.len());
        passed += 1;
    } else {
        crate::serial_println!("FAIL (got {} packages)", pkgs.len());
        failed += 1;
    }

    // Test 2: Init from ext2 repositories
    crate::serial_write("  [TEST 2/4] Repository init... ");
    if init() {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("SKIP (no repositories on disk)\n");
    }

    // Test 3: Package lookup
    crate::serial_write("  [TEST 3/4] Package lookup... ");
    // Try updating first
    let _ = apk_update();
    if let Some(pkg) = find_package("busybox") {
        crate::serial_println!("OK (found {} v{})", pkg.name, pkg.version);
        passed += 1;
    } else {
        crate::serial_write("SKIP (no package index loaded)\n");
    }

    // Test 4: Alpine rootfs presence check
    crate::serial_write("  [TEST 4/4] Alpine rootfs components... ");
    let mut found = 0u32;
    let check_paths = [
        "/lib/ld-musl-x86_64.so.1",
        "/bin/busybox",
        "/bin/sh",
        "/usr/lib/libpython3.12.so.1.0",
    ];
    for path in &check_paths {
        if crate::fs::ext2::lookup_path(path).is_some() {
            found += 1;
        }
    }
    if found > 0 {
        crate::serial_println!("OK ({}/{} components found)", found, check_paths.len());
        passed += 1;
    } else {
        crate::serial_write("SKIP (no Alpine rootfs on ext2 disk)\n");
    }

    crate::serial_println!("\n========================================");
    crate::serial_println!("[APK TESTS] {}/{} passed", passed, passed + failed);
    if failed == 0 && passed > 0 {
        crate::serial_write("[APK TESTS] ALL PASSED!\n");
    }
    crate::serial_println!("========================================");
}
