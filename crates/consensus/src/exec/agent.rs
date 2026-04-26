use crate::ConsensusError;
use call_evm::{EvmExecutor, EvmState};
use call_protocol::account::AccountState;
use call_protocol::instructions::{Instruction, InstructionResult};
use call_protocol::registry::AssetRegistry;
use call_protocol::FeeParams;

// ── Agent instruction helpers ─────────────────────────────────────────

pub(crate) fn is_agent_instruction(instr: &Instruction) -> bool {
    matches!(
        instr,
        Instruction::AgentPay { .. }
            | Instruction::AgentBatchPay { .. }
            | Instruction::AgentCall { .. }
            | Instruction::AgentBridgeDeposit { .. }
            | Instruction::RegisterAgent { .. }
            | Instruction::GrantAgentBalance { .. }
            | Instruction::RevokeAgentBalance { .. }
    )
}

/// Verify agent permissions for a single instruction.
pub(crate) fn verify_agent_instruction_permissions(
    agent: &call_agent::AgentRegistration,
    asset_id: call_primitives::AssetId,
    amount: u128,
    current_block: u64,
) -> Result<(), ConsensusError> {
    let perms = &agent.permissions;
    if !perms.is_asset_allowed(asset_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "agent {}: asset {} not allowed",
            agent.agent_id, asset_id
        )));
    }
    if amount > perms.per_tx_limit {
        return Err(ConsensusError::InvalidBlock(format!(
            "agent {}: amount {} exceeds per-tx limit {}",
            agent.agent_id, amount, perms.per_tx_limit
        )));
    }
    if perms.is_expired(current_block) {
        return Err(ConsensusError::InvalidBlock(format!(
            "agent {}: permissions expired",
            agent.agent_id
        )));
    }
    Ok(())
}

pub(crate) fn execute_agent_instruction(
    instruction: &Instruction,
    sender: call_primitives::Address,
    agent_balances: &mut call_agent::AgentBalances,
    agent_registry: &mut call_agent::AgentRegistry,
    account: &mut AccountState,
    evm_state: &mut EvmState,
    bridge_state: &mut call_bridge::BridgeStateManager,
    registry: &mut AssetRegistry,
    evm_executor: &EvmExecutor,
    current_block_height: u64,
    fee_params: &FeeParams,
    agent_events: &mut Vec<call_agent::AgentEvent>,
) -> Result<InstructionResult, ConsensusError> {
    match instruction {
        Instruction::AgentPay { payment } => {
            let agent = agent_registry
                .get_agent(payment.agent_id)
                .ok_or_else(|| ConsensusError::InvalidBlock("agent not found".into()))?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "agent pay: sender is not owner".into(),
                ));
            }
            verify_agent_instruction_permissions(
                agent,
                payment.asset_id,
                payment.amount,
                current_block_height,
            )?;
            agent_balances
                .deduct(agent.owner, payment.agent_id, payment.asset_id, payment.amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("agent pay: {e}")))?;
            account
                .credit_balance(payment.asset_id, payment.to, payment.amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("agent pay: {e}")))?;
            agent_events.push(call_agent::AgentEvent {
                event_type: call_agent::AgentEventType::AgentPay,
                agent_id: payment.agent_id,
                tx_hash: None,
                asset_id: payment.asset_id,
                amount: payment.amount,
                recipient: Some(payment.to),
                block_height: current_block_height,
            });
            Ok(InstructionResult::Success)
        }
        Instruction::AgentBatchPay { payments } => {
            for payment in payments {
                let agent = agent_registry
                    .get_agent(payment.agent_id)
                    .ok_or_else(|| ConsensusError::InvalidBlock("agent not found".into()))?;
                if agent.owner != sender {
                    return Err(ConsensusError::InvalidBlock(
                        "agent batch pay: sender is not owner".into(),
                    ));
                }
                verify_agent_instruction_permissions(
                    agent,
                    payment.asset_id,
                    payment.amount,
                    current_block_height,
                )?;
                agent_balances
                    .deduct(agent.owner, payment.agent_id, payment.asset_id, payment.amount)
                    .map_err(|e| {
                        ConsensusError::InvalidBlock(format!("agent batch pay: {e}"))
                    })?;
                account
                    .credit_balance(payment.asset_id, payment.to, payment.amount)
                    .map_err(|e| {
                        ConsensusError::InvalidBlock(format!("agent batch pay: {e}"))
                    })?;
                agent_events.push(call_agent::AgentEvent {
                    event_type: call_agent::AgentEventType::AgentBatchPay,
                    agent_id: payment.agent_id,
                    tx_hash: None,
                    asset_id: payment.asset_id,
                    amount: payment.amount,
                    recipient: Some(payment.to),
                    block_height: current_block_height,
                });
            }
            Ok(InstructionResult::Success)
        }
        Instruction::AgentCall { agent_id, target, data } => {
            let agent = agent_registry
                .get_agent(*agent_id)
                .ok_or_else(|| ConsensusError::InvalidBlock("agent not found".into()))?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "agent call: sender is not owner".into(),
                ));
            }
            if !agent.permissions.is_protocol_allowed(target) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "agent {}: target {:?} not in allowed protocols",
                    agent.agent_id, target
                )));
            }
            if agent.permissions.is_expired(current_block_height) {
                return Err(ConsensusError::InvalidBlock(
                    "agent call: permissions expired".into(),
                ));
            }
            let tx = call_evm::EvmTransaction {
                caller: sender,
                nonce: 0,
                gas_limit: 1_000_000,
                gas_price: 0,
                to: Some(*target),
                value: call_primitives::U256::ZERO,
                data: call_primitives::Bytes::from(data.clone()),
                chain_id: evm_executor.chain_id,
            };
            match evm_executor.execute_tx(tx, evm_state) {
                Ok(_) => {
                    agent_events.push(call_agent::AgentEvent {
                        event_type: call_agent::AgentEventType::AgentCall,
                        agent_id: *agent_id,
                        tx_hash: None,
                        asset_id: 0,
                        amount: 0,
                        recipient: Some(*target),
                        block_height: current_block_height,
                    });
                    Ok(InstructionResult::Success)
                }
                Err(e) => Err(ConsensusError::InvalidBlock(format!("agent call: {e:?}"))),
            }
        }
        Instruction::AgentBridgeDeposit {
            agent_id,
            asset_id,
            amount,
            target_address,
            ..
        } => {
            let agent = agent_registry
                .get_agent(*agent_id)
                .ok_or_else(|| ConsensusError::InvalidBlock("agent not found".into()))?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "agent bridge deposit: sender is not owner".into(),
                ));
            }
            verify_agent_instruction_permissions(
                agent,
                *asset_id,
                *amount,
                current_block_height,
            )?;
            agent_balances
                .deduct(agent.owner, *agent_id, *asset_id, *amount)
                .map_err(|e| {
                    ConsensusError::InvalidBlock(format!("agent bridge deposit: {e}"))
                })?;
            account
                .deduct_balance(*asset_id, sender, *amount)
                .map_err(|e| {
                    ConsensusError::InvalidBlock(format!("agent bridge deposit: {e}"))
                })?;

            let config = call_bridge::BridgeConfig::default();
            let op = call_bridge::BridgeOp::DepositToEvm {
                asset_id: *asset_id,
                from: sender,
                to: call_primitives::Address::from_slice(
                    &target_address[..target_address.len().min(20)],
                ),
                amount: *amount,
            };
            let bridge_address = registry
                .get_evm_contract_address(*asset_id)
                .unwrap_or_else(|| call_primitives::Address::from_slice(&[0xCCu8; 20]));

            match call_bridge::execute_deposit(
                &op, account, evm_state, evm_executor, bridge_state, &config, registry,
                bridge_address, sender, current_block_height,
            ) {
                Ok(exec) if exec.success => {
                    let _ = registry.add_evm_supply(*asset_id, *amount);
                    agent_events.push(call_agent::AgentEvent {
                        event_type: call_agent::AgentEventType::AgentBridgeDeposit,
                        agent_id: *agent_id,
                        tx_hash: None,
                        asset_id: *asset_id,
                        amount: *amount,
                        recipient: Some(call_primitives::Address::from_slice(
                            &target_address[..target_address.len().min(20)],
                        )),
                        block_height: current_block_height,
                    });
                    Ok(InstructionResult::Success)
                }
                Ok(_) => {
                    let _ = agent_balances.credit(agent.owner, *agent_id, *asset_id, *amount);
                    let _ = account.credit_balance(*asset_id, sender, *amount);
                    Err(ConsensusError::InvalidBlock(
                        "agent bridge deposit failed".into(),
                    ))
                }
                Err(e) => {
                    let _ = agent_balances.credit(agent.owner, *agent_id, *asset_id, *amount);
                    let _ = account.credit_balance(*asset_id, sender, *amount);
                    Err(ConsensusError::InvalidBlock(format!(
                        "agent bridge deposit: {e:?}"
                    )))
                }
            }
        }
        Instruction::RegisterAgent {
            pubkey,
            name,
            url,
        } => {
            if pubkey.len() != 64 {
                return Err(ConsensusError::InvalidBlock(
                    "RegisterAgent: pubkey must be 64 bytes".into(),
                ));
            }
            let mut pk = [0u8; 64];
            pk.copy_from_slice(pubkey);
            let fee = fee_params.base_fee;
            if fee > 0 {
                let sender_balance = account.get_balance(call_protocol::CALL_ASSET_ID, &sender);
                if sender_balance < fee {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "RegisterAgent: insufficient CALL balance for fee: need {fee}, have {sender_balance}"
                    )));
                }
                account
                    .deduct_balance(call_protocol::CALL_ASSET_ID, sender, fee)
                    .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAgent: {e}")))?;
            }
            agent_registry
                .register_agent(
                    sender,
                    pk,
                    name.clone(),
                    url.clone(),
                    [0u8; 32],
                    None,
                    current_block_height,
                    None,
                )
                .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAgent: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::GrantAgentBalance {
            agent_id,
            asset_id,
            amount,
        } => {
            let agent = agent_registry
                .get_agent(*agent_id)
                .ok_or_else(|| {
                    ConsensusError::InvalidBlock(format!(
                        "GrantAgentBalance: agent {agent_id} not found"
                    ))
                })?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "GrantAgentBalance: only agent owner can grant".into(),
                ));
            }
            agent_balances
                .grant_funds(sender, *agent_id, *asset_id, *amount, account)
                .map_err(|e| {
                    ConsensusError::InvalidBlock(format!("GrantAgentBalance: {e:?}"))
                })?;
            Ok(InstructionResult::Success)
        }
        Instruction::RevokeAgentBalance {
            agent_id,
            asset_id,
        } => {
            let agent = agent_registry
                .get_agent(*agent_id)
                .ok_or_else(|| {
                    ConsensusError::InvalidBlock(format!(
                        "RevokeAgentBalance: agent {agent_id} not found"
                    ))
                })?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "RevokeAgentBalance: only agent owner can revoke".into(),
                ));
            }
            agent_balances.revoke_funds(sender, *agent_id, *asset_id);
            Ok(InstructionResult::Success)
        }
        _ => Err(ConsensusError::InvalidBlock("not an agent instruction".into())),
    }
}
