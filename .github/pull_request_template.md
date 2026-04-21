## Description

<!-- Brief summary of what this PR does and why. -->

## Changes

<!-- List the key changes made in this PR. -->

- [ ] Kernel changes (`kernel/src/`)
- [ ] Userspace changes (`userspace/`)
- [ ] Build system / scripts
- [ ] Documentation
- [ ] CI / workflows

## Jalon / Milestone

<!-- Which Jalon does this correspond to? e.g., Jalon 146 -->

**Jalon**: 

## Testing

### Build Verification
- [ ] `cargo check --lib --target x86_64-aetherion.json` passes
- [ ] `cargo bootimage --release` succeeds
- [ ] No new compiler warnings introduced

### Runtime Verification
- [ ] QEMU boot test completes without PANIC / SIGSEGV
- [ ] Serial console shows expected boot messages
- [ ] `execve` syscall logs visible for loaded binaries

### Specific Tests
<!-- Describe any additional tests you ran. -->

## Risk Assessment

<!-- What could break? What should reviewers pay extra attention to? -->

| Area | Risk | Mitigation |
|------|------|------------|
| IDT / interrupts | | |
| Syscall handling | | |
| ELF loading | | |
| FAT32 filesystem | | |
| Scheduler | | |

## Anti-Corruption Notes

<!-- If you encountered sandbox/filesystem corruption, document it here. -->

- [ ] Build used tmpfs for `kernel/target/` 
- [ ] Cargo registry was pre-fetched and locked read-only
- [ ] Single-threaded build (`CARGO_BUILD_JOBS=1`)

## Screenshots / Logs

<!-- Paste relevant QEMU output or build logs if applicable. -->

<details>
<summary>Build output</summary>

```
# paste cargo bootimage output here
```

</details>

<details>
<summary>QEMU boot log</summary>

```
# paste QEMU serial output here
```

</details>

## Checklist

- [ ] Code compiles without errors
- [ ] No new PANIC paths introduced without justification
- [ ] `include_bytes!` paths are valid (no missing ELFs)
- [ ] Commit message follows `type(scope): description` format
- [ ] BUILD.md updated if build process changed
- [ ] CHANGELOG.md updated (if applicable)
