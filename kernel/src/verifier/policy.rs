// verifier/policy.rs - Policy Engine (Couche 5)
//
// Rule-based security policy engine.
// Rules are evaluated in order; first match wins.
// Default action: Deny (whitelist approach).
//
// Rule types:
//   - PathPrefix: match operations on paths starting with a prefix
//   - MaxWriteSize: enforce maximum write payload size
//   - ReadOnly: deny all writes to matched paths
//   - RateLimit: cap operations per interval (future)

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicU64, Ordering};

// ===== Policy Actions =====

/// Action to take when a rule matches
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    /// Allow the operation
    Allow,
    /// Deny the operation
    Deny,
    /// Allow but log (audit trail)
    Audit,
}

// ===== Rule Conditions =====

/// Condition that a rule checks
#[derive(Debug, Clone)]
pub enum RuleCondition {
    /// Path starts with this prefix (e.g., "/dev/")
    PathPrefix(String),
    /// Path exactly matches
    PathExact(String),
    /// Write size exceeds this limit (bytes)
    MaxWriteSize(usize),
    /// Any operation (catch-all)
    Any,
}

// ===== Policy Rule =====

/// A single policy rule
#[derive(Debug, Clone)]
pub struct PolicyRule {
    /// Human-readable rule name
    pub name: String,
    /// Condition to evaluate
    pub condition: RuleCondition,
    /// Action to take if condition matches
    pub action: PolicyAction,
    /// Operation type this rule applies to
    pub operation: OperationType,
    /// Whether rule is enabled
    pub enabled: bool,
}

/// Types of operations the verifier can intercept
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationType {
    /// VFS write operation
    VfsWrite,
    /// VFS read operation
    VfsRead,
    /// Device mount operation
    DeviceMount,
    /// Any operation
    Any,
}

impl PolicyRule {
    /// Create a new rule
    pub fn new(name: &str, condition: RuleCondition, action: PolicyAction, op: OperationType) -> Self {
        Self {
            name: String::from(name),
            condition,
            action,
            operation: op,
            enabled: true,
        }
    }

    /// Evaluate this rule against an operation context
    pub fn evaluate(&self, ctx: &OperationContext) -> Option<PolicyAction> {
        if !self.enabled {
            return None;
        }

        // Check operation type
        if self.operation != OperationType::Any && self.operation != ctx.operation {
            return None;
        }

        // Evaluate condition
        match &self.condition {
            RuleCondition::PathPrefix(prefix) => {
                if ctx.path.starts_with(prefix.as_str()) {
                    Some(self.action)
                } else {
                    None
                }
            }
            RuleCondition::PathExact(exact) => {
                if ctx.path == exact.as_str() {
                    Some(self.action)
                } else {
                    None
                }
            }
            RuleCondition::MaxWriteSize(max_size) => {
                if ctx.data_size > *max_size {
                    Some(self.action)
                } else {
                    None
                }
            }
            RuleCondition::Any => {
                Some(self.action)
            }
        }
    }
}

// ===== Operation Context =====

/// Context describing an operation to be verified
pub struct OperationContext<'a> {
    /// Path of the operation
    pub path: &'a str,
    /// Type of operation
    pub operation: OperationType,
    /// Size of data payload (for writes)
    pub data_size: usize,
    /// Source component ID
    pub source_component: u8,
}

// ===== Verifier Metrics =====

/// Atomic metrics for the verifier
pub struct VerifierMetrics {
    pub rules_evaluated: AtomicU64,
    pub operations_allowed: AtomicU64,
    pub operations_denied: AtomicU64,
    pub operations_audited: AtomicU64,
    pub policy_violations: AtomicU64,
}

impl VerifierMetrics {
    const fn new() -> Self {
        Self {
            rules_evaluated: AtomicU64::new(0),
            operations_allowed: AtomicU64::new(0),
            operations_denied: AtomicU64::new(0),
            operations_audited: AtomicU64::new(0),
            policy_violations: AtomicU64::new(0),
        }
    }
}

/// Global verifier metrics
static VERIFIER_METRICS: VerifierMetrics = VerifierMetrics::new();

/// Snapshot of verifier metrics
#[derive(Debug, Clone)]
pub struct VerifierMetricsSnapshot {
    pub rules_evaluated: u64,
    pub operations_allowed: u64,
    pub operations_denied: u64,
    pub operations_audited: u64,
    pub policy_violations: u64,
}

/// Get current verifier metrics snapshot
pub fn get_metrics() -> VerifierMetricsSnapshot {
    VerifierMetricsSnapshot {
        rules_evaluated: VERIFIER_METRICS.rules_evaluated.load(Ordering::Relaxed),
        operations_allowed: VERIFIER_METRICS.operations_allowed.load(Ordering::Relaxed),
        operations_denied: VERIFIER_METRICS.operations_denied.load(Ordering::Relaxed),
        operations_audited: VERIFIER_METRICS.operations_audited.load(Ordering::Relaxed),
        policy_violations: VERIFIER_METRICS.policy_violations.load(Ordering::Relaxed),
    }
}

// ===== Policy Engine =====

/// Maximum number of rules
const MAX_RULES: usize = 32;

lazy_static! {
    /// Global policy rule set
    static ref POLICY_RULES: Mutex<Vec<PolicyRule>> = Mutex::new(Vec::new());
}

/// Verifier errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifierError {
    /// Operation denied by policy
    PolicyDenied,
    /// Too many rules configured
    RuleLimitExceeded,
    /// Invalid rule configuration
    InvalidRule,
    /// Verifier not initialized
    NotInitialized,
}

impl core::fmt::Display for VerifierError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::PolicyDenied => write!(f, "Operation denied by security policy"),
            Self::RuleLimitExceeded => write!(f, "Maximum rule count exceeded"),
            Self::InvalidRule => write!(f, "Invalid rule configuration"),
            Self::NotInitialized => write!(f, "Verifier not initialized"),
        }
    }
}

/// Initialize the policy engine with default security rules
pub fn init() -> Result<(), VerifierError> {
    crate::serial_println!("[VERIFIER] Initializing policy engine...");

    let mut rules = POLICY_RULES.lock();
    rules.clear();

    // Rule 1: Allow reads from /dev/ (device access)
    rules.push(PolicyRule::new(
        "allow-dev-read",
        RuleCondition::PathPrefix(String::from("/dev/")),
        PolicyAction::Allow,
        OperationType::VfsRead,
    ));

    // Rule 2: Allow writes to /dev/ (device access)
    rules.push(PolicyRule::new(
        "allow-dev-write",
        RuleCondition::PathPrefix(String::from("/dev/")),
        PolicyAction::Allow,
        OperationType::VfsWrite,
    ));

    // Rule 3: Allow reads from /tmp/ (temporary files)
    rules.push(PolicyRule::new(
        "allow-tmp-read",
        RuleCondition::PathPrefix(String::from("/tmp/")),
        PolicyAction::Allow,
        OperationType::VfsRead,
    ));

    // Rule 4: Audit writes to /tmp/ (temporary files)
    rules.push(PolicyRule::new(
        "audit-tmp-write",
        RuleCondition::PathPrefix(String::from("/tmp/")),
        PolicyAction::Audit,
        OperationType::VfsWrite,
    ));

    // Rule 5: Deny writes larger than 64 KB (DoS protection)
    rules.push(PolicyRule::new(
        "deny-large-writes",
        RuleCondition::MaxWriteSize(65536),
        PolicyAction::Deny,
        OperationType::VfsWrite,
    ));

    // Rule 6: Deny access to /sys/ (system paths, reserved)
    rules.push(PolicyRule::new(
        "deny-sys-access",
        RuleCondition::PathPrefix(String::from("/sys/")),
        PolicyAction::Deny,
        OperationType::Any,
    ));

    // Rule 7: Default deny (whitelist approach, catch-all)
    rules.push(PolicyRule::new(
        "default-deny",
        RuleCondition::Any,
        PolicyAction::Deny,
        OperationType::Any,
    ));

    let count = rules.len();
    crate::serial_println!("[VERIFIER] Loaded {} policy rules", count);

    Ok(())
}

/// Add a custom policy rule
pub fn add_rule(rule: PolicyRule) -> Result<(), VerifierError> {
    let mut rules = POLICY_RULES.lock();
    if rules.len() >= MAX_RULES {
        return Err(VerifierError::RuleLimitExceeded);
    }
    crate::serial_println!("[VERIFIER] Adding rule: {}", rule.name);
    rules.push(rule);
    Ok(())
}

/// Evaluate an operation against the policy rule set
///
/// Returns the policy action (Allow/Deny/Audit).
/// First matching rule wins; default is Deny.
pub fn evaluate(ctx: &OperationContext) -> PolicyAction {
    let rules = POLICY_RULES.lock();

    for rule in rules.iter() {
        VERIFIER_METRICS.rules_evaluated.fetch_add(1, Ordering::Relaxed);

        if let Some(action) = rule.evaluate(ctx) {
            match action {
                PolicyAction::Allow => {
                    VERIFIER_METRICS.operations_allowed.fetch_add(1, Ordering::Relaxed);
                }
                PolicyAction::Deny => {
                    VERIFIER_METRICS.operations_denied.fetch_add(1, Ordering::Relaxed);
                    VERIFIER_METRICS.policy_violations.fetch_add(1, Ordering::Relaxed);
                    crate::serial_println!(
                        "[VERIFIER][DENY] Rule '{}' denied {:?} on '{}'",
                        rule.name, ctx.operation, ctx.path
                    );
                }
                PolicyAction::Audit => {
                    VERIFIER_METRICS.operations_audited.fetch_add(1, Ordering::Relaxed);
                    crate::serial_println!(
                        "[VERIFIER][AUDIT] Rule '{}' auditing {:?} on '{}'",
                        rule.name, ctx.operation, ctx.path
                    );
                }
            }
            return action;
        }
    }

    // Default deny (should not reach here if catch-all rule exists)
    VERIFIER_METRICS.operations_denied.fetch_add(1, Ordering::Relaxed);
    VERIFIER_METRICS.policy_violations.fetch_add(1, Ordering::Relaxed);
    PolicyAction::Deny
}

/// Get the number of active rules
pub fn rule_count() -> usize {
    POLICY_RULES.lock().len()
}
