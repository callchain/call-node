use crate::ConsensusError;
use call_protocol::account::AccountState;
use call_protocol::instructions::{Instruction, InstructionResult};
use call_protocol::registry::AssetStatus;

// ── Bridge instruction helpers ────────────────────────────────────────

pub(crate) fn is_bridge_instruction(instr: &Instruction) -> bool {
    matches!(instr, Instruction::ExternalBridgeDeposit { .. } | Instruction::ExternalBridgeWithdraw { .. } | Instruction::ChallengeBridgeDeposit { .. } | Instruction::BridgeDeposit { .. } | Instruction::BridgeToEvm { .. } | Instruction::BridgeToProtocol { .. })
}

pub(crate) fn execute_bridge_instruction(
    instruction: &Instruction,
    sender: call_primitives::Address,
    account: &mut AccountState,
    bridge_state: &mut call_bridge::BridgeStateManager,
    config: &call_bridge::BridgeConfig,
    validators: &[call_primitives::Address],
    current_block_height: u64,
    evm_state: &mut call_evm::EvmState,
    evm_executor: &call_evm::EvmExecutor,
    registry: &mut call_protocol::registry::AssetRegistry,
) -> Result<InstructionResult, ConsensusError> {
    match instruction {
        Instruction::ExternalBridgeDeposit {
            source_tx_hash,
            source_chain,
            source_block_number,
            external_sender,
            recipient,
            asset_id,
            amount,
            validator_signatures,
        } => {
            let chain = match *source_chain {
                0 => call_bridge::ExternalChain::EthereumMainnet,
                1 => call_bridge::ExternalChain::Arbitrum,
                _ => return Err(ConsensusError::InvalidBlock("bridge: unknown source chain".into())),
            };
            let signatures: Vec<call_bridge::BridgeSignature> = validator_signatures
                .iter()
                .map(|(idx, sig)| call_bridge::BridgeSignature {
                    validator_index: *idx,
                    signature: sig.as_slice().try_into().unwrap_or([0u8; 65]),
                })
                .collect();
            let op = call_bridge::ExternalBridgeOp::Deposit {
                source_chain: chain,
                source_tx_hash: call_primitives::B256::from(*source_tx_hash),
                source_block_number: *source_block_number,
                sender: external_sender.clone(),
                recipient: *recipient,
                asset_id: *asset_id,
                amount: *amount,
                signatures,
            };
            match call_bridge::process_external_deposit(
                &op,
                account,
                bridge_state,
                config,
                validators,
                current_block_height,
                None, // source_contract: not provided in ExternalBridgeDeposit; registry check used
            ) {
                Ok(_) => Ok(InstructionResult::Success),
                Err(e) => Err(ConsensusError::InvalidBlock(format!("bridge deposit: {e:?}"))),
            }
        }
        Instruction::ExternalBridgeWithdraw {
            target_chain,
            target_address,
            asset_id,
            sender,
            amount,
        } => {
            let chain = match *target_chain {
                0 => call_bridge::ExternalChain::EthereumMainnet,
                1 => call_bridge::ExternalChain::Arbitrum,
                _ => return Err(ConsensusError::InvalidBlock("bridge: unknown target chain".into())),
            };
            let op = call_bridge::ExternalBridgeOp::Withdraw {
                target_chain: chain,
                target_address: target_address.clone(),
                asset_id: *asset_id,
                sender: *sender,
                amount: *amount,
            };
            match call_bridge::process_external_withdraw(
                &op,
                account,
                bridge_state,
                config,
                current_block_height,
            ) {
                Ok(_) => Ok(InstructionResult::Success),
                Err(e) => Err(ConsensusError::InvalidBlock(format!("bridge withdraw: {e:?}"))),
            }
        }
        Instruction::BridgeDeposit {
            source_chain,
            target_address,
            amount,
            asset_id,
            proof,
        } => {
            // Legacy BridgeDeposit: proof must contain a serialized BridgeDepositProof
            if proof.is_empty() {
                return Err(ConsensusError::InvalidBlock(
                    "bridge deposit: empty proof".into(),
                ));
            }
            let deposit_proof: call_bridge::BridgeDepositProof = serde_json::from_slice(proof)
                .map_err(|e| ConsensusError::InvalidBlock(format!("bridge deposit: invalid proof format: {e}")))?;

            let chain = match *source_chain as u8 {
                0 => call_bridge::ExternalChain::EthereumMainnet,
                1 => call_bridge::ExternalChain::Arbitrum,
                _ => return Err(ConsensusError::InvalidBlock("bridge: unknown source chain".into())),
            };
            let signatures: Vec<call_bridge::BridgeSignature> = deposit_proof.signatures
                .iter()
                .map(|(idx, sig)| call_bridge::BridgeSignature {
                    validator_index: *idx,
                    signature: sig.as_slice().try_into().unwrap_or([0u8; 65]),
                })
                .collect();
            let op = call_bridge::ExternalBridgeOp::Deposit {
                source_chain: chain,
                source_tx_hash: call_primitives::B256::from(deposit_proof.source_tx_hash),
                source_block_number: deposit_proof.source_block_number,
                sender: deposit_proof.external_sender,
                recipient: *target_address,
                asset_id: *asset_id,
                amount: *amount,
                signatures,
            };
            match call_bridge::process_external_deposit(
                &op,
                account,
                bridge_state,
                config,
                validators,
                current_block_height,
                None,
            ) {
                Ok(_) => Ok(InstructionResult::Success),
                Err(e) => Err(ConsensusError::InvalidBlock(format!("bridge deposit: {e:?}"))),
            }
        }
        Instruction::ChallengeBridgeDeposit {
            source_tx_hash,
            proof,
        } => {
            // Permissionless challenge: anyone can submit proof during challenge period
            if proof.is_empty() {
                return Err(ConsensusError::InvalidBlock(
                    "bridge challenge: proof cannot be empty".into(),
                ));
            }
            let revoked = call_bridge::challenge_pending_deposit(
                bridge_state,
                &call_primitives::B256::from(*source_tx_hash),
                current_block_height,
            );
            if revoked {
                Ok(InstructionResult::Success)
            } else {
                Err(ConsensusError::InvalidBlock(
                    "bridge challenge: no pending deposit found for source_tx_hash".into(),
                ))
            }
        }
        Instruction::BridgeToEvm {
            asset_id,
            to,
            amount,
        } => {
            // 1. Reject virtual USD (asset_id == 0)
            if *asset_id == 0 {
                return Err(ConsensusError::InvalidBlock(
                    "BridgeToEvm: asset 0 (USD) is not bridgeable".into(),
                ));
            }

            // 2. Validate asset is registered
            let asset = registry.get_asset(*asset_id).ok_or_else(|| {
                ConsensusError::InvalidBlock(format!(
                    "BridgeToEvm: asset {} not registered",
                    asset_id
                ))
            })?;
            if asset.status != AssetStatus::Active {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToEvm: asset {} is not active (status: {:?})",
                    asset_id, asset.status
                )));
            }

            // 3. Check bridge not paused
            if bridge_state.is_paused(*asset_id) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToEvm: bridge paused for asset {}",
                    asset_id
                )));
            }

            // 4. Check per-tx limit
            bridge_state
                .check_per_tx_limit(*amount, config.max_per_tx)
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToEvm: {e}")))?;

            // 5. Check daily limit
            bridge_state
                .check_and_update_daily_limit(
                    *asset_id,
                    *amount,
                    config.daily_limit_per_asset,
                    current_block_height,
                    config.blocks_per_day,
                )
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToEvm: {e}")))?;

            // 6. Check protocol balance is sufficient
            let protocol_balance = account.get_balance(*asset_id, &sender);
            if protocol_balance < *amount {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToEvm: insufficient protocol balance for asset {}: have {}, need {}",
                    asset_id, protocol_balance, amount
                )));
            }

            // 7. Deduct protocol balance
            account
                .deduct_balance(*asset_id, sender, *amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToEvm: {e}")))?;

            // 8. Bridge to EVM
            let amount_u256 = call_evm::U256::from(*amount);
            let exec_result = if *asset_id == call_protocol::CALL_ASSET_ID {
                // CALL: transfer as native EVM balance
                let current = evm_state.get_balance(to);
                evm_state.set_balance(*to, current + amount_u256);
                Ok(call_evm::EvmExecutionResult {
                    success: true,
                    gas_used: 21_000,
                    output: call_evm::Bytes::default(),
                    logs: vec![],
                })
            } else {
                // User-defined asset: mint ERC-20 wrapped token
                let Some(contract_addr) = registry.get_evm_contract_address(*asset_id) else {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "BridgeToEvm: no EVM contract registered for asset {}",
                        asset_id
                    )));
                };
                evm_executor
                    .evm_call_bridge_mint(
                        call_protocol::BRIDGE_EVM_ADDRESS,
                        contract_addr,
                        evm_state,
                        *to,
                        amount_u256,
                    )
                    .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToEvm: {e:?}")))
            };

            match exec_result {
                Ok(execution) => {
                    if execution.success {
                        let _ = registry.add_evm_supply(*asset_id, *amount);
                        bridge_state.record_deposit(*asset_id, *amount);
                        Ok(InstructionResult::Success)
                    } else {
                        Err(ConsensusError::InvalidBlock(
                            "BridgeToEvm: EVM operation reverted".into(),
                        ))
                    }
                }
                Err(e) => Err(e),
            }
        }
        Instruction::BridgeToProtocol {
            asset_id,
            to,
            amount,
        } => {
            // 1. Reject virtual USD (asset_id == 0)
            if *asset_id == 0 {
                return Err(ConsensusError::InvalidBlock(
                    "BridgeToProtocol: asset 0 (USD) is not bridgeable".into(),
                ));
            }

            // 2. Validate asset is registered
            let asset = registry.get_asset(*asset_id).ok_or_else(|| {
                ConsensusError::InvalidBlock(format!(
                    "BridgeToProtocol: asset {} not registered",
                    asset_id
                ))
            })?;
            if asset.status != AssetStatus::Active {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToProtocol: asset {} is not active (status: {:?})",
                    asset_id, asset.status
                )));
            }

            // 3. Check bridge not paused
            if bridge_state.is_paused(*asset_id) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToProtocol: bridge paused for asset {}",
                    asset_id
                )));
            }

            // 4. Check per-tx limit
            bridge_state
                .check_per_tx_limit(*amount, config.max_per_tx)
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToProtocol: {e}")))?;

            // 5. Check daily limit
            bridge_state
                .check_and_update_daily_limit(
                    *asset_id,
                    *amount,
                    config.daily_limit_per_asset,
                    current_block_height,
                    config.blocks_per_day,
                )
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToProtocol: {e}")))?;

            let amount_u256 = call_evm::U256::from(*amount);

            // 6. Withdraw from EVM
            let exec_result = if *asset_id == call_protocol::CALL_ASSET_ID {
                // CALL: transfer from native EVM balance
                let evm_balance = evm_state.get_balance(&sender);
                if evm_balance < amount_u256 {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "BridgeToProtocol: insufficient EVM native balance for CALL: have {}, need {}",
                        evm_balance, amount_u256
                    )));
                }
                evm_state.set_balance(sender, evm_balance - amount_u256);
                Ok(call_evm::EvmExecutionResult {
                    success: true,
                    gas_used: 21_000,
                    output: call_evm::Bytes::default(),
                    logs: vec![],
                })
            } else {
                // User-defined asset: burn ERC-20 wrapped token
                let Some(contract_addr) = registry.get_evm_contract_address(*asset_id) else {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "BridgeToProtocol: no EVM contract registered for asset {}",
                        asset_id
                    )));
                };
                evm_executor
                    .evm_call_bridge_burn(
                        sender,
                        contract_addr,
                        evm_state,
                        amount_u256,
                    )
                    .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToProtocol: {e:?}")))
            };

            match exec_result {
                Ok(execution) => {
                    if execution.success {
                        // 7. Credit protocol balance
                        account
                            .credit_balance(*asset_id, *to, *amount)
                            .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToProtocol: {e}")))?;
                        let _ = registry.sub_evm_supply(*asset_id, *amount);
                        bridge_state.record_withdrawal(*asset_id, *amount);
                        Ok(InstructionResult::Success)
                    } else {
                        Err(ConsensusError::InvalidBlock(
                            "BridgeToProtocol: EVM operation reverted".into(),
                        ))
                    }
                }
                Err(e) => Err(e),
            }
        }
        _ => Err(ConsensusError::InvalidBlock("not a bridge instruction".into())),
    }
}
