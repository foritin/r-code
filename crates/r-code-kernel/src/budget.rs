//! Root-owned budgets shared across a task tree.
//!
//! The root task owns the budget; children draw from it. Exhaustion is a
//! first-class terminal cause preserving partial results; progress and
//! liveness are separate concerns.

use serde::{Deserialize, Serialize};

/// A shared budget pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BudgetPool {
    /// Total units granted to the root.
    pub total: u64,
    /// Units already consumed by the whole tree.
    pub consumed: u64,
}

/// Budget errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BudgetError {
    #[error("budget exhausted: {consumed}/{total} units consumed; refusing {requested} more")]
    Exhausted {
        consumed: u64,
        total: u64,
        requested: u64,
    },
}

impl BudgetPool {
    pub fn new(total: u64) -> Self {
        Self { total, consumed: 0 }
    }

    pub fn remaining(&self) -> u64 {
        self.total.saturating_sub(self.consumed)
    }

    pub fn is_exhausted(&self) -> bool {
        self.remaining() == 0
    }

    /// Consume units for any tree member; refusal is terminal.
    pub fn consume(&mut self, requested: u64) -> Result<(), BudgetError> {
        if self.remaining() < requested {
            return Err(BudgetError::Exhausted {
                consumed: self.consumed,
                total: self.total,
                requested,
            });
        }
        self.consumed += requested;
        Ok(())
    }

    /// Refund units (e.g. a cancelled child's reservation returns).
    pub fn refund(&mut self, units: u64) {
        self.consumed = self.consumed.saturating_sub(units);
    }
}

/// A per-child reservation handle: dropping it refunds unconsumed units.
pub struct BudgetReservation<'a> {
    pool: &'a mut BudgetPool,
    units: u64,
    spent: u64,
}

impl<'a> BudgetReservation<'a> {
    /// Reserve up to `units` for a child (bounded by remaining).
    pub fn reserve(pool: &'a mut BudgetPool, units: u64) -> Self {
        let units = units.min(pool.remaining());
        pool.consume(units).ok(); // bounded above; cannot fail
        Self {
            pool,
            units,
            spent: 0,
        }
    }

    /// Spend part of the reservation.
    pub fn spend(&mut self, units: u64) -> Result<(), BudgetError> {
        if self.spent + units > self.units {
            return Err(BudgetError::Exhausted {
                consumed: self.pool.consumed,
                total: self.pool.total,
                requested: units,
            });
        }
        self.spent += units;
        Ok(())
    }
}

impl Drop for BudgetReservation<'_> {
    fn drop(&mut self) {
        // Unspent reservation returns to the pool.
        let unspent = self.units - self.spent;
        self.pool.refund(unspent);
    }
}
