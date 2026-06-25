//! Native Agentic Runtime — Agents as First-Class Kernel Processes
//!
//! # Architecture (2026-06-25)
//!
//! In AetherionOS, an **Agent** is not a userspace daemon — it is a **kernel
//! process with a PID**, enhanced with AI-specific capabilities. This is the
//! fundamental differentiator: agents are scheduled by the kernel scheduler,
//! communicate via the CognitiveBus, and access compute backends directly.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                   Agentic Runtime                           │
//! │                                                             │
//! │  ┌─────────────────────────────────────────────────────┐   │
//! │  │              Agent Registry (MAX=64)                 │   │
//! │  │                                                      │   │
//! │  │  Agent "Matriarch"    PID=1   Model=SmolLM-135M     │   │
//! │  │    |-- Agent "Coder"  PID=5   Model=DeepSeek-Coder  │   │
//! │  │    |-- Agent "Vision" PID=6   Model=LLaVA-7B        │   │
//! │  │    +-- Agent "Search" PID=7   Model=None (tool-only)│   │
//! │  │                                                      │   │
//! │  │  Each agent has:                                     │   │
//! │  │    - PID (from kernel process table)                 │   │
//! │  │    - Bound model (optional, from multi_model)        │   │
//! │  │    - Memory slots (from persistent_memory)           │   │
//! │  │    - Tool permissions (whitelist)                     │   │
//! │  │    - Compute affinity (preferred backend)            │   │
//! │  │    - Message inbox (via CognitiveBus)                │   │
//! │  │    - Token budget (max tokens/request)               │   │
//! │  │    - Energy class (high/normal/low priority power)   │   │
//! │  └─────────────────────────────────────────────────────┘   │
//! │                                                             │
//! │  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐ │
//! │  │ Lifecycle Mgr│  │ Router       │  │ Supervisor       │ │
//! │  │ spawn        │  │ route_intent │  │ health_check     │ │
//! │  │ suspend      │  │ broadcast    │  │ restart_failed   │ │
//! │  │ resume       │  │ delegate     │  │ escalate         │ │
//! │  │ terminate    │  │              │  │ rebalance        │ │
//! │  └──────────────┘  └──────────────┘  └──────────────────┘ │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Matriarchal Hierarchy
//!
//! - **Matriarch** (PID ~1): Root agent, orchestrates all others. Immortal.
//! - **SubMatriarch**: Specialist coordinators. Persistent across requests.
//! - **Worker**: Ephemeral agents for single tasks. Auto-terminate on completion.
//! - **Daemon**: Infrastructure agents (GC, health). Run forever.

use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, AtomicU32, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;

// ============================================================================
// TYPES
// ============================================================================

/// Unique agent identifier (distinct from PID).
pub type AgentId = u64;

/// Agent capability flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AgentCapability {
    Inference        = 1 << 0,
    ToolCalling      = 1 << 1,
    CodeGeneration   = 1 << 2,
    VisionProcessing = 1 << 3,
    Embedding        = 1 << 4,
    Orchestration    = 1 << 5,
    StorageAccess    = 1 << 6,
    NetworkAccess    = 1 << 7,
    SystemConfig     = 1 << 8,
    SpawnAgents      = 1 << 9,
}

/// Bitmask of capabilities.
pub type CapabilityMask = u32;

/// Agent lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AgentState {
    Created    = 0,
    Running    = 1,
    Suspended  = 2,
    Blocked    = 3,
    Completed  = 4,
    Failed     = 5,
    Terminated = 6,
}

impl AgentState {
    pub fn is_alive(self) -> bool {
        matches!(self, Self::Created | Self::Running | Self::Suspended | Self::Blocked)
    }
}

/// Agent class — determines lifecycle and priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AgentClass {
    Matriarch    = 0,
    SubMatriarch = 1,
    Worker       = 2,
    Daemon       = 3,
}

/// Tool permission entry.
#[derive(Debug, Clone)]
pub struct ToolPermission {
    pub tool_name: String,
    pub allowed: bool,
    pub max_calls_per_request: u32,
    pub calls_used: u32,
}

/// Agent resource limits.
#[derive(Debug, Clone, Copy)]
pub struct AgentLimits {
    pub max_tokens_per_request: u32,
    pub max_tokens_total: u64,
    pub max_kv_cache_bytes: u64,
    pub max_ticks_per_request: u64,
    pub max_children: u16,
    pub max_tool_calls: u16,
}

impl AgentLimits {
    pub const MATRIARCH: Self = Self {
        max_tokens_per_request: 4096,
        max_tokens_total: u64::MAX,
        max_kv_cache_bytes: 128 * 1024 * 1024,
        max_ticks_per_request: 100_000,
        max_children: 32,
        max_tool_calls: 50,
    };
    pub const SUB_MATRIARCH: Self = Self {
        max_tokens_per_request: 2048,
        max_tokens_total: 1_000_000,
        max_kv_cache_bytes: 64 * 1024 * 1024,
        max_ticks_per_request: 50_000,
        max_children: 8,
        max_tool_calls: 20,
    };
    pub const WORKER: Self = Self {
        max_tokens_per_request: 1024,
        max_tokens_total: 100_000,
        max_kv_cache_bytes: 16 * 1024 * 1024,
        max_ticks_per_request: 10_000,
        max_children: 0,
        max_tool_calls: 5,
    };
    pub const DAEMON: Self = Self {
        max_tokens_per_request: 0,
        max_tokens_total: 0,
        max_kv_cache_bytes: 0,
        max_ticks_per_request: u64::MAX,
        max_children: 4,
        max_tool_calls: 100,
    };
}

/// Agent resource usage counters.
#[derive(Debug, Clone, Copy)]
pub struct AgentUsage {
    pub tokens_generated: u64,
    pub tokens_prompted: u64,
    pub requests_completed: u64,
    pub tool_calls_executed: u64,
    pub ticks_consumed: u64,
    pub restart_count: u32,
    pub last_active_tick: u64,
    pub created_tick: u64,
}

impl Default for AgentUsage {
    fn default() -> Self {
        Self {
            tokens_generated: 0, tokens_prompted: 0,
            requests_completed: 0, tool_calls_executed: 0,
            ticks_consumed: 0, restart_count: 0,
            last_active_tick: 0, created_tick: 0,
        }
    }
}

/// The core Agent descriptor.
#[derive(Debug, Clone)]
pub struct AgentDescriptor {
    pub id: AgentId,
    pub pid: u64,
    pub parent_id: AgentId,
    pub name: String,
    pub class: AgentClass,
    pub state: AgentState,
    pub capabilities: CapabilityMask,
    pub model_idx: Option<u16>,
    pub preferred_backend: Option<u8>,
    pub limits: AgentLimits,
    pub usage: AgentUsage,
    pub tools: Vec<ToolPermission>,
    pub children: Vec<AgentId>,
    pub memory_session: Option<u64>,
    pub kv_session: Option<u64>,
    pub system_prompt: String,
    pub tags: Vec<(String, String)>,
}

// ============================================================================
// SPAWN REQUEST
// ============================================================================

pub struct SpawnRequest {
    pub name: String,
    pub class: AgentClass,
    pub parent_id: AgentId,
    pub capabilities: CapabilityMask,
    pub model_idx: Option<u16>,
    pub preferred_backend: Option<u8>,
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub tags: Vec<(String, String)>,
}

// ============================================================================
// ERRORS
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentError {
    NotFound,
    RegistryFull,
    InvalidParent,
    ClassViolation,
    LimitExceeded,
    InvalidState,
    CapabilityDenied,
    ToolDenied,
    ProcessSpawnFailed,
    ModelNotFound,
}

// ============================================================================
// MESSAGING
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    InferenceRequest = 0,
    InferenceResult  = 1,
    ToolRequest      = 2,
    ToolResult       = 3,
    Delegate         = 4,
    StatusReport     = 5,
    Shutdown         = 6,
    Ping             = 7,
    Pong             = 8,
    MemorySync       = 9,
}

#[derive(Debug, Clone)]
pub struct AgentMessage {
    pub from: AgentId,
    pub to: AgentId,
    pub msg_type: MessageType,
    pub payload: String,
    pub correlation_id: u64,
    pub tick: u64,
}

// ============================================================================
// SUPERVISOR ACTIONS
// ============================================================================

#[derive(Debug, Clone, Copy)]
pub enum SupervisorAction {
    Restarted(AgentId),
    Cleaned(AgentId),
    MarkedFailed(AgentId),
}

// ============================================================================
// GLOBAL STATE
// ============================================================================

const MAX_AGENTS: usize = 64;
static NEXT_AGENT_ID: AtomicU64 = AtomicU64::new(1);
static AGENT_COUNT: AtomicU32 = AtomicU32::new(0);
static AGENTS_SPAWNED: AtomicU64 = AtomicU64::new(0);
static AGENTS_TERMINATED: AtomicU64 = AtomicU64::new(0);
static CURRENT_TICK: AtomicU64 = AtomicU64::new(0);

lazy_static! {
    static ref AGENT_REGISTRY: Mutex<BTreeMap<AgentId, AgentDescriptor>> =
        Mutex::new(BTreeMap::new());
    static ref PID_TO_AGENT: Mutex<BTreeMap<u64, AgentId>> =
        Mutex::new(BTreeMap::new());
    static ref AGENT_MAILBOX: Mutex<BTreeMap<AgentId, Vec<AgentMessage>>> =
        Mutex::new(BTreeMap::new());
}

// ============================================================================
// LIFECYCLE MANAGEMENT
// ============================================================================

/// Spawn a new agent. Returns AgentId on success.
pub fn spawn_agent(req: SpawnRequest) -> Result<AgentId, AgentError> {
    let count = AGENT_COUNT.load(Ordering::Acquire) as usize;
    if count >= MAX_AGENTS {
        return Err(AgentError::RegistryFull);
    }

    // Validate parent
    if req.parent_id != 0 {
        let registry = AGENT_REGISTRY.lock();
        let parent = registry.get(&req.parent_id).ok_or(AgentError::InvalidParent)?;
        if parent.capabilities & (AgentCapability::SpawnAgents as u32) == 0 {
            return Err(AgentError::CapabilityDenied);
        }
        if parent.children.len() >= parent.limits.max_children as usize {
            return Err(AgentError::LimitExceeded);
        }
        match parent.class {
            AgentClass::Matriarch | AgentClass::SubMatriarch | AgentClass::Daemon => {}
            AgentClass::Worker => return Err(AgentError::ClassViolation),
        }
        drop(registry);
    }

    let agent_id = NEXT_AGENT_ID.fetch_add(1, Ordering::Relaxed);
    let tick = CURRENT_TICK.load(Ordering::Relaxed);

    let limits = match req.class {
        AgentClass::Matriarch    => AgentLimits::MATRIARCH,
        AgentClass::SubMatriarch => AgentLimits::SUB_MATRIARCH,
        AgentClass::Worker       => AgentLimits::WORKER,
        AgentClass::Daemon       => AgentLimits::DAEMON,
    };

    let tools: Vec<ToolPermission> = req.tools.iter().map(|name| {
        ToolPermission {
            tool_name: name.clone(),
            allowed: true,
            max_calls_per_request: limits.max_tool_calls as u32,
            calls_used: 0,
        }
    }).collect();

    // PID = 1000 + agent_id (real binding via crate::process at runtime)
    let pid = 1000 + agent_id;

    let descriptor = AgentDescriptor {
        id: agent_id,
        pid,
        parent_id: req.parent_id,
        name: req.name.clone(),
        class: req.class,
        state: AgentState::Created,
        capabilities: req.capabilities,
        model_idx: req.model_idx,
        preferred_backend: req.preferred_backend,
        limits,
        usage: AgentUsage { created_tick: tick, ..AgentUsage::default() },
        tools,
        children: Vec::new(),
        memory_session: None,
        kv_session: None,
        system_prompt: req.system_prompt,
        tags: req.tags,
    };

    {
        let mut registry = AGENT_REGISTRY.lock();
        registry.insert(agent_id, descriptor);
    }
    {
        let mut pid_map = PID_TO_AGENT.lock();
        pid_map.insert(pid, agent_id);
    }
    {
        let mut mailbox = AGENT_MAILBOX.lock();
        mailbox.insert(agent_id, Vec::new());
    }

    if req.parent_id != 0 {
        let mut registry = AGENT_REGISTRY.lock();
        if let Some(parent) = registry.get_mut(&req.parent_id) {
            parent.children.push(agent_id);
        }
    }

    AGENT_COUNT.fetch_add(1, Ordering::Release);
    AGENTS_SPAWNED.fetch_add(1, Ordering::Relaxed);

    crate::serial_println!(
        "[AGENT-RT] Spawned #{} '{}' class={:?} pid={} parent={} caps=0x{:x}",
        agent_id, req.name, req.class, pid, req.parent_id, req.capabilities
    );

    Ok(agent_id)
}

/// Start agent (Created/Suspended -> Running).
pub fn start_agent(id: AgentId) -> Result<(), AgentError> {
    let mut registry = AGENT_REGISTRY.lock();
    let agent = registry.get_mut(&id).ok_or(AgentError::NotFound)?;
    if agent.state != AgentState::Created && agent.state != AgentState::Suspended {
        return Err(AgentError::InvalidState);
    }
    agent.state = AgentState::Running;
    crate::serial_println!("[AGENT-RT] Started #{} '{}'", id, agent.name);
    Ok(())
}

/// Suspend agent (Running -> Suspended). Preserves KV cache and memory.
pub fn suspend_agent(id: AgentId) -> Result<(), AgentError> {
    let mut registry = AGENT_REGISTRY.lock();
    let agent = registry.get_mut(&id).ok_or(AgentError::NotFound)?;
    if agent.state != AgentState::Running {
        return Err(AgentError::InvalidState);
    }
    agent.state = AgentState::Suspended;
    crate::serial_println!("[AGENT-RT] Suspended #{} '{}'", id, agent.name);
    Ok(())
}

/// Resume agent (Suspended -> Running).
pub fn resume_agent(id: AgentId) -> Result<(), AgentError> {
    let mut registry = AGENT_REGISTRY.lock();
    let agent = registry.get_mut(&id).ok_or(AgentError::NotFound)?;
    if agent.state != AgentState::Suspended {
        return Err(AgentError::InvalidState);
    }
    agent.state = AgentState::Running;
    crate::serial_println!("[AGENT-RT] Resumed #{} '{}'", id, agent.name);
    Ok(())
}

/// Terminate agent. Cleans up resources, removes from parent.
pub fn terminate_agent(id: AgentId) -> Result<(), AgentError> {
    let (parent_id, name, pid, children) = {
        let mut registry = AGENT_REGISTRY.lock();
        let agent = registry.get_mut(&id).ok_or(AgentError::NotFound)?;
        if agent.class == AgentClass::Matriarch {
            return Err(AgentError::ClassViolation);
        }
        agent.state = AgentState::Terminated;
        (agent.parent_id, agent.name.clone(), agent.pid, agent.children.clone())
    };

    // Terminate children recursively
    for child_id in children {
        let _ = terminate_agent(child_id);
    }

    // Remove from parent
    if parent_id != 0 {
        let mut registry = AGENT_REGISTRY.lock();
        if let Some(parent) = registry.get_mut(&parent_id) {
            parent.children.retain(|&c| c != id);
        }
    }

    { let mut m = PID_TO_AGENT.lock(); m.remove(&pid); }
    { let mut m = AGENT_MAILBOX.lock(); m.remove(&id); }

    AGENT_COUNT.fetch_sub(1, Ordering::Release);
    AGENTS_TERMINATED.fetch_add(1, Ordering::Relaxed);
    crate::serial_println!("[AGENT-RT] Terminated #{} '{}'", id, name);
    Ok(())
}

// ============================================================================
// MESSAGING
// ============================================================================

/// Send a message to an agent's mailbox.
pub fn send_message(msg: AgentMessage) -> Result<(), AgentError> {
    {
        let registry = AGENT_REGISTRY.lock();
        let target = registry.get(&msg.to).ok_or(AgentError::NotFound)?;
        if !target.state.is_alive() {
            return Err(AgentError::InvalidState);
        }
    }
    let mut mailbox = AGENT_MAILBOX.lock();
    if let Some(inbox) = mailbox.get_mut(&msg.to) {
        if inbox.len() >= 256 { inbox.remove(0); }
        inbox.push(msg);
    }
    Ok(())
}

/// Receive next message (FIFO). Returns None if empty.
pub fn recv_message(id: AgentId) -> Option<AgentMessage> {
    let mut mailbox = AGENT_MAILBOX.lock();
    mailbox.get_mut(&id).and_then(|inbox| {
        if inbox.is_empty() { None } else { Some(inbox.remove(0)) }
    })
}

/// Check if agent has pending messages.
pub fn has_messages(id: AgentId) -> bool {
    let mailbox = AGENT_MAILBOX.lock();
    mailbox.get(&id).map(|m| !m.is_empty()).unwrap_or(false)
}

/// Broadcast to all agents of a specific class.
pub fn broadcast(from: AgentId, class: AgentClass, msg_type: MessageType, payload: String) {
    let targets: Vec<AgentId> = {
        let registry = AGENT_REGISTRY.lock();
        registry.values()
            .filter(|a| a.class == class && a.state.is_alive() && a.id != from)
            .map(|a| a.id)
            .collect()
    };
    let tick = CURRENT_TICK.load(Ordering::Relaxed);
    let corr = NEXT_AGENT_ID.fetch_add(1, Ordering::Relaxed);
    for target in targets {
        let _ = send_message(AgentMessage {
            from, to: target, msg_type,
            payload: payload.clone(), correlation_id: corr, tick,
        });
    }
}

// ============================================================================
// ROUTING — Intent-based agent selection
// ============================================================================

/// Find the best agent to handle a given intent based on capabilities and load.
pub fn route_intent(
    required_caps: CapabilityMask,
    preferred_tag: Option<(&str, &str)>,
) -> Option<AgentId> {
    let registry = AGENT_REGISTRY.lock();
    let mut best: Option<(AgentId, u32)> = None;

    for agent in registry.values() {
        if !agent.state.is_alive() || agent.state == AgentState::Suspended { continue; }
        if agent.capabilities & required_caps != required_caps { continue; }
        if agent.class == AgentClass::Daemon { continue; }

        let mut score: u32 = 100;
        match agent.class {
            AgentClass::Matriarch    => score += 10,
            AgentClass::SubMatriarch => score += 50,
            AgentClass::Worker       => score += 30,
            AgentClass::Daemon       => {}
        }

        if let Some((key, value)) = preferred_tag {
            if agent.tags.iter().any(|(k, v)| k == key && v == value) {
                score += 100;
            }
        }

        if required_caps & (AgentCapability::Inference as u32) != 0 && agent.model_idx.is_some() {
            score += 40;
        }

        // Penalize loaded agents
        let load = agent.usage.requests_completed
            .saturating_mul(100)
            .checked_div(agent.limits.max_tokens_total.max(1))
            .unwrap_or(0) as u32;
        score = score.saturating_sub(load);

        match best {
            None => best = Some((agent.id, score)),
            Some((_, bs)) if score > bs => best = Some((agent.id, score)),
            _ => {}
        }
    }
    best.map(|(id, _)| id)
}

/// Delegate task to child or spawn a new worker.
pub fn delegate(
    from: AgentId,
    task_payload: String,
    required_caps: CapabilityMask,
) -> Result<AgentId, AgentError> {
    // Try existing child
    let suitable_child = {
        let registry = AGENT_REGISTRY.lock();
        let from_agent = registry.get(&from).ok_or(AgentError::NotFound)?;
        from_agent.children.iter().find(|&&cid| {
            registry.get(&cid).map(|c| {
                c.state.is_alive() && (c.capabilities & required_caps == required_caps)
            }).unwrap_or(false)
        }).copied()
    };

    if let Some(child_id) = suitable_child {
        let tick = CURRENT_TICK.load(Ordering::Relaxed);
        send_message(AgentMessage {
            from, to: child_id, msg_type: MessageType::Delegate,
            payload: task_payload,
            correlation_id: NEXT_AGENT_ID.fetch_add(1, Ordering::Relaxed),
            tick,
        })?;
        return Ok(child_id);
    }

    // Spawn worker
    {
        let registry = AGENT_REGISTRY.lock();
        let from_agent = registry.get(&from).ok_or(AgentError::NotFound)?;
        if from_agent.capabilities & (AgentCapability::SpawnAgents as u32) == 0 {
            return Err(AgentError::CapabilityDenied);
        }
    }

    let worker_id = spawn_agent(SpawnRequest {
        name: String::from("delegated-worker"),
        class: AgentClass::Worker,
        parent_id: from,
        capabilities: required_caps,
        model_idx: None,
        preferred_backend: None,
        system_prompt: String::new(),
        tools: Vec::new(),
        tags: Vec::new(),
    })?;
    start_agent(worker_id)?;

    let tick = CURRENT_TICK.load(Ordering::Relaxed);
    send_message(AgentMessage {
        from, to: worker_id, msg_type: MessageType::Delegate,
        payload: task_payload,
        correlation_id: NEXT_AGENT_ID.fetch_add(1, Ordering::Relaxed),
        tick,
    })?;
    Ok(worker_id)
}

// ============================================================================
// SUPERVISOR — Health monitoring and auto-restart
// ============================================================================

/// Supervisor tick: health-checks, auto-restart, cleanup.
pub fn supervisor_tick() -> Vec<SupervisorAction> {
    let tick = CURRENT_TICK.fetch_add(1, Ordering::Relaxed) + 1;
    let mut actions = Vec::new();

    let snapshot: Vec<(AgentId, AgentClass, AgentState, u64, String)> = {
        let registry = AGENT_REGISTRY.lock();
        registry.values()
            .map(|a| (a.id, a.class, a.state, a.usage.last_active_tick, a.name.clone()))
            .collect()
    };

    for (id, class, state, last_active, name) in snapshot {
        match state {
            AgentState::Failed => {
                match class {
                    AgentClass::Matriarch | AgentClass::SubMatriarch | AgentClass::Daemon => {
                        crate::serial_println!("[SUPERVISOR] #{} '{}' failed, restarting", id, name);
                        let mut registry = AGENT_REGISTRY.lock();
                        if let Some(agent) = registry.get_mut(&id) {
                            agent.state = AgentState::Created;
                            agent.usage.restart_count += 1;
                            for tool in agent.tools.iter_mut() { tool.calls_used = 0; }
                        }
                        drop(registry);
                        let _ = start_agent(id);
                        actions.push(SupervisorAction::Restarted(id));
                    }
                    AgentClass::Worker => {
                        let _ = terminate_agent(id);
                        actions.push(SupervisorAction::Cleaned(id));
                    }
                }
            }
            AgentState::Completed if class == AgentClass::Worker => {
                let _ = terminate_agent(id);
                actions.push(SupervisorAction::Cleaned(id));
            }
            AgentState::Running => {
                let idle = tick.saturating_sub(last_active);
                let timeout = match class {
                    AgentClass::Worker       => 5_000,
                    AgentClass::SubMatriarch => 30_000,
                    _ => u64::MAX,
                };
                if idle > timeout {
                    crate::serial_println!("[SUPERVISOR] #{} '{}' stuck ({} ticks idle)", id, name, idle);
                    let mut registry = AGENT_REGISTRY.lock();
                    if let Some(agent) = registry.get_mut(&id) {
                        agent.state = AgentState::Failed;
                    }
                    actions.push(SupervisorAction::MarkedFailed(id));
                }
            }
            _ => {}
        }
    }
    actions
}

// ============================================================================
// QUERY API
// ============================================================================

pub fn get_agent(id: AgentId) -> Option<AgentDescriptor> {
    AGENT_REGISTRY.lock().get(&id).cloned()
}

pub fn get_agent_by_pid(pid: u64) -> Option<AgentDescriptor> {
    let agent_id = PID_TO_AGENT.lock().get(&pid).copied()?;
    AGENT_REGISTRY.lock().get(&agent_id).cloned()
}

pub fn list_agents() -> Vec<(AgentId, String, AgentClass, AgentState)> {
    AGENT_REGISTRY.lock().values()
        .filter(|a| a.state.is_alive())
        .map(|a| (a.id, a.name.clone(), a.class, a.state))
        .collect()
}

pub fn count_by_class() -> (u32, u32, u32, u32) {
    let registry = AGENT_REGISTRY.lock();
    let (mut m, mut s, mut w, mut d) = (0u32, 0u32, 0u32, 0u32);
    for a in registry.values() {
        if !a.state.is_alive() { continue; }
        match a.class {
            AgentClass::Matriarch    => m += 1,
            AgentClass::SubMatriarch => s += 1,
            AgentClass::Worker       => w += 1,
            AgentClass::Daemon       => d += 1,
        }
    }
    (m, s, w, d)
}

/// Update last_active_tick (called during inference/tool execution).
pub fn touch_agent(id: AgentId) {
    let tick = CURRENT_TICK.load(Ordering::Relaxed);
    let mut registry = AGENT_REGISTRY.lock();
    if let Some(agent) = registry.get_mut(&id) {
        agent.usage.last_active_tick = tick;
    }
}

/// Record tokens generated by an agent.
pub fn record_tokens(id: AgentId, prompt_tokens: u64, gen_tokens: u64) {
    let mut registry = AGENT_REGISTRY.lock();
    if let Some(agent) = registry.get_mut(&id) {
        agent.usage.tokens_prompted = agent.usage.tokens_prompted.saturating_add(prompt_tokens);
        agent.usage.tokens_generated = agent.usage.tokens_generated.saturating_add(gen_tokens);
        agent.usage.requests_completed += 1;
        let total = agent.usage.tokens_generated + agent.usage.tokens_prompted;
        if total >= agent.limits.max_tokens_total && agent.class == AgentClass::Worker {
            agent.state = AgentState::Completed;
            crate::serial_println!(
                "[AGENT-RT] Worker #{} '{}' hit token limit ({})",
                id, agent.name, total
            );
        }
    }
}

/// Record tool call for permission tracking.
pub fn record_tool_call(id: AgentId, tool_name: &str) -> Result<(), AgentError> {
    let mut registry = AGENT_REGISTRY.lock();
    let agent = registry.get_mut(&id).ok_or(AgentError::NotFound)?;
    let tool = agent.tools.iter_mut().find(|t| t.tool_name == tool_name);
    match tool {
        Some(t) if t.allowed => {
            if t.max_calls_per_request > 0 && t.calls_used >= t.max_calls_per_request {
                return Err(AgentError::LimitExceeded);
            }
            t.calls_used += 1;
            agent.usage.tool_calls_executed += 1;
            Ok(())
        }
        Some(_) => Err(AgentError::ToolDenied),
        None => {
            if agent.capabilities & (AgentCapability::ToolCalling as u32) != 0 {
                agent.usage.tool_calls_executed += 1;
                Ok(())
            } else {
                Err(AgentError::ToolDenied)
            }
        }
    }
}

/// Bind model to agent.
pub fn bind_model(id: AgentId, model_idx: u16) -> Result<(), AgentError> {
    let mut registry = AGENT_REGISTRY.lock();
    let agent = registry.get_mut(&id).ok_or(AgentError::NotFound)?;
    if agent.capabilities & (AgentCapability::Inference as u32) == 0 {
        return Err(AgentError::CapabilityDenied);
    }
    agent.model_idx = Some(model_idx);
    crate::serial_println!("[AGENT-RT] Bound model {} to #{} '{}'", model_idx, id, agent.name);
    Ok(())
}

/// Bind persistent memory session.
pub fn bind_memory(id: AgentId, session_id: u64) -> Result<(), AgentError> {
    let mut registry = AGENT_REGISTRY.lock();
    let agent = registry.get_mut(&id).ok_or(AgentError::NotFound)?;
    agent.memory_session = Some(session_id);
    Ok(())
}

/// Bind KV cache session.
pub fn bind_kv_cache(id: AgentId, session_id: u64) -> Result<(), AgentError> {
    let mut registry = AGENT_REGISTRY.lock();
    let agent = registry.get_mut(&id).ok_or(AgentError::NotFound)?;
    agent.kv_session = Some(session_id);
    Ok(())
}

// ============================================================================
// DIAGNOSTICS
// ============================================================================

/// Print full status to serial.
pub fn print_status() {
    let (m, s, w, d) = count_by_class();
    let spawned = AGENTS_SPAWNED.load(Ordering::Relaxed);
    let terminated = AGENTS_TERMINATED.load(Ordering::Relaxed);
    crate::serial_println!("[AGENT-RT STATUS]");
    crate::serial_println!(
        "  {} alive ({}M+{}S+{}W+{}D) | spawned={} terminated={}",
        m+s+w+d, m, s, w, d, spawned, terminated
    );
    let registry = AGENT_REGISTRY.lock();
    for agent in registry.values() {
        if !agent.state.is_alive() { continue; }
        crate::serial_println!(
            "  #{:<3} {:?:<12} {:?:<10} '{}' model={:?} tok={}+{} tools={} children={}",
            agent.id, agent.class, agent.state, agent.name,
            agent.model_idx, agent.usage.tokens_prompted, agent.usage.tokens_generated,
            agent.usage.tool_calls_executed, agent.children.len(),
        );
    }
}
