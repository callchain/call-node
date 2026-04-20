//! Agent transaction verification and execution (per spec §6.6, §6.8)
//!
//! - verify_agent_tx: full 5-step validation
//! - execute_agent_tx: multi-instruction execution with 0.5x gas discount
//! - Helper functions for AgentPay, AgentBatchPay, AgentCall, AgentBridgeDeposit

use call_primitives::{Address, AssetId, PublicKey};
use call_protocol::{
    balances::BalanceState,
    registry::AssetRegistry,
    compliance::ComplianceEngine,
    instructions::{execute_protocol_instructions, Instruction, InstructionResult},
    transaction::calculate_gas_units,
};
use call_crypto::secp256k1_verify;
use call_bridge::{
    execute_deposit as bridge_execute_deposit,
    BridgeConfig, BridgeStateManager,
};
use call_evm::{EvmExecutor, EvmState, EvmTransaction, Bytes};
use call_shielded::ShieldedState;
use crate::{
    AgentBalances, AgentError, AgentFeeConfig, AgentNonces, AgentPermissions,
    AgentDailyUsage, AgentRegistration, SignedAgentTx,
    requires_owner_signature, verify_agent_permissions,
};

/// Agent transaction context (includes resolved owner key for signature verification)
#[derive(Debug, Clone)]
pub struct AgentTxContext {
    pub agent_id: u64,
    pub nonce: u64,
    pub expires_at: u64,
    pub agent_signature: [u8; 65],
    pub owner_public_key: PublicKey,
}

/// Verify agent transaction (per spec §6.6)
///
/// Full 5-step validation:
/// 1. Agent signature verification
/// 2. Nonce check (stale/duplicate)
/// 3. Per-instruction permissions: assets, counterparties, per_tx_limit
/// 4. Expiry check
/// 5. Owner signature threshold for amounts above require_owner_signature_above
pub fn verify_agent_tx(
    signed_tx: &SignedAgentTx,
    agent: &AgentRegistration,
    permissions: &AgentPermissions,
    nonces: &mut AgentNonces,
    daily_usage: &mut AgentDailyUsage,
    fee_config: &AgentFeeConfig,
    owner_public_key: PublicKey,
    current_block: u64,
    current_time: u64,
) -> Result<(), AgentError> {
    let protocol_tx = &signed_tx.protocol_tx;

    // 1. Agent signature verification
    // Compute the tx hash and verify against agent's public key
    let tx_hash = compute_agent_tx_hash(protocol_tx, agent.agent_id);
    let sig = extract_signature(&protocol_tx.auth);
    secp256k1_verify(&agent.agent_public_key, &sig, &tx_hash)
        .map_err(|_| AgentError::InvalidAgentSignature)?;

    // 2. Nonce check (stale/duplicate)
    nonces.check_and_increment(agent.owner, agent.agent_id, protocol_tx.nonce)?;

    // 3. Per-instruction permissions: assets, counterparties, per_tx_limit
    for instr in &protocol_tx.instructions {
        let details = extract_instruction_details(instr);
        if details.is_empty() {
            return Err(AgentError::PermissionDenied("unsupported instruction".into()));
        }
        for (asset_id, counterparty, amount) in details {
            verify_agent_permissions(
                permissions,
                daily_usage,
                fee_config,
                asset_id,
                &counterparty,
                amount,
                protocol_tx.gas_limit as u128, // approximate fee
                current_block,
                current_time,
            )?;
        }
    }

    // 4. Expiry check
    let expires_at = signed_tx.protocol_tx.expires_at;
    if expires_at != 0 && current_block > expires_at {
        return Err(AgentError::TransactionExpired);
    }
    // Also check agent-level permission expiry
    if permissions.is_expired(current_block) {
        return Err(AgentError::TransactionExpired);
    }

    // 5. Owner signature threshold
    let total_amount: u128 = protocol_tx
        .instructions
        .iter()
        .flat_map(|i| extract_instruction_details(i))
        .map(|(_, _, a)| a)
        .sum();

    if requires_owner_signature(total_amount, fee_config) {
        if signed_tx.owner_signature.is_none() {
            return Err(AgentError::OwnerSignatureRequired);
        }
        // Verify owner signature
        let owner_sig = signed_tx.owner_signature.unwrap();
        secp256k1_verify(
            &owner_public_key,
            &owner_sig,
            &tx_hash,
        )
        .map_err(|_| AgentError::InvalidOwnerSignature)?;
    }

    Ok(())
}

/// Execute agent transaction with gas discount (per spec §6.8)
///
/// Agent transactions get 0.5x gas discount on base gas units.
/// Multi-instruction execution follows the same atomicity rules as protocol transactions.
pub fn execute_agent_tx(
    signed_tx: &SignedAgentTx,
    agent: &AgentRegistration,
    balances: &mut AgentBalances,
    protocol_balances: &mut BalanceState,
    _evm_state: &mut EvmState,
    _evm_executor: &EvmExecutor,
    _bridge_state: &mut BridgeStateManager,
    _bridge_config: &BridgeConfig,
    registry: &AssetRegistry,
    compliance: &mut ComplianceEngine,
    shielded_state: &mut ShieldedState,
    agent_evm_address: Address,
) -> Result<Vec<InstructionResult>, AgentError> {
    let protocol_tx = &signed_tx.protocol_tx;

    // Calculate gas with agent discount (0.5x base for agent instructions per §6.8)
    let gas_units = calculate_gas_units(&protocol_tx.instructions);
    let agent_gas_units = gas_units / 2; // Agent discount

    // Deduct gas fee from agent balance using the transaction's fee currency
    let fee_asset_id = match protocol_tx.fee_currency {
        call_primitives::FeeCurrency::Call => 1,
        call_primitives::FeeCurrency::Stablecoin(id) => id,
    };
    let fee = agent_gas_units as u128 * protocol_tx.max_fee;
    let agent_balance = balances.get_balance(agent.owner, agent.agent_id, fee_asset_id);
    if agent_balance < fee {
        return Err(AgentError::InsufficientAgentBalance(agent.agent_id, fee));
    }
    balances.deduct(agent.owner, agent.agent_id, fee_asset_id, fee)?;

    // Execute instructions via protocol engine
    let results = execute_protocol_instructions(
        &protocol_tx.instructions,
        protocol_balances,
        registry,
        compliance,
        shielded_state,
        agent_evm_address,
        None,
        &mut None,
        None,
    )
    .map_err(|e| AgentError::ExecutionFailed(format!("{:?}", e)))?;

    Ok(results)
}

/// Execute AgentPay instruction helper
pub fn execute_agent_pay(
    agent_id: u64,
    asset_id: AssetId,
    to: Address,
    amount: u128,
    balances: &mut AgentBalances,
    owner: Address,
    protocol_balances: &mut BalanceState,
) -> Result<(), AgentError> {
    // Deduct from agent balance
    balances.deduct(owner, agent_id, asset_id, amount)?;

    // Credit to protocol balance of recipient
    let _ = protocol_balances.credit_balance(asset_id, to, amount);

    Ok(())
}

/// Execute AgentBatchPay instruction helper
pub fn execute_agent_batch_pay(
    agent_id: u64,
    payments: &[(AssetId, Address, u128)],
    balances: &mut AgentBalances,
    owner: Address,
    protocol_balances: &mut BalanceState,
) -> Result<(), AgentError> {
    for &(asset_id, to, amount) in payments {
        execute_agent_pay(agent_id, asset_id, to, amount, balances, owner, protocol_balances)?;
    }
    Ok(())
}

/// Execute AgentCall instruction helper (delegate to EVM)
pub fn execute_agent_call(
    _agent_id: u64,
    target: Address,
    data: Vec<u8>,
    evm_state: &mut EvmState,
    evm_executor: &EvmExecutor,
    caller: Address,
) -> Result<call_evm::EvmExecutionResult, AgentError> {
    let tx = EvmTransaction {
        caller,
        nonce: 0,
        gas_limit: 1_000_000,
        gas_price: 0,
        to: Some(target),
        value: alloy_primitives::U256::ZERO,
        data: Bytes::from(data),
        chain_id: evm_executor.chain_id,
    };

    evm_executor.execute_tx(tx, evm_state)
        .map_err(|e| AgentError::ExecutionFailed(format!("{:?}", e)))
}

/// Execute AgentBridgeDeposit instruction helper
pub fn execute_agent_bridge_deposit(
    agent_id: u64,
    asset_id: AssetId,
    amount: u128,
    _target_chain: u64,
    target_address: Vec<u8>,
    balances: &mut AgentBalances,
    owner: Address,
    protocol_balances: &mut BalanceState,
    evm_state: &mut EvmState,
    evm_executor: &EvmExecutor,
    bridge_state: &mut BridgeStateManager,
    config: &BridgeConfig,
    registry: &AssetRegistry,
    bridge_address: Address,
    current_block: u64,
) -> Result<(), AgentError> {
    // 1. Deduct from agent balance
    balances.deduct(owner, agent_id, asset_id, amount)?;

    // 2. Deduct from protocol balance (agent -> protocol)
    protocol_balances
        .deduct_balance(asset_id, owner, amount)
        .map_err(|_| AgentError::ExecutionFailed("insufficient protocol balance".into()))?;

    // 3. Bridge deposit to EVM
    let op = call_bridge::BridgeOp::DepositToEvm {
        asset_id,
        from: owner,
        to: Address::from_slice(&target_address),
        amount,
    };

    let result = bridge_execute_deposit(
        &op,
        protocol_balances,
        evm_state,
        evm_executor,
        bridge_state,
        config,
        registry,
        bridge_address,
        owner,
        current_block,
    );

    match result {
        Ok(exec) => {
            if exec.success {
                Ok(())
            } else {
                // Restore balances on failure
                let _ = balances.credit(owner, agent_id, asset_id, amount);
                let _ = protocol_balances.credit_balance(asset_id, owner, amount);
                Err(AgentError::ExecutionFailed("bridge deposit failed".into()))
            }
        }
        Err(e) => {
            // Restore balances on failure
            let _ = balances.credit(owner, agent_id, asset_id, amount);
            let _ = protocol_balances.credit_balance(asset_id, owner, amount);
            Err(AgentError::ExecutionFailed(format!("{:?}", e)))
        }
    }
}

/// Extract signature from auth scheme
fn extract_signature(auth: &call_protocol::AuthScheme) -> [u8; 65] {
    match auth {
        call_protocol::AuthScheme::SingleSig { signature } => *signature,
        call_protocol::AuthScheme::MultiSig { signatures } => signatures[0],
        call_protocol::AuthScheme::SessionKey { signature, .. } => *signature,
    }
}

/// Compute agent transaction hash for signature verification
fn compute_agent_tx_hash(
    tx: &call_protocol::ProtocolTransaction,
    agent_id: u64,
) -> [u8; 32] {
    use call_crypto::keccak256;
    let mut buf = Vec::new();
    buf.extend_from_slice(&agent_id.to_be_bytes());
    buf.extend_from_slice(&tx.nonce.to_be_bytes());
    buf.extend_from_slice(&tx.gas_limit.to_be_bytes());
    buf.extend_from_slice(&tx.max_fee.to_be_bytes());
    buf.extend_from_slice(&tx.expires_at.to_be_bytes());
    keccak256(&buf).0
}

/// Extract asset_id, counterparty, amount from an instruction.
/// Returns a Vec so that multi-payment instructions (BatchTransfer, AgentBatchPay)
/// are fully checked.
fn extract_instruction_details(instr: &Instruction) -> Vec<(AssetId, Address, u128)> {
    match instr {
        Instruction::Transfer { asset_id, to, amount, .. } => vec![(*asset_id, *to, *amount)],
        Instruction::BatchTransfer { asset_id, payments, .. } => {
            payments.iter().map(|p| (*asset_id, p.to, p.amount)).collect()
        }
        Instruction::Approve { asset_id, spender, amount } => vec![(*asset_id, *spender, *amount)],
        Instruction::TransferFrom { asset_id, from, to: _, amount } => vec![(*asset_id, *from, *amount)],
        Instruction::AgentPay { payment } => vec![(payment.asset_id, payment.to, payment.amount)],
        Instruction::AgentBatchPay { payments } => {
            payments.iter().map(|p| (p.asset_id, p.to, p.amount)).collect()
        }
        Instruction::AgentBridgeDeposit { asset_id, amount, target_address, .. } => {
            // For bridge deposit, use target chain address as counterparty proxy
            let addr = if target_address.len() >= 20 {
                Address::from_slice(&target_address[..20])
            } else {
                Address::ZERO
            };
            vec![(*asset_id, addr, *amount)]
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_addr;
    use call_crypto::generate_keypair;
    use call_bridge::BridgeConfig;

    fn setup_registry() -> AssetRegistry {
        let mut registry = AssetRegistry::new();
        registry
            .register_asset("TEST".into(), "Test".into(), 18, test_addr(1), 0)
            .ok();
        registry
    }

    fn make_agent_registration(owner: Address) -> AgentRegistration {
        let (_, _) = generate_keypair();
        let (_, pubkey) = generate_keypair();
        AgentRegistration {
            agent_id: 0,
            owner,
            agent_public_key: pubkey,
            name: "test-agent".into(),
            url: "https://test.com".into(),
            metadata_hash: [0u8; 32],
            domain_proof: None,
            domain_verified: false,
            registered_at: 0,
            permissions: AgentPermissions::default(),
        }
    }

    #[test]
    fn test_extract_instruction_details_transfer() {
        let instr = Instruction::Transfer {
            asset_id: 1,
            to: test_addr(2),
            amount: 500,
            memo: None,
        };
        let details = extract_instruction_details(&instr);
        assert_eq!(details.len(), 1);
        let (asset_id, counterparty, amount) = details[0];
        assert_eq!(asset_id, 1);
        assert_eq!(counterparty, test_addr(2));
        assert_eq!(amount, 500);
    }

    #[test]
    fn test_extract_instruction_details_agent_pay() {
        let instr = Instruction::AgentPay {
            payment: call_protocol::instructions::AgentPayment {
                agent_id: 0,
                asset_id: 1,
                to: test_addr(3),
                amount: 200,
            },
        };
        let details = extract_instruction_details(&instr);
        assert_eq!(details.len(), 1);
        let (asset_id, counterparty, amount) = details[0];
        assert_eq!(asset_id, 1);
        assert_eq!(counterparty, test_addr(3));
        assert_eq!(amount, 200);
    }

    #[test]
    fn test_extract_instruction_details_agent_bridge_deposit() {
        let target_addr = test_addr(5);
        let instr = Instruction::AgentBridgeDeposit {
            agent_id: 0,
            asset_id: 1,
            amount: 100,
            target_chain: 1,
            target_address: target_addr.as_slice().to_vec(),
        };
        let details = extract_instruction_details(&instr);
        assert_eq!(details.len(), 1);
        let (asset_id, counterparty, amount) = details[0];
        assert_eq!(asset_id, 1);
        assert_eq!(counterparty, target_addr);
        assert_eq!(amount, 100);
    }

    #[test]
    fn test_extract_instruction_details_batch_transfer_all_payments() {
        let instr = Instruction::BatchTransfer {
            asset_id: 1,
            payments: vec![
                call_protocol::instructions::PaymentEntry { to: test_addr(2), amount: 100, memo: None },
                call_protocol::instructions::PaymentEntry { to: test_addr(3), amount: 200, memo: None },
                call_protocol::instructions::PaymentEntry { to: test_addr(4), amount: 300, memo: None },
            ],
        };
        let details = extract_instruction_details(&instr);
        assert_eq!(details.len(), 3);
        assert_eq!(details[0], (1, test_addr(2), 100));
        assert_eq!(details[1], (1, test_addr(3), 200));
        assert_eq!(details[2], (1, test_addr(4), 300));
    }

    #[test]
    fn test_agent_pay_success() {
        let mut agent_balances = AgentBalances::new();
        let mut protocol_balances = BalanceState::new();
        let owner = test_addr(1);
        protocol_balances.balances.set_balance(1, owner, 1000).unwrap();

        agent_balances.grant_funds(owner, 0, 1, 1000, &mut protocol_balances).unwrap();

        execute_agent_pay(
            0, 1, test_addr(2), 500,
            &mut agent_balances, owner,
            &mut protocol_balances,
        ).unwrap();

        assert_eq!(agent_balances.get_balance(owner, 0, 1), 500);
        assert_eq!(protocol_balances.get_balance(1, &test_addr(2)), 500);
    }

    #[test]
    fn test_agent_pay_insufficient_balance() {
        let mut agent_balances = AgentBalances::new();
        let mut protocol_balances = BalanceState::new();
        let owner = test_addr(1);
        protocol_balances.balances.set_balance(1, owner, 100).unwrap();

        agent_balances.grant_funds(owner, 0, 1, 100, &mut protocol_balances).unwrap();

        let result = execute_agent_pay(
            0, 1, test_addr(2), 200,
            &mut agent_balances, owner,
            &mut protocol_balances,
        );
        assert!(matches!(result, Err(AgentError::InsufficientAgentBalance(0, 200))));
    }

    #[test]
    fn test_agent_batch_pay() {
        let mut agent_balances = AgentBalances::new();
        let mut protocol_balances = BalanceState::new();
        let owner = test_addr(1);
        protocol_balances.balances.set_balance(1, owner, 3000).unwrap();

        agent_balances.grant_funds(owner, 0, 1, 3000, &mut protocol_balances).unwrap();

        let payments = vec![
            (1, test_addr(2), 1000),
            (1, test_addr(3), 1500),
        ];

        execute_agent_batch_pay(
            0, &payments,
            &mut agent_balances, owner,
            &mut protocol_balances,
        ).unwrap();

        assert_eq!(agent_balances.get_balance(owner, 0, 1), 500);
        assert_eq!(protocol_balances.get_balance(1, &test_addr(2)), 1000);
        assert_eq!(protocol_balances.get_balance(1, &test_addr(3)), 1500);
    }

    #[test]
    fn test_agent_update_config() {
        let config = AgentFeeConfig {
            fee_payer: crate::FeePayer::OwnerPays,
            owner_max_daily_fee: 100_000,
            owner_max_total_fee: 1_000_000,
            require_owner_signature_above: 5000,
        };
        assert_eq!(config.owner_max_daily_fee, 100_000);
        assert_eq!(config.require_owner_signature_above, 5000);
    }

    #[test]
    fn test_agent_bridge_deposit_flow() {
        let mut agent_balances = AgentBalances::new();
        let mut protocol_balances = BalanceState::new();
        protocol_balances.balances.set_balance(1, test_addr(1), 1000).unwrap();
        let mut evm_state = EvmState::new();
        let evm_executor = EvmExecutor::new(1);
        let mut bridge_state = BridgeStateManager::default();
        let config = BridgeConfig::default();
        let registry = setup_registry();
        let owner = test_addr(1);

        agent_balances.grant_funds(owner, 0, 1, 500, &mut protocol_balances).unwrap();

        let target_addr = test_addr(2);
        let result = execute_agent_bridge_deposit(
            0, 1, 200,
            1, target_addr.as_slice().to_vec(),
            &mut agent_balances, owner,
            &mut protocol_balances,
            &mut evm_state, &evm_executor,
            &mut bridge_state, &config, &registry,
            test_addr(0xCC),
            100,
        );

        // EVM bridge will fail (no contract at address), but agent balance should be restored
        assert!(result.is_err());
        // Balance should be restored after failure
        assert_eq!(agent_balances.get_balance(owner, 0, 1), 500);
    }

    #[test]
    fn test_compute_agent_tx_hash() {
        let agent = make_agent_registration(test_addr(1));
        let (_secret, _) = generate_keypair();

        let tx = call_protocol::ProtocolTransaction {
            sender: agent.owner,
            nonce: 0,
            instructions: vec![Instruction::Transfer {
                asset_id: 1,
                to: test_addr(2),
                amount: 100,
                memo: None,
            }],
            expires_at: 0,
            auth: call_protocol::AuthScheme::SingleSig { signature: [0u8; 65] },
            gas_config: call_protocol::GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1000,
        };

        let hash = compute_agent_tx_hash(&tx, agent.agent_id);
        assert_eq!(hash.len(), 32);

        // Deterministic: same inputs = same hash
        let hash2 = compute_agent_tx_hash(&tx, agent.agent_id);
        assert_eq!(hash, hash2);
    }
}
