//! Serde support for durable entities whose public API intentionally omits it.

use crate::action::ActionInvocation;
use crate::approval::{
    ApprovalExpiryPolicy, ApprovalGate, ApprovalResolutionSource, DecisionAuthorizationPolicy,
};
use crate::artifact::{ArtifactKind, ArtifactRef, JsonRef, ObjectRecord};
use crate::budget::{BudgetLedgerEntry, BudgetLedgerKind, BudgetLedgerReason};
use crate::definition::{ActionPin, PublishableDefinition, WorkflowDefinition};
use crate::ids::{CostUnits, Digest, Id, NodeInstanceId, Timestamp, TopologicalRank, Version};
use crate::revision::WorkflowRevision;
use crate::run::{
    AttemptErrorClass, AttemptState, BlockedFromState, ChoiceSelection, EdgeFact, EdgeKind,
    EdgeState, GateState, NodeAttempt, NodeFailureKind, NodeKind, NodeRun, RunFailureKind,
    RunLimits, RunState, WorkflowRun,
};
use crate::scope::ExecutionScope;
use crate::store::{
    CommandKind, CommandReceipt, CommandReceiptOutcome, DefinitionRecord, EngineClaim,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

macro_rules! serde_public_struct {
    ($module:ident, $type:path, { $($field:ident : $field_type:ty),* $(,)? }) => {
        mod $module {
            use super::*;

            #[derive(Deserialize, Serialize)]
            pub(super) struct Repr {
                $(pub(super) $field: $field_type),*
            }
        }

        impl Serialize for $type {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $module::Repr {
                    $($field: self.$field.clone()),*
                }
                .serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = $module::Repr::deserialize(deserializer)?;
                Ok(Self {
                    $($field: value.$field),*
                })
            }
        }
    };
}

serde_public_struct!(object_record, ObjectRecord, {
    scope: ExecutionScope,
    digest: Digest,
    size_bytes: u64,
    object_key: String,
    created_at: Timestamp,
});

serde_public_struct!(artifact_ref, ArtifactRef, {
    scope: ExecutionScope,
    artifact_ref_id: Id,
    digest: Digest,
    size_bytes: u64,
    media_type: String,
    kind: ArtifactKind,
    producer_run_id: Option<Id>,
    producer_node_id: Option<NodeInstanceId>,
    producer_attempt_id: Option<Id>,
    ordinal: u32,
    created_at: Timestamp,
});

impl Serialize for JsonRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        ArtifactRef::deserialize(deserializer).map(Self)
    }
}

serde_public_struct!(definition_record, DefinitionRecord, {
    scope: ExecutionScope,
    definition_id: Id,
    display_name: String,
    description: String,
    created_at: Timestamp,
    created_by: String,
    latest_revision_hash: Option<Digest>,
    version: Version,
});

serde_public_struct!(action_pin, ActionPin, {
    reference_location: String,
    name: String,
    contract_version: String,
    input_schema_digest: Digest,
    output_schema_digest: Digest,
    compatible_implementation_requirement: Digest,
    input_schema_ref: JsonRef,
    output_schema_ref: JsonRef,
});

serde_public_struct!(publishable_definition, PublishableDefinition, {
    definition: WorkflowDefinition,
    topological_ranks: BTreeMap<Id, TopologicalRank>,
});

serde_public_struct!(workflow_revision, WorkflowRevision, {
    scope: ExecutionScope,
    definition_id: Id,
    revision_hash: Digest,
    definition_format_version: String,
    canonical_definition_ref: JsonRef,
    run_input_schema_ref: JsonRef,
    run_output_schema_ref: JsonRef,
    run_input_schema_digest: Digest,
    run_output_schema_digest: Digest,
    root_node_ids: Vec<Id>,
    node_count: u32,
    node_topological_ranks: BTreeMap<Id, TopologicalRank>,
    action_pins: Vec<ActionPin>,
    published_at: Timestamp,
    published_by: String,
});

serde_public_struct!(engine_claim, EngineClaim, {
    scope: ExecutionScope,
    control_plane_id: String,
    instance_id: Id,
    generation: u64,
    claimed_at: Timestamp,
    heartbeat_at: Timestamp,
    expires_at: Timestamp,
    version: Version,
});

serde_public_struct!(workflow_run, WorkflowRun, {
    scope: ExecutionScope,
    run_id: Id,
    definition_id: Id,
    revision_hash: Digest,
    input_ref: JsonRef,
    create_request_fingerprint: Digest,
    status: RunState,
    failure_kind: Option<RunFailureKind>,
    failure_diagnostics_ref: Option<JsonRef>,
    output_ref: Option<JsonRef>,
    budget_limit: CostUnits,
    budget_consumed: CostUnits,
    budget_reserved: CostUnits,
    dynamic_node_count: u64,
    total_attempt_count: u64,
    aggregate_object_bytes: u64,
    limits: RunLimits,
    lifetime_deadline_at: Timestamp,
    frontier_epoch: u64,
    last_event_seq: u64,
    created_at: Timestamp,
    updated_at: Timestamp,
    started_at: Option<Timestamp>,
    finished_at: Option<Timestamp>,
    blocked_incompatibilities_ref: Option<JsonRef>,
    blocked_incompatibility_fingerprint: Option<Digest>,
    corrupt_bad_artifact_ref_id: Option<Id>,
    corrupt_owner_node_id: Option<NodeInstanceId>,
    corrupt_error_class: Option<crate::artifact::FailedReadClass>,
    corrupt_proof_fingerprint: Option<Digest>,
    version: Version,
});

serde_public_struct!(node_run, NodeRun, {
    scope: ExecutionScope,
    run_id: Id,
    node_instance_id: NodeInstanceId,
    definition_node_id: Id,
    kind: NodeKind,
    parent_map_instance_id: Option<NodeInstanceId>,
    map_item_index: Option<u32>,
    map_item_digest: Option<Digest>,
    topological_rank: TopologicalRank,
    status: crate::run::NodeState,
    blocked_from_status: Option<BlockedFromState>,
    active_attempt_id: Option<Id>,
    attempt_count: u32,
    next_eligible_at: Option<Timestamp>,
    budget_wait_amount: Option<CostUnits>,
    result_ref: Option<JsonRef>,
    failure_kind: Option<NodeFailureKind>,
    failure_diagnostics_ref: Option<JsonRef>,
    incoming_total: u32,
    incoming_satisfied: u32,
    incoming_skipped: u32,
    skip_reason: Option<crate::run::SkipReason>,
    choice_input_ref: Option<JsonRef>,
    choice_selected_case: Option<ChoiceSelection>,
    map_input_ref: Option<JsonRef>,
    map_expansion_digest: Option<Digest>,
    map_child_count: Option<u32>,
    approval_gate_id: Option<Id>,
    created_at: Timestamp,
    updated_at: Timestamp,
    version: Version,
});

serde_public_struct!(edge_fact, EdgeFact, {
    scope: ExecutionScope,
    run_id: Id,
    edge_id: Id,
    from_node_id: NodeInstanceId,
    to_node_id: NodeInstanceId,
    choice_case_index: Option<u32>,
    kind: EdgeKind,
    state: EdgeState,
    skip_reason: Option<crate::run::SkipReason>,
    resolved_at: Option<Timestamp>,
    version: Version,
});

serde_public_struct!(node_attempt, NodeAttempt, {
    scope: ExecutionScope,
    run_id: Id,
    attempt_id: Id,
    node_instance_id: NodeInstanceId,
    attempt_number: u32,
    worker_id: Id,
    engine_instance_id: Id,
    engine_generation: u64,
    completion_credential_digest: Digest,
    invocation_id: Id,
    idempotency_key: String,
    status: AttemptState,
    declared_max_cost: CostUnits,
    reserved_cost: CostUnits,
    settled_cost: Option<CostUnits>,
    deadline_at: Timestamp,
    started_at: Timestamp,
    finished_at: Option<Timestamp>,
    output_ref: Option<JsonRef>,
    artifact_refs: Vec<ArtifactRef>,
    error_class: Option<AttemptErrorClass>,
    error_code: Option<String>,
    diagnostics_ref: Option<JsonRef>,
});

serde_public_struct!(action_invocation, ActionInvocation, {
    scope: ExecutionScope,
    run_id: Id,
    invocation_id: Id,
    node_instance_id: NodeInstanceId,
    attempt_id: Id,
    action_reference_location: String,
    action_name: String,
    contract_version: String,
    revision_hash: Digest,
    input_schema_digest: Digest,
    output_schema_digest: Digest,
    compatible_implementation_requirement: Digest,
    bound_input_ref: JsonRef,
    bound_input_digest: Digest,
    bound_input_size_bytes: u64,
    binding_derivation_digest: Digest,
    created_at: Timestamp,
});

serde_public_struct!(approval_gate, ApprovalGate, {
    scope: ExecutionScope,
    run_id: Id,
    gate_id: Id,
    node_instance_id: NodeInstanceId,
    request_ref: JsonRef,
    status: GateState,
    expires_at: Timestamp,
    on_expiry: ApprovalExpiryPolicy,
    authorization_policy: DecisionAuthorizationPolicy,
    decision_payload_ref: Option<JsonRef>,
    deciding_principal: Option<String>,
    resolution_source: Option<ApprovalResolutionSource>,
    decided_at: Option<Timestamp>,
    decision_fingerprint: Option<Digest>,
    version: Version,
});

serde_public_struct!(budget_ledger_entry, BudgetLedgerEntry, {
    scope: ExecutionScope,
    run_id: Id,
    ledger_seq: u64,
    attempt_id: Id,
    node_instance_id: NodeInstanceId,
    kind: BudgetLedgerKind,
    reserved_delta: i128,
    consumed_delta: CostUnits,
    reservation_amount: CostUnits,
    reason: BudgetLedgerReason,
    created_at: Timestamp,
});

#[derive(Deserialize, Serialize)]
enum CommandKindRepr {
    CreateRun,
    CancelRun,
}

impl Serialize for CommandKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::CreateRun => CommandKindRepr::CreateRun,
            Self::CancelRun => CommandKindRepr::CancelRun,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CommandKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match CommandKindRepr::deserialize(deserializer)? {
            CommandKindRepr::CreateRun => Self::CreateRun,
            CommandKindRepr::CancelRun => Self::CancelRun,
        })
    }
}

#[derive(Deserialize, Serialize)]
enum CommandReceiptOutcomeRepr {
    CreateRunCommitted {
        run_id: Id,
        status: RunState,
        run_version: Version,
        batch_id: Id,
        first_event_seq: u64,
        last_event_seq: u64,
    },
    CancelRunCommitted {
        run_id: Id,
        prior_status: RunState,
        status: RunState,
        run_version: Version,
        batch_id: Id,
        first_event_seq: u64,
        last_event_seq: u64,
    },
}

impl From<CommandReceiptOutcome> for CommandReceiptOutcomeRepr {
    fn from(value: CommandReceiptOutcome) -> Self {
        match value {
            CommandReceiptOutcome::CreateRunCommitted {
                run_id,
                status,
                run_version,
                batch_id,
                first_event_seq,
                last_event_seq,
            } => Self::CreateRunCommitted {
                run_id,
                status,
                run_version,
                batch_id,
                first_event_seq,
                last_event_seq,
            },
            CommandReceiptOutcome::CancelRunCommitted {
                run_id,
                prior_status,
                status,
                run_version,
                batch_id,
                first_event_seq,
                last_event_seq,
            } => Self::CancelRunCommitted {
                run_id,
                prior_status,
                status,
                run_version,
                batch_id,
                first_event_seq,
                last_event_seq,
            },
        }
    }
}

impl From<CommandReceiptOutcomeRepr> for CommandReceiptOutcome {
    fn from(value: CommandReceiptOutcomeRepr) -> Self {
        match value {
            CommandReceiptOutcomeRepr::CreateRunCommitted {
                run_id,
                status,
                run_version,
                batch_id,
                first_event_seq,
                last_event_seq,
            } => Self::CreateRunCommitted {
                run_id,
                status,
                run_version,
                batch_id,
                first_event_seq,
                last_event_seq,
            },
            CommandReceiptOutcomeRepr::CancelRunCommitted {
                run_id,
                prior_status,
                status,
                run_version,
                batch_id,
                first_event_seq,
                last_event_seq,
            } => Self::CancelRunCommitted {
                run_id,
                prior_status,
                status,
                run_version,
                batch_id,
                first_event_seq,
                last_event_seq,
            },
        }
    }
}

impl Serialize for CommandReceiptOutcome {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        CommandReceiptOutcomeRepr::from(self.clone()).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CommandReceiptOutcome {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(CommandReceiptOutcomeRepr::deserialize(deserializer)?.into())
    }
}

serde_public_struct!(command_receipt, CommandReceipt, {
    scope: ExecutionScope,
    command_kind: CommandKind,
    idempotency_token: String,
    request_fingerprint: Digest,
    run_id: Id,
    outcome: CommandReceiptOutcome,
    batch_id: Id,
    committed_at: Timestamp,
});
