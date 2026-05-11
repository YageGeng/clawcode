//! Type conversions from clawcode internal types to ACP schema types.
//!
//! All conversions use the `From` trait with move semantics.
//! Since both `protocol` types and the ACP schema types are
//! foreign to the acp crate, these impls live here where the
//! protocol types are local (satisfying the orphan rule).

use acp::schema;
use agent_client_protocol as acp;

use crate::event::StopReason;
use crate::permission::PermissionOptionKind;
use crate::plan::{PlanPriority, PlanStatus};
use crate::tool::ToolCallStatus;

// ── StopReason ──

impl From<StopReason> for schema::StopReason {
    fn from(r: StopReason) -> Self {
        match r {
            StopReason::EndTurn => Self::EndTurn,
            StopReason::Cancelled => Self::Cancelled,
            // ACP has no Error variant; map to Cancelled.
            StopReason::Error => Self::Cancelled,
        }
    }
}

// ── ToolCallStatus ──

impl From<ToolCallStatus> for schema::ToolCallStatus {
    fn from(s: ToolCallStatus) -> Self {
        match s {
            ToolCallStatus::Pending => Self::Pending,
            ToolCallStatus::InProgress => Self::InProgress,
            ToolCallStatus::Completed => Self::Completed,
            ToolCallStatus::Failed => Self::Failed,
        }
    }
}

// ── PlanPriority ──

impl From<PlanPriority> for schema::PlanEntryPriority {
    fn from(p: PlanPriority) -> Self {
        match p {
            PlanPriority::Low => Self::Low,
            PlanPriority::Medium => Self::Medium,
            PlanPriority::High => Self::High,
        }
    }
}

// ── PlanStatus ──

impl From<PlanStatus> for schema::PlanEntryStatus {
    fn from(s: PlanStatus) -> Self {
        match s {
            PlanStatus::Pending => Self::Pending,
            PlanStatus::InProgress => Self::InProgress,
            PlanStatus::Completed => Self::Completed,
        }
    }
}

// ── PermissionOptionKind ──

impl From<PermissionOptionKind> for schema::PermissionOptionKind {
    fn from(k: PermissionOptionKind) -> Self {
        match k {
            PermissionOptionKind::AllowOnce => Self::AllowOnce,
            PermissionOptionKind::AllowAlways => Self::AllowAlways,
            PermissionOptionKind::RejectOnce => Self::RejectOnce,
            PermissionOptionKind::RejectAlways => Self::RejectAlways,
        }
    }
}
