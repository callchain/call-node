//! Tests for instruction execution.

use crate::instructions::exec::execute_protocol_instructions;
use crate::instructions::types::{Instruction, PaymentMemo};
use crate::registry::AssetRegistry;
use crate::compliance::ComplianceEngine;
use crate::AccountState;
use call_primitives::Address;
use call_shielded::ShieldedState;

fn test_addr(n: u8) -> Address {
    Address::repeat_byte(n)
}

#[test]
fn test_execute_transfer() {
    let mut account = AccountState::new();
    account
        .balances
        .set_balance(1, test_addr(1), 1000)
        .unwrap();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, test_addr(1), 0, 0, 0)
        .unwrap();
    let mut compliance = ComplianceEngine::new();
    let mut shielded_state = ShieldedState::new();

    let instructions = vec![Instruction::Transfer {
        asset_id: 1,
        to: test_addr(2),
        amount: 500,
        memo: None,
    }];

    let results = execute_protocol_instructions(
        &instructions,
        &mut account,
        &mut registry,
        &mut compliance,
        &mut shielded_state,
        test_addr(1),
        None,
        &mut None,
        None,
    )
    .expect("execute");

    assert_eq!(results.len(), 1);
    assert_eq!(account.get_balance(1, &test_addr(1)), 500);
    assert_eq!(account.get_balance(1, &test_addr(2)), 500);
}

#[test]
fn test_execute_batch_transfer() {
    let mut account = AccountState::new();
    account
        .balances
        .set_balance(1, test_addr(1), 3000)
        .unwrap();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, test_addr(1), 0, 0, 0)
        .unwrap();
    let mut compliance = ComplianceEngine::new();
    let mut shielded_state = ShieldedState::new();

    let instructions = vec![Instruction::BatchTransfer {
        asset_id: 1,
        payments: vec![
            crate::instructions::types::PaymentEntry {
                to: test_addr(2),
                amount: 1000,
                memo: None,
            },
            crate::instructions::types::PaymentEntry {
                to: test_addr(3),
                amount: 1000,
                memo: None,
            },
        ],
    }];

    let results = execute_protocol_instructions(
        &instructions,
        &mut account,
        &mut registry,
        &mut compliance,
        &mut shielded_state,
        test_addr(1),
        None,
        &mut None,
        None,
    )
    .expect("execute");
    assert_eq!(results.len(), 1);
    assert_eq!(account.get_balance(1, &test_addr(1)), 1000);
}

#[test]
fn test_execute_approve_and_transfer_from() {
    let mut account = AccountState::new();
    account
        .balances
        .set_balance(1, test_addr(1), 1000)
        .unwrap();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, test_addr(1), 0, 0, 0)
        .unwrap();
    let mut compliance = ComplianceEngine::new();
    let mut shielded_state = ShieldedState::new();

    let instructions = vec![
        Instruction::Approve {
            asset_id: 1,
            spender: test_addr(1), // approve to self for this test
            amount: 500,
        },
        Instruction::TransferFrom {
            asset_id: 1,
            from: test_addr(1),
            to: test_addr(3),
            amount: 300,
        },
    ];

    let results = execute_protocol_instructions(
        &instructions,
        &mut account,
        &mut registry,
        &mut compliance,
        &mut shielded_state,
        test_addr(1),
        None,
        &mut None,
        None,
    )
    .expect("execute");
    assert_eq!(results.len(), 2);
    assert_eq!(account.get_balance(1, &test_addr(3)), 300);
}

#[test]
fn test_execute_mint_issuer_only() {
    // Mint requires sender to be the asset issuer.
    let mut account = AccountState::new();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("T".into(), "Test".into(), 18, test_addr(1), 0, 100, 0)
        .unwrap();
    let mut compliance = ComplianceEngine::new();
    let mut shielded_state = ShieldedState::new();

    // Non-issuer tries to mint — should fail
    let instructions = vec![Instruction::Mint {
        asset_id: 1,
        to: test_addr(99),
        amount: 1000,
    }];
    let result = execute_protocol_instructions(
        &instructions,
        &mut account,
        &mut registry,
        &mut compliance,
        &mut shielded_state,
        test_addr(99), // not the issuer
        None,
        &mut None,
        None,
    );
    assert!(result.is_err());

    // Issuer mints — should succeed
    let result = execute_protocol_instructions(
        &instructions,
        &mut account,
        &mut registry,
        &mut compliance,
        &mut shielded_state,
        test_addr(1), // the issuer
        None,
        &mut None,
        None,
    );
    assert!(result.is_ok());
    assert_eq!(account.get_balance(1, &test_addr(99)), 1000);
}

#[test]
fn test_execute_burn_issuer_only() {
    let mut account = AccountState::new();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("T".into(), "Test".into(), 18, test_addr(1), 0, 100, 0)
        .unwrap();
    account
        .balances
        .set_balance(1, test_addr(1), 1000)
        .unwrap();
    let mut compliance = ComplianceEngine::new();
    let mut shielded_state = ShieldedState::new();

    // Non-issuer tries to burn — should fail
    let instructions = vec![Instruction::Burn {
        asset_id: 1,
        from: test_addr(1),
        amount: 500,
    }];
    let result = execute_protocol_instructions(
        &instructions,
        &mut account,
        &mut registry,
        &mut compliance,
        &mut shielded_state,
        test_addr(99), // not the issuer
        None,
        &mut None,
        None,
    );
    assert!(result.is_err());

    // Issuer burns — should succeed
    let result = execute_protocol_instructions(
        &instructions,
        &mut account,
        &mut registry,
        &mut compliance,
        &mut shielded_state,
        test_addr(1), // the issuer
        None,
        &mut None,
        None,
    );
    assert!(result.is_ok());
    assert_eq!(account.get_balance(1, &test_addr(1)), 500);
}

#[test]
fn test_memo_size_limits() {
    let memo_ok = PaymentMemo {
        message: "a".repeat(256),
        reference: Some("r".repeat(128)),
        metadata: Some(vec![0u8; 1024]),
    };
    assert!(memo_ok.validate().is_ok());

    let memo_big = PaymentMemo {
        message: "a".repeat(257),
        reference: None,
        metadata: None,
    };
    assert!(memo_big.validate().is_err());

    let memo_ref_big = PaymentMemo {
        message: "ok".into(),
        reference: Some("r".repeat(129)),
        metadata: None,
    };
    assert!(memo_ref_big.validate().is_err());

    let memo_meta_big = PaymentMemo {
        message: "ok".into(),
        reference: None,
        metadata: Some(vec![0u8; 1025]),
    };
    assert!(memo_meta_big.validate().is_err());
}

#[test]
fn test_instruction_rollback_on_failure() {
    let mut account = AccountState::new();
    account
        .balances
        .set_balance(1, test_addr(1), 1000)
        .unwrap();
    let mut registry = AssetRegistry::new();
    let mut compliance = ComplianceEngine::new();
    let mut shielded_state = ShieldedState::new();

    let instructions = vec![
        Instruction::Transfer {
            asset_id: 1,
            to: test_addr(2),
            amount: 200,
            memo: None,
        },
        Instruction::Transfer {
            asset_id: 1,
            to: test_addr(3),
            amount: 2000, // insufficient — should trigger rollback
            memo: None,
        },
    ];

    let result = execute_protocol_instructions(
        &instructions,
        &mut account,
        &mut registry,
        &mut compliance,
        &mut shielded_state,
        test_addr(1),
        None,
        &mut None,
        None,
    );
    assert!(result.is_err());
    // State should be rolled back to original
    assert_eq!(account.get_balance(1, &test_addr(1)), 1000);
    assert_eq!(account.get_balance(1, &test_addr(2)), 0);
}

#[test]
fn test_atomic_multi_instruction() {
    let mut account = AccountState::new();
    account
        .balances
        .set_balance(1, test_addr(1), 1000)
        .unwrap();
    let mut registry = AssetRegistry::new();
    registry
        .register_asset("CALL".into(), "Callchain".into(), 18, test_addr(1), 0, 0, 0)
        .unwrap();
    let mut compliance = ComplianceEngine::new();
    let mut shielded_state = ShieldedState::new();

    let instructions = vec![
        Instruction::Transfer {
            asset_id: 1,
            to: test_addr(2),
            amount: 300,
            memo: None,
        },
        Instruction::Transfer {
            asset_id: 1,
            to: test_addr(3),
            amount: 200,
            memo: None,
        },
    ];

    let results = execute_protocol_instructions(
        &instructions,
        &mut account,
        &mut registry,
        &mut compliance,
        &mut shielded_state,
        test_addr(1),
        None,
        &mut None,
        None,
    )
    .expect("execute");
    assert_eq!(results.len(), 2);
    assert_eq!(account.get_balance(1, &test_addr(1)), 500);
    assert_eq!(account.get_balance(1, &test_addr(2)), 300);
    assert_eq!(account.get_balance(1, &test_addr(3)), 200);
}
