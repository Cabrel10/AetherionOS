#!/bin/bash
# Script pour créer les issues GitHub documentant les problèmes locaux

GITHUB_TOKEN="GH_TOKEN_PLACEHOLDER"
REPO_OWNER="Cabrel10"
REPO_NAME="AetherionOS"

# Issue 1: Tests FAT32 causent OOM
echo "Creating Issue 1: FAT32 tests cause OOM with large files..."
curl -X POST \
  -H "Authorization: token $GITHUB_TOKEN" \
  -H "Accept: application/vnd.github.v3+json" \
  https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/issues \
  -d '{
    "title": "[BUG] FAT32 run_tests() causes OOM with files >8MB",
    "body": "## Problem\n\nThe `fs::fat32::run_tests()` function in `kernel/src/main.rs` calls `read_file_path()` which attempts to load entire files into memory.\n\n**Symptom:**\n```\n[FATAL] Heap allocation failed! size=2147483648, align=1\n[FATAL] System halted - out of memory.\n```\n\n**Root Cause:**\n- TEST 5/5 loads entire file to display only 16 bytes\n- With Mistral 7B (2GB files), kernel heap (8MB) cannot allocate\n- System crashes before reaching Ring 3\n\n## Impact\n- Cannot test with real LLM models\n- System does not boot if disk.img contains files >8MB\n- Unit tests become time bombs\n\n## Proposed Solution\n\n```rust\n// In run_tests()\nif entry.file_size > MAX_SAFE_TEST_SIZE {\n    // Use chunked read for large files\n    match read_file_path_chunk(&path, 0, 4096) {\n        Some(chunk) => test_passed(),\n        None => test_failed(),\n    }\n} else {\n    // Normal read for small files\n    match read_file_path(&path) { ... }\n}\n```\n\n## Temporary Workaround\n\nDisable tests by default:\n```rust\n// kernel/src/main.rs line ~1430\n// fs::fat32::run_tests();  // Disabled to avoid OOM\n```\n\n## Environment\n- Kernel heap: 8MB\n- Test file: Mistral 7B (2GB per part)\n- Jalons tested: J52-J60",
    "labels": ["bug", "critical", "memory", "testing"]
  }'

echo ""
echo "Creating Issue 2: Missing file_exists() function..."
curl -X POST \
  -H "Authorization: token $GITHUB_TOKEN" \
  -H "Accept: application/vnd.github.v3+json" \
  https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/issues \
  -d '{
    "title": "[ENHANCEMENT] Add file_exists() to avoid loading files just to check existence",
    "body": "## Problem\n\n`sys_open()` currently uses `read_file_path()` to check if a file exists, which loads the entire file into memory.\n\n**Code:**\n```rust\n// kernel/src/arch/x86_64/syscall.rs line ~553\nlet exists = crate::fs::fat32::read_file_path(disk_path).is_some();\n// ← Loads 2GB just to check existence!\n```\n\n## Impact\n- OOM when opening files >8MB\n- Even with chunked read available (J52), sys_open ignores it\n- Inefficient resource usage\n\n## Proposed Solution\n\nAdd `file_exists()` function to FAT32 module:\n\n```rust\n// kernel/src/fs/fat32.rs\npub fn file_exists(disk_path: &str) -> bool {\n    unsafe {\n        let fs = match FAT32_FS {\n            Some(ref f) => f,\n            None => return false,\n        };\n        fs.find_directory_entry(disk_path).is_some()\n    }\n}\n```\n\nUpdate `sys_open()`:\n```rust\nlet exists = crate::fs::fat32::file_exists(disk_path);\n```\n\n## Benefits\n- No memory allocation for existence checks\n- Consistent with chunked read philosophy (J52)\n- Prevents OOM on large files\n\n## Related\n- Depends on J52 (chunked read)\n- Related to Issue #1 (FAT32 tests OOM)",
    "labels": ["enhancement", "filesystem", "performance"]
  }'

echo ""
echo "Creating Issue 3: disk.img tracked in Git..."
curl -X POST \
  -H "Authorization: token $GITHUB_TOKEN" \
  -H "Accept: application/vnd.github.v3+json" \
  https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/issues \
  -d '{
    "title": "[WORKFLOW] disk.img should not be tracked in Git",
    "body": "## Problem\n\n`disk.img` is currently tracked by Git, causing issues for local development:\n\n- Each `git pull` overwrites local disk.img (8GB with models) with upstream version (64MB empty)\n- Loss of 4.1GB of data (Mistral 7B) on every sync\n- Developers must manually recreate disk.img after each pull\n\n## Impact on Workflow\n\n```bash\n$ git pull origin main\n# ... disk.img overwritten ...\n$ ls -lh disk.img\n-rw-rw-r-- 1 user user 64M  # Should be 8GB!\n\n# Manual recreation needed\n$ sudo mount -o loop disk.img /mnt/aetherion\n$ sudo cp mistral_part_* /mnt/aetherion/models/\n# ... 5 minutes wait ...\n```\n\n## Proposed Solutions\n\n### Option A: .gitignore\n```bash\n# .gitignore\ndisk.img\ndisk_*.img\n*.gguf\nmodels/\n```\n\n### Option B: Template Approach\n```bash\n# Git contains disk_template.img (64MB)\n# Users create their own disk.img\ncp disk_template.img disk.img\n./setup_disk.sh  # Script for expansion + model copy\n```\n\n### Option C: Setup Script\n```bash\n#!/bin/bash\n# setup_local_env.sh\nif [ ! -f \"disk.img\" ] || [ $(stat -c%s disk.img) -lt 8000000000 ]; then\n    echo \"Creating production disk.img (8GB)...\"\n    dd if=/dev/zero of=disk.img bs=1M count=8192\n    mkfs.vfat -F 32 disk.img\n    # ... mount and setup ...\nfi\n```\n\n## Recommendation\n\nOption B (template) is preferred:\n- Provides base structure\n- Allows local customization\n- No Git conflicts\n- Clear separation of concerns",
    "labels": ["workflow", "documentation", "git"]
  }'

echo ""
echo "Creating Issue 4: Boot agent hardcoded in kernel..."
curl -X POST \
  -H "Authorization: token $GITHUB_TOKEN" \
  -H "Accept: application/vnd.github.v3+json" \
  https://api.github.com/repos/$REPO_OWNER/$REPO_NAME/issues \
  -d '{
    "title": "[ENHANCEMENT] Add boot configuration file instead of hardcoded agent",
    "body": "## Problem\n\nThe boot agent is hardcoded in the kernel:\n\n```rust\n// kernel/src/main.rs line ~1822\nlet elf_binary = AGENT_Q4_DEQUANT_ELF;  // ← Hardcoded!\nlet elf_name = \"/bin/agent_q4_dequant.elf\";\n```\n\n## Impact\n- Changing agent requires full kernel recompilation (~8 seconds)\n- Cannot quickly test different agents\n- No fallback mechanism if agent crashes\n- Difficult to configure for different environments\n\n## Proposed Solution\n\nAdd boot configuration file:\n\n```toml\n# /disk/boot.toml\n[boot]\ndefault_agent = \"agent_visual_term\"\nfallback_agent = \"agent_orchestrator\"\ntimeout_seconds = 30\n\n[disk]\nmax_file_preload_mb = 1\n\n[debug]\nenable_fat32_tests = false\nlog_level = \"info\"\n```\n\n**Implementation:**\n```rust\n// Read /disk/boot.toml at startup (before tests)\nlet config = read_boot_config();\nlet agent_name = config.default_agent.unwrap_or(\"agent_http\");\nlet elf_binary = get_agent_binary(agent_name)?;\n```\n\n## Benefits\n- Change agent without recompilation\n- Persistent configuration on disk\n- Automatic fallback on failure\n- Environment-specific settings\n- Can disable dangerous tests via config\n\n## Related\n- Would solve Issue #1 (could disable FAT32 tests via config)\n- Improves development workflow\n- Enables production vs development configurations",
    "labels": ["enhancement", "configuration", "boot"]
  }'

echo ""
echo "✓ All issues created successfully!"
echo ""
echo "View issues at: https://github.com/$REPO_OWNER/$REPO_NAME/issues"
