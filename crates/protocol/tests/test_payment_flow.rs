//! Payment flow integration tests
mod integration;

// Re-export test module contents
mod test_payment_flow_impl {
    use super::integration::*;
    use call_primitives::{Address, FeeCurrency, InstructionType};
    use call_protocol::AccountState;
    use call_protocol::compliance::ComplianceEngine;
    use call_protocol::instructions::{
        Instruction, PaymentEntry, PaymentMemo,
    };
    use call_protocol::registry::AssetRegistry;
    use call_protocol::smart_accounts::SmartAccountRegistry;
    use call_protocol::sponsor::SponsorRegistry;
    use call_protocol::transaction::{
        calculate_gas_units, compute_fee, deduct_gas, update_base_fee,
        AuthScheme, FeeParams, GasConfig,
    };
    use call_protocol::ProtocolTransaction;
    use call_shielded::ShieldedState;

    // ── Full Protocol Payment Flow ─────────────────────────────────────────

    /// Register asset -> transfer -> check balance -> verify receipt
    #[test]
    fn test_full_payment_flow_register_transfer_balance() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);
        let receiver = addr(3);

        let asset_id = setup_asset(&mut account, &mut registry, "TEST", issuer, holder, 10_000);

        assert_eq!(account.get_balance(asset_id, &holder), 10_000);
        assert_eq!(account.get_balance(asset_id, &receiver), 0);

        let tx = make_tx(
            holder,
            1,
            vec![make_transfer(asset_id, receiver, 3_000)],
            GasConfig::SelfPay,
        );
        execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params).unwrap();

        assert_eq!(account.get_balance(asset_id, &holder), 7_000);
        assert_eq!(account.get_balance(asset_id, &receiver), 3_000);
    }

    // ── Multi-Instruction Atomic Transfer ──────────────────────────────────

    /// Transfer + Approve in single tx
    #[test]
    fn test_atomic_multi_instruction_transfer_approve_bridge() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let holder = addr(1);
        let spender = addr(2);
        let receiver = addr(3);

        let asset_id = setup_asset(&mut account, &mut registry, "ATOMIC", addr(10), holder, 5_000);

        let tx = make_tx(
            holder,
            1,
            vec![
                Instruction::Transfer {
                    asset_id,
                    to: receiver,
                    amount: 1_000,
                    memo: None,
                },
                Instruction::Approve {
                    asset_id,
                    spender,
                    amount: 500,
                },
            ],
            GasConfig::SelfPay,
        );
        execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params).unwrap();

        assert_eq!(account.get_balance(asset_id, &holder), 4_000);
        assert_eq!(account.get_balance(asset_id, &receiver), 1_000);
        assert_eq!(
            account.allowances.get_allowance(asset_id, &holder, &spender),
            500
        );
    }

    /// Rollback: if one instruction fails, all revert
    #[test]
    fn test_atomic_rollback_on_failure() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let fee_params = FeeParams::default();

        let holder = addr(1);
        let receiver = addr(2);

        let asset_id = setup_asset(&mut account, &mut registry, "ROLL", addr(10), holder, 1_000);

        let result = call_protocol::instructions::execute_protocol_instructions(
            &[
                Instruction::Transfer {
                    asset_id,
                    to: receiver,
                    amount: 500,
                    memo: None,
                },
                Instruction::Transfer {
                    asset_id,
                    to: receiver,
                    amount: 9_999,
                    memo: None,
                },
            ],
            &mut account,
            &mut registry,
            &mut compliance,
            &mut ShieldedState::new(),
            holder,
            None,
            &mut None,
            None,
        );
        assert!(result.is_err());

        assert_eq!(account.get_balance(asset_id, &holder), 1_000);
        assert_eq!(account.get_balance(asset_id, &receiver), 0);
    }

    // ── Batch Transfer with Memo ───────────────────────────────────────────

    #[test]
    fn test_batch_transfer_with_memo() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let sender = addr(1);
        let recipients: Vec<Address> = (2..12).map(|i| addr(i)).collect();

        let asset_id = setup_asset(&mut account, &mut registry, "BATCH", addr(10), sender, 100_000);

        let payments: Vec<PaymentEntry> = recipients
            .iter()
            .map(|r| PaymentEntry {
                to: *r,
                amount: 1_000,
                memo: Some(make_memo()),
            })
            .collect();

        let tx = make_tx(
            sender,
            1,
            vec![Instruction::BatchTransfer {
                asset_id,
                payments,
            }],
            GasConfig::SelfPay,
        );

        let gas_units = calculate_gas_units(&tx.instructions);
        assert!(gas_units > 0);

        execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params).unwrap();

        for r in &recipients {
            assert_eq!(account.get_balance(asset_id, r), 1_000);
        }
        assert_eq!(account.get_balance(asset_id, &sender), 90_000);
    }

    // ── Stablecoin Gas Payment ────────────────────────────────────────────

    #[test]
    fn test_stablecoin_gas_payment() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let sender = addr(1);
        let receiver = addr(2);

        let stable_id = setup_asset(&mut account, &mut registry, "USDC", addr(10), sender, 10_000);
        // Need enough stablecoin balance to cover the fee (~100k CALL wei equivalent)
        account.balances.set_balance(stable_id, sender, 10_000_000).unwrap();

        let tx = ProtocolTransaction {
            sender,
            nonce: 1,
            instructions: vec![make_transfer(stable_id, receiver, 100)],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Stablecoin(stable_id),
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: sig_byte(0xAA),
            },
        };

        execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params).unwrap();

        assert_eq!(account.get_balance(stable_id, &receiver), 100);
    }

    // ── GasConfig: SelfPay ────────────────────────────────────────────────

    #[test]
    fn test_gas_self_pay_deducted_from_sender() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let sender = addr(1);
        let receiver = addr(2);

        setup_asset(&mut account, &mut registry, "SELF", addr(10), sender, 5_000);
        account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000).unwrap();

        let call_balance_before = account.get_balance(call_protocol::CALL_ASSET_ID, &sender);

        let tx = make_tx(
            sender,
            1,
            vec![make_transfer(1, receiver, 100)],
            GasConfig::SelfPay,
        );
        execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params).unwrap();

        let call_balance_after = account.get_balance(call_protocol::CALL_ASSET_ID, &sender);
        assert!(call_balance_after < call_balance_before);
    }

    // ── GasConfig: AuthorizedSponsor ──────────────────────────────────────

    #[test]
    fn test_gas_authorized_sponsor_pays() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut fee_params = FeeParams::default();
        let mut sponsors = SponsorRegistry::new();

        let sender = addr(1);
        let receiver = addr(2);
        let sponsor_addr = addr(50);

        let asset_id = setup_asset(&mut account, &mut registry, "SPON", addr(10), sender, 5_000);
        account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 100).unwrap();
        setup_sponsor(&mut sponsors, &mut account, sponsor_addr, vec![sender], 10_000_000);

        let tx = make_tx(
            sender,
            1,
            vec![make_transfer(asset_id, receiver, 100)],
            GasConfig::AuthorizedSponsor { sponsor: sponsor_addr },
        );

        let gas_units = calculate_gas_units(&tx.instructions);
        let fee = compute_fee(gas_units, 0, fee_params.base_fee);

        sponsors
            .verify_and_deduct_authorized_sponsor(&sponsor_addr, &sender, fee, 1, &mut account)
            .unwrap();

        call_protocol::instructions::execute_protocol_instructions(
            &tx.instructions,
            &mut account,
            &mut registry,
            &mut compliance,
            &mut ShieldedState::new(),
            sender,
            None,
            &mut None,
            None,
        )
        .unwrap();

        assert_eq!(account.get_balance(asset_id, &receiver), 100);
        assert_eq!(account.get_balance(call_protocol::CALL_ASSET_ID, &sender), 100);
    }

    // ── AuthScheme: MultiSig 2-of-3 ───────────────────────────────────────

    #[test]
    fn test_multisig_2_of_3_authorization() {
        let mut registry = SmartAccountRegistry::new();
        let account = addr(1);
        let signers = vec![addr(2), addr(3), addr(4)];

        setup_multisig(&mut registry, account, signers.clone(), 2);

        assert!(registry.verify_multisig(&account, &[signers[0], signers[1]]).is_ok());
        assert!(registry.verify_multisig(&account, &[signers[0]]).is_err());
        assert!(registry.verify_multisig(&account, &signers).is_ok());
    }

    // ── AuthScheme: SessionKey with Limits ────────────────────────────────

    #[test]
    fn test_session_key_with_limits() {
        let mut registry = SmartAccountRegistry::new();
        let account = addr(1);
        let session = addr(99);
        let expires = 10_000;

        setup_session_key(&mut registry, account, session, expires);

        assert!(registry.verify_session_key(&account, &session, 5_000).is_ok());
        assert!(registry.verify_session_key(&account, &session, 20_000).is_err());
        assert!(registry.verify_session_key(&account, &addr(88), 5_000).is_err());
    }

    // ── Base Fee Dynamics ────────────────────────────────────────────────

    #[test]
    fn test_base_fee_dynamics_under_congestion() {
        let mut params = FeeParams::default();
        assert_eq!(params.base_fee, 10);

        // Simulate very congested blocks (gas used >> target of 10M)
        // With coeff=1, increase = base_fee * (diff/target * 1/8)
        // For gas_used=20M: diff=10M, increase = 10 * (10M/10M * 1/8) = 10/8 = 1 per call
        for _ in 0..10 {
            update_base_fee(&mut params, 20_000_000); // max gas per block
        }
        assert!(params.base_fee > 10);
        let congested_fee = params.base_fee;

        // Simulate idle blocks (gas used < target)
        for _ in 0..10 {
            update_base_fee(&mut params, 0);
        }
        assert!(params.base_fee < congested_fee);
    }

    // ── Frozen / Delisted Asset Restrictions ───────────────────────────────

    #[test]
    fn test_frozen_asset_rejects_transfer() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);
        let receiver = addr(3);

        let asset_id = setup_asset(&mut account, &mut registry, "FROZEN", issuer, holder, 10_000);
        registry.freeze_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            holder,
            1,
            vec![make_transfer(asset_id, receiver, 1_000)],
            GasConfig::SelfPay,
        );
        let result = execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params);
        assert!(result.is_err(), "transfer on frozen asset should fail");
        assert!(result.unwrap_err().contains("frozen"));
    }

    #[test]
    fn test_delisted_asset_rejects_transfer() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);
        let receiver = addr(3);

        let asset_id = setup_asset(&mut account, &mut registry, "DELISTED", issuer, holder, 10_000);
        registry.delist_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            holder,
            1,
            vec![make_transfer(asset_id, receiver, 1_000)],
            GasConfig::SelfPay,
        );
        let result = execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params);
        assert!(result.is_err(), "transfer on delisted asset should fail");
        assert!(result.unwrap_err().contains("delisted"));
    }

    #[test]
    fn test_frozen_asset_rejects_batch_transfer() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);

        let asset_id = setup_asset(&mut account, &mut registry, "FROZEN2", issuer, holder, 10_000);
        registry.freeze_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            holder,
            1,
            vec![Instruction::BatchTransfer {
                asset_id,
                payments: vec![PaymentEntry {
                    to: addr(3),
                    amount: 1_000,
                    memo: None,
                }],
            }],
            GasConfig::SelfPay,
        );
        let result = execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params);
        assert!(result.is_err(), "batch transfer on frozen asset should fail");
    }

    #[test]
    fn test_frozen_asset_rejects_approve() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);

        let asset_id = setup_asset(&mut account, &mut registry, "FROZEN3", issuer, holder, 10_000);
        registry.freeze_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            holder,
            1,
            vec![Instruction::Approve {
                asset_id,
                spender: addr(3),
                amount: 1_000,
            }],
            GasConfig::SelfPay,
        );
        let result = execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params);
        assert!(result.is_err(), "approve on frozen asset should fail");
    }

    #[test]
    fn test_frozen_asset_rejects_transfer_from() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);
        let spender = addr(3);

        let asset_id = setup_asset(&mut account, &mut registry, "FROZEN4", issuer, holder, 10_000);
        account.allowances.set_allowance(asset_id, holder, spender, 5_000);
        registry.freeze_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            spender,
            1,
            vec![Instruction::TransferFrom {
                asset_id,
                from: holder,
                to: addr(4),
                amount: 1_000,
            }],
            GasConfig::SelfPay,
        );
        let result = execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params);
        assert!(result.is_err(), "transfer_from on frozen asset should fail");
    }

    #[test]
    fn test_frozen_asset_rejects_mint() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);

        let asset_id = setup_asset(&mut account, &mut registry, "FROZEN5", issuer, holder, 0);
        registry.freeze_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            issuer,
            1,
            vec![Instruction::Mint {
                asset_id,
                to: holder,
                amount: 1_000,
            }],
            GasConfig::SelfPay,
        );
        let result = execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params);
        assert!(result.is_err(), "mint on frozen asset should fail");
    }

    #[test]
    fn test_delisted_asset_rejects_mint() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);

        let asset_id = setup_asset(&mut account, &mut registry, "DELISTED2", issuer, holder, 0);
        registry.delist_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            issuer,
            1,
            vec![Instruction::Mint {
                asset_id,
                to: holder,
                amount: 1_000,
            }],
            GasConfig::SelfPay,
        );
        let result = execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params);
        assert!(result.is_err(), "mint on delisted asset should fail");
        assert!(result.unwrap_err().contains("delisted"));
    }

    #[test]
    fn test_frozen_asset_rejects_burn() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);

        let asset_id = setup_asset(&mut account, &mut registry, "FROZEN6", issuer, holder, 1_000);
        registry.freeze_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            issuer,
            1,
            vec![Instruction::Burn {
                asset_id,
                from: holder,
                amount: 500,
            }],
            GasConfig::SelfPay,
        );
        let result = execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params);
        assert!(result.is_err(), "burn on frozen asset should fail");
    }

    #[test]
    fn test_delisted_asset_rejects_burn() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);

        let asset_id = setup_asset(&mut account, &mut registry, "DELISTED3", issuer, holder, 1_000);
        registry.delist_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            issuer,
            1,
            vec![Instruction::Burn {
                asset_id,
                from: holder,
                amount: 500,
            }],
            GasConfig::SelfPay,
        );
        let result = execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params);
        assert!(result.is_err(), "burn on delisted asset should fail");
        assert!(result.unwrap_err().contains("delisted"));
    }

    #[test]
    fn test_unfrozen_asset_allows_transfer() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::new();
        let mut shielded_state = ShieldedState::new();
        let fee_params = FeeParams::default();

        let issuer = addr(1);
        let holder = addr(2);
        let receiver = addr(3);

        let asset_id = setup_asset(&mut account, &mut registry, "UNFROZEN", issuer, holder, 10_000);
        registry.freeze_asset(asset_id, &issuer).unwrap();
        registry.unfreeze_asset(asset_id, &issuer).unwrap();

        let tx = make_tx(
            holder,
            1,
            vec![make_transfer(asset_id, receiver, 1_000)],
            GasConfig::SelfPay,
        );
        execute_tx(&tx, &mut account, &mut registry, &mut compliance, &mut shielded_state, &fee_params).unwrap();
        assert_eq!(account.get_balance(asset_id, &receiver), 1_000);
    }
}
