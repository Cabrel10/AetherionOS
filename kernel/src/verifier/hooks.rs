// verifier/hooks.rs - Verifier Hooks for VFS Operations (Couche 5)
//
// Provides the hook interface between the VFS (Couche 4) and the
// policy engine. Each VFS operation can call verify_write / verify_read
// before executing, and the verifier decides Allow/Deny/Audit.

use super::policy::{self, OperationContext, OperationType, PolicyAction, VerifierError};
use crate::ipc::{self, IntentMessage, ComponentId, Priority};

/// Intent IDs for verifier events on the Cognitive Bus
const VERIFIER_ALLOW: u32 = 0x5001;
const VERIFIER_DENY: u32 = 0x5002;
const VERIFIER_AUDIT: u32 = 0x5003;

/// Verify a VFS write operation against the security policy.
///
/// Returns Ok(()) if allowed, Err(VerifierError::PolicyDenied) if denied.
/// Audit results in Ok(()) but the event is logged.
pub fn verify_write(path: &str, data_size: usize) -> Result<(), VerifierError> {
    let ctx = OperationContext {
        path,
        operation: OperationType::VfsWrite,
        data_size,
        source_component: ComponentId::Filesystem as u8,
    };

    match policy::evaluate(&ctx) {
        PolicyAction::Allow => {
            bus_publish(VERIFIER_ALLOW, path.len() as u64);
            Ok(())
        }
        PolicyAction::Deny => {
            bus_publish(VERIFIER_DENY, path.len() as u64);
            Err(VerifierError::PolicyDenied)
        }
        PolicyAction::Audit => {
            bus_publish(VERIFIER_AUDIT, data_size as u64);
            Ok(()) // Allowed but logged
        }
    }
}

/// Verify a VFS read operation against the security policy.
pub fn verify_read(path: &str) -> Result<(), VerifierError> {
    let ctx = OperationContext {
        path,
        operation: OperationType::VfsRead,
        data_size: 0,
        source_component: ComponentId::Filesystem as u8,
    };

    match policy::evaluate(&ctx) {
        PolicyAction::Allow => {
            bus_publish(VERIFIER_ALLOW, path.len() as u64);
            Ok(())
        }
        PolicyAction::Deny => {
            bus_publish(VERIFIER_DENY, path.len() as u64);
            Err(VerifierError::PolicyDenied)
        }
        PolicyAction::Audit => {
            bus_publish(VERIFIER_AUDIT, 0);
            Ok(())
        }
    }
}

/// Verify a device mount operation.
pub fn verify_mount(path: &str) -> Result<(), VerifierError> {
    let ctx = OperationContext {
        path,
        operation: OperationType::DeviceMount,
        data_size: 0,
        source_component: ComponentId::Filesystem as u8,
    };

    match policy::evaluate(&ctx) {
        PolicyAction::Allow => Ok(()),
        PolicyAction::Deny => Err(VerifierError::PolicyDenied),
        PolicyAction::Audit => Ok(()),
    }
}

/// Publish verifier event to Cognitive Bus (best-effort)
fn bus_publish(intent_id: u32, payload: u64) {
    let msg = IntentMessage::new(
        ComponentId::Verifier,
        ComponentId::Orchestrator,
        intent_id,
        Priority::High,
        payload,
    );

    if let Err(e) = ipc::bus::publish(msg) {
        crate::serial_println!("[VERIFIER][WARN] Bus publish failed: {:?}", e);
    }
}
