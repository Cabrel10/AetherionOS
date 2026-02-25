// verifier/mod.rs - Couche 5: Verifier (Policy Engine)
//
// ACHA-OS Security Verifier Layer
//
// Architecture:
//   - Rule-based policy engine for VFS and IPC operations
//   - Hook system for intercepting writes with permission checks
//   - Cognitive Bus integration for security event reporting
//   - Configurable rules with allow/deny/audit actions
//   - Atomic metrics for verifier operations
//
// Security Model:
//   Every VFS write passes through the verifier BEFORE execution:
//     1. Rule evaluation (path patterns, size limits, rate limits)
//     2. Action enforcement (Allow, Deny, Audit)
//     3. Event publication to Cognitive Bus
//     4. Metrics update

pub mod policy;
pub mod hooks;
