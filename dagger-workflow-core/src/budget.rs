//! Immutable budget ledger types from contract sections 1.15 and 11.

use crate::ids::{CostUnits, Id, NodeInstanceId, Timestamp};
use crate::scope::ExecutionScope;
use serde::{Deserialize, Serialize};

/// The closed budget-ledger operation vocabulary. Contract section 1.15.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BudgetLedgerKind {
    /// Add one live reservation. Contract section 11.1.
    Reserve,
    /// Settle with trusted actual cost. Contract section 11.2.
    SettleActual,
    /// Settle the full reservation without trusted actual cost. Contract section 11.2.
    SettleFullUnknown,
}

/// The closed budget-ledger reason vocabulary. Contract section 1.15.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BudgetLedgerReason {
    /// Attempt started. Contract section 1.15.
    Started,
    /// Attempt succeeded. Contract section 1.15.
    Succeeded,
    /// Retryable result. Contract section 1.15.
    Retryable,
    /// Permanent result. Contract section 1.15.
    Permanent,
    /// Contract result. Contract section 1.15.
    Contract,
    /// Deadline timeout. Contract section 1.15.
    TimedOut,
    /// Crash-unknown result. Contract section 1.15.
    UnknownOutcome,
    /// Run cancellation. Contract section 1.15.
    Cancelled,
    /// Live completion lost its fence. Contract section 1.15.
    Stale,
}

/// One immutable reservation or settlement ledger entry. Contract section 1.15.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BudgetLedgerEntry {
    /// Ledger scope. Contract section 1.15.
    pub scope: ExecutionScope,
    /// Owning run ID. Contract section 1.15.
    pub run_id: Id,
    /// Run-local ledger sequence. Contract section 1.15.
    pub ledger_seq: u64,
    /// Correlated attempt ID. Contract section 1.15.
    pub attempt_id: Id,
    /// Correlated node instance ID. Contract section 1.15.
    pub node_instance_id: NodeInstanceId,
    /// Closed ledger operation. Contract section 1.15.
    pub kind: BudgetLedgerKind,
    /// Signed reserved-cost change. Contract section 1.15.
    pub reserved_delta: i128,
    /// Consumed-cost increase. Contract section 1.15.
    pub consumed_delta: CostUnits,
    /// Original reservation amount. Contract section 1.15.
    pub reservation_amount: CostUnits,
    /// Closed accounting reason. Contract section 1.15.
    pub reason: BudgetLedgerReason,
    /// Database creation timestamp. Contract section 1.15.
    pub created_at: Timestamp,
}

/// Inputs to an atomic pre-invocation reservation. Contract section 11.1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetReservation {
    /// Declared maximum to reserve. Contract section 11.1.
    pub declared_max: CostUnits,
}

/// Inputs to an accepted trusted-cost settlement. Contract section 11.2.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BudgetSettlement {
    /// Immutable original reservation. Contract section 11.2.
    pub reservation: CostUnits,
    /// Trusted actual cost or full reservation. Contract section 11.2.
    pub consumed: CostUnits,
    /// Closed settlement reason. Contract section 11.2.
    pub reason: BudgetLedgerReason,
}
