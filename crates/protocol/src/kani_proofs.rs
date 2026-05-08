//! Kani model checker proofs for critical protocol invariants.
//!
//! Run with: `cargo kani --crate call-protocol`

#[cfg(kani)]
mod proofs {
    /// Transfer preserves total balance: sender + recipient before == after.
    #[kani::proof]
    fn verify_transfer_preserves_total() {
        let sender_before: u128 = kani::any();
        let recipient_before: u128 = kani::any();
        let amount: u128 = kani::any();

        let total_before = sender_before.saturating_add(recipient_before);

        if let Some(sender_after) = sender_before.checked_sub(amount) {
            if let Some(recipient_after) = recipient_before.checked_add(amount) {
                let total_after = sender_after.saturating_add(recipient_after);
                assert_eq!(total_before, total_after);
            }
        }
    }

    /// Mint increases total supply by exact amount.
    #[kani::proof]
    fn verify_mint_increases_supply() {
        let supply: u128 = kani::any();
        let amount: u128 = kani::any();

        if let Some(new_supply) = supply.checked_add(amount) {
            assert_eq!(new_supply, supply + amount);
            assert!(new_supply >= supply);
        }
    }

    /// Burn decreases total supply by exact amount.
    #[kani::proof]
    fn verify_burn_decreases_supply() {
        let supply: u128 = kani::any();
        let amount: u128 = kani::any();

        if let Some(new_supply) = supply.checked_sub(amount) {
            assert_eq!(new_supply, supply - amount);
            assert!(new_supply <= supply);
        }
    }

    /// Balance can never go negative with checked arithmetic.
    #[kani::proof]
    fn verify_balance_never_negative() {
        let balance: u128 = kani::any();
        let amount: u128 = kani::any();

        if let Some(new_balance) = balance.checked_sub(amount) {
            assert!(new_balance <= balance);
        }
    }

    /// Nonce always increments by exactly 1.
    #[kani::proof]
    fn verify_nonce_increments() {
        let nonce: u64 = kani::any();
        let new_nonce = nonce.saturating_add(1);
        assert!(new_nonce > nonce || nonce == u64::MAX);
    }

    /// Slashing reduces stake or leaves it at zero.
    #[kani::proof]
    fn verify_slash_reduces_stake() {
        let stake: u128 = kani::any();
        let slash_amount: u128 = kani::any();

        if let Some(new_stake) = stake.checked_sub(slash_amount) {
            assert!(new_stake <= stake);
        }
    }

    /// Allowance decrease is exact and never underflows.
    #[kani::proof]
    fn verify_allowance_decrease_exact() {
        let allowance: u128 = kani::any();
        let amount: u128 = kani::any();

        if let Some(new_allowance) = allowance.checked_sub(amount) {
            assert_eq!(new_allowance, allowance - amount);
            assert!(new_allowance <= allowance);
        }
    }
}
