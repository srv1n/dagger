//! Immutable budget ledger types.

use crate::ids::{CostUnits, Id, NodeInstanceId, Timestamp};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};

/// The closed budget-ledger operation vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BudgetLedgerKind {
    /// Add one live reservation.
    Reserve,
    /// Settle with trusted actual cost.
    SettleActual,
    /// Settle the full reservation without trusted actual cost.
    SettleFullUnknown,
}

/// The closed budget-ledger reason vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BudgetLedgerReason {
    /// Attempt started.
    Started,
    /// Attempt succeeded.
    Succeeded,
    /// Retryable result.
    Retryable,
    /// Permanent result.
    Permanent,
    /// Contract result.
    Contract,
    /// Deadline timeout.
    TimedOut,
    /// Crash-unknown result.
    UnknownOutcome,
    /// Run cancellation.
    Cancelled,
    /// Live completion lost its fence.
    Stale,
}

/// One immutable reservation or settlement ledger entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BudgetLedgerEntry {
    /// Ledger scope.
    pub scope: ExecutionScope,
    /// Owning run ID.
    pub run_id: Id,
    /// Run-local ledger sequence.
    pub ledger_seq: u64,
    /// Correlated attempt ID.
    pub attempt_id: Id,
    /// Correlated node instance ID.
    pub node_instance_id: NodeInstanceId,
    /// Closed ledger operation.
    pub kind: BudgetLedgerKind,
    /// Signed reserved-cost change.
    pub reserved_delta: i128,
    /// Consumed-cost increase.
    pub consumed_delta: CostUnits,
    /// Original reservation amount.
    pub reservation_amount: CostUnits,
    /// Closed accounting reason.
    pub reason: BudgetLedgerReason,
    /// Database creation timestamp.
    pub created_at: Timestamp,
}

/// Inputs to an atomic pre-invocation reservation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetReservation {
    /// Declared maximum to reserve.
    pub declared_max: CostUnits,
}

/// Inputs to an accepted trusted-cost settlement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetSettlement {
    /// Immutable original reservation.
    pub reservation: CostUnits,
    /// Trusted actual cost or full reservation.
    pub consumed: CostUnits,
    /// Closed settlement reason.
    pub reason: BudgetLedgerReason,
}
