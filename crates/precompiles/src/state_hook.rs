//! State hook infrastructure for precompiles.
//!
//! Precompiles run inside revm but need access to protocol state (AccountState,
//! AssetRegistry, etc.). Since `Block::execute` already holds write locks on all
//! state via `StateWriteBundle`, precompiles must NOT acquire their own locks.
//!
//! This module uses thread-local scoped references:
//! 1. `StateHookGuard::new()` stores raw pointers to all protocol state in TLS
//! 2. Precompiles access state through safe accessor functions
//! 3. `StateHookGuard::drop()` clears TLS to prevent leakage between blocks
//!
//! # Safety
//! The raw pointers are only valid during `Block::execute`. The guard's `Drop`
//! impl clears them even during panic unwinding. Thread-local storage prevents
//! cross-thread issues.

use std::cell::RefCell;

use call_governance::GovernanceManager;
use call_oracle::OracleManager;
use call_protocol::{AccountState, AssetRegistry};
use call_protocol::compliance::ComplianceEngine;
use call_shielded::ShieldedState;

// ── Thread-local state references ─────────────────────────────────────

thread_local! {
    static TL_STATE: RefCell<Option<ExecutionStateRef>> = RefCell::new(None);
}

/// Scoped references to all protocol state. Only valid while a
/// `StateHookGuard` is alive in the same thread.
#[derive(Clone, Copy)]
struct ExecutionStateRef {
    account: *mut AccountState,
    registry: *mut AssetRegistry,
    compliance: *mut ComplianceEngine,
    shielded_state: *mut ShieldedState,
    oracle: *mut OracleManager,
    governance: *mut GovernanceManager,
}

// Safety: ExecutionStateRef is !Send and !Sync because it contains raw
// pointers, and we only use it via thread-local storage.

/// Guard that injects protocol state into thread-local storage for precompiles.
///
/// Create this at the start of `Block::execute` and let it drop at the end.
/// All precompile state access goes through the safe `with_*` functions below.
pub struct StateHookGuard;

impl StateHookGuard {
    /// Inject all protocol state into the precompile hook layer.
    ///
    /// # Safety
    /// The caller must ensure that all passed references outlive this guard.
    /// This is naturally guaranteed when called from `Block::execute`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        account: &mut AccountState,
        registry: &mut AssetRegistry,
        compliance: &mut ComplianceEngine,
        shielded_state: &mut ShieldedState,
        oracle: Option<&mut OracleManager>,
        governance: Option<&mut GovernanceManager>,
    ) -> Self {
        let refs = ExecutionStateRef {
            account,
            registry,
            compliance,
            shielded_state,
            oracle: oracle.map_or(std::ptr::null_mut(), |r| r),
            governance: governance.map_or(std::ptr::null_mut(), |r| r),
        };
        TL_STATE.with(|t| *t.borrow_mut() = Some(refs));
        Self
    }
}

impl StateHookGuard {
    /// Create a guard from raw pointers. Useful when the caller already
    /// holds mutable references but cannot pass them due to borrow-checker
    /// constraints (e.g. inside `Block::execute` where `subsystems` is
    /// accessed both before and after guard creation).
    ///
    /// # Safety
    /// All pointers must be valid for the lifetime of the returned guard.
    /// Null pointers for optional state (oracle, governance) are allowed.
    pub unsafe fn from_raw(
        account: *mut AccountState,
        registry: *mut AssetRegistry,
        compliance: *mut ComplianceEngine,
        shielded_state: *mut ShieldedState,
        oracle: *mut OracleManager,
        governance: *mut GovernanceManager,
    ) -> Self {
        let refs = ExecutionStateRef {
            account,
            registry,
            compliance,
            shielded_state,
            oracle,
            governance,
        };
        TL_STATE.with(|t| *t.borrow_mut() = Some(refs));
        Self
    }
}

impl Drop for StateHookGuard {
    fn drop(&mut self) {
        TL_STATE.with(|t| *t.borrow_mut() = None);
    }
}

// ── Safe accessors (used by precompiles) ──────────────────────────────

/// Access the account state (if hooked).
///
/// # Safety
/// The closure receives `&mut AccountState`. The caller must not panic while
/// holding this reference, and must not store it beyond the closure body.
pub fn with_account_state<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut AccountState) -> R,
{
    TL_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref().map(|r| {
            // SAFETY: The pointer is valid because StateHookGuard is alive
            // and was constructed with a reference that outlives the guard.
            unsafe { f(&mut *r.account) }
        })
    })
}

/// Access the asset registry (if hooked).
pub fn with_registry<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut AssetRegistry) -> R,
{
    TL_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref().map(|r| unsafe { f(&mut *r.registry) })
    })
}

/// Access the compliance engine (if hooked).
pub fn with_compliance<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut ComplianceEngine) -> R,
{
    TL_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref().map(|r| unsafe { f(&mut *r.compliance) })
    })
}

/// Access the shielded state (if hooked).
pub fn with_shielded_state<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut ShieldedState) -> R,
{
    TL_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref()
            .filter(|r| !r.shielded_state.is_null())
            .map(|r| unsafe { f(&mut *r.shielded_state) })
    })
}

/// Access the oracle manager (if hooked).
pub fn with_oracle<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut OracleManager) -> R,
{
    TL_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref()
            .filter(|r| !r.oracle.is_null())
            .map(|r| unsafe { f(&mut *r.oracle) })
    })
}

/// Access the governance manager (if hooked).
pub fn with_governance<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut GovernanceManager) -> R,
{
    TL_STATE.with(|t| {
        let refs = t.borrow();
        refs.as_ref()
            .filter(|r| !r.governance.is_null())
            .map(|r| unsafe { f(&mut *r.governance) })
    })
}

/// Check if the state hook is currently active (for testing/diagnostics).
pub fn is_hook_active() -> bool {
    TL_STATE.with(|t| t.borrow().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_hook_guard_lifecycle() {
        let mut account = AccountState::default();
        let mut registry = AssetRegistry::default();
        let mut compliance = ComplianceEngine::default();
        let mut shielded = ShieldedState::new();
        let mut oracle = OracleManager::default();
        let mut gov = GovernanceManager::default();

        assert!(!is_hook_active());

        {
            let _guard = StateHookGuard::new(
                &mut account,
                &mut registry,
                &mut compliance,
                &mut shielded,
                Some(&mut oracle),
                Some(&mut gov),
            );

            assert!(is_hook_active());

            // Account state access
            let result = with_account_state(|acc| {
                acc.credit_balance(0, call_primitives::Address::ZERO, 100).unwrap();
                acc.get_balance(0, &call_primitives::Address::ZERO)
            });
            assert_eq!(result, Some(100));

            // Oracle access
            let result = with_oracle(|o| o.config.staleness_secs);
            assert_eq!(result, Some(900));
        }

        assert!(!is_hook_active());

        // After guard drops, access returns None
        let result = with_account_state(|acc| acc.get_balance(0, &call_primitives::Address::ZERO));
        assert_eq!(result, None);
    }
}
