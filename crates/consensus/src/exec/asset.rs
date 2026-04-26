use crate::ConsensusError;
use call_governance::GovernanceManager;
use call_protocol::account::AccountState;
use call_protocol::instructions::{Instruction, InstructionResult};
use call_protocol::registry::{AssetRegistry, AssetStatus};

// ── Asset instruction helpers ─────────────────────────────────────────

pub(crate) fn is_asset_instruction(instr: &Instruction) -> bool {
    matches!(instr, Instruction::RegisterAsset { .. } | Instruction::EvmIssuerMint { .. })
}

pub(crate) fn execute_asset_instruction(
    instruction: &Instruction,
    sender: call_primitives::Address,
    account: &mut AccountState,
    registry: &mut AssetRegistry,
    governance: Option<&mut GovernanceManager>,
    evm_state: &mut call_evm::EvmState,
    evm_executor: &call_evm::EvmExecutor,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    match instruction {
        Instruction::RegisterAsset {
            symbol,
            name,
            decimals,
            max_supply,
        } => {
            // 1. Collect asset registration fee
            let fee = {
                let gov = governance.as_ref().ok_or_else(|| {
                    ConsensusError::InvalidBlock("governance not available".into())
                })?;
                gov.config.asset_registration_fee
            };
            if fee > 0 {
                let sender_balance = account.get_balance(call_protocol::CALL_ASSET_ID, &sender);
                if sender_balance < fee {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "RegisterAsset: insufficient CALL balance for fee: need {fee}, have {sender_balance}"
                    )));
                }
                account
                    .deduct_balance(call_protocol::CALL_ASSET_ID, sender, fee)
                    .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAsset: {e}")))?;
            }

            // 2. Register asset
            let asset_id = registry
                .register_asset(symbol.clone(), name.clone(), *decimals, sender, 0, current_block_height, *max_supply)
                .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAsset: {e}")))?;

            // 3. Deploy EVM wrapped token via system deployer
            let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
            evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
            evm_state.create_account(deployer);

            let (contract_addr, deploy_result) = evm_executor
                .deploy_erc20_template(
                    deployer,
                    evm_state,
                    name,
                    symbol,
                    *decimals,
                    call_protocol::BRIDGE_EVM_ADDRESS,
                    sender,
                    call_primitives::U256::from(*max_supply),
                    call_primitives::U256::from(asset_id),
                )
                .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAsset: ERC-20 deploy failed: {e:?}")))?;

            if !deploy_result.success {
                return Err(ConsensusError::InvalidBlock(
                    "RegisterAsset: ERC-20 deployment reverted".into(),
                ));
            }

            // 4. Bind contract address
            registry.set_evm_contract_address(asset_id, contract_addr);

            Ok(InstructionResult::Success)
        }
        Instruction::EvmIssuerMint {
            asset_id,
            to,
            amount,
        } => {
            // 1. Asset must exist and be active
            let asset = registry
                .get_asset(*asset_id)
                .ok_or_else(|| {
                    ConsensusError::InvalidBlock(format!(
                        "EvmIssuerMint: asset {} not found",
                        asset_id
                    ))
                })?;
            if asset.status != AssetStatus::Active {
                return Err(ConsensusError::InvalidBlock(format!(
                    "EvmIssuerMint: asset {} is not active (status: {:?})",
                    asset_id, asset.status
                )));
            }

            // 2. Only issuer can mint
            if asset.issuer != sender {
                return Err(ConsensusError::InvalidBlock(
                    "EvmIssuerMint: caller is not asset issuer".into(),
                ));
            }

            // 3. CALL (asset_id == 1) has no wrapped ERC-20 contract
            if *asset_id == call_protocol::CALL_ASSET_ID {
                return Err(ConsensusError::InvalidBlock(
                    "EvmIssuerMint: CALL asset has no EVM wrapped token".into(),
                ));
            }

            // 4. Cap check (read-only on registry)
            if asset.would_exceed_cap(*amount) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "EvmIssuerMint: cap exceeded for asset {}",
                    asset_id
                )));
            }

            // 5. Must have an EVM contract address
            let contract_addr = asset.evm_contract_address.ok_or_else(|| {
                ConsensusError::InvalidBlock(format!(
                    "EvmIssuerMint: no EVM contract registered for asset {}",
                    asset_id
                ))
            })?;

            // 6. Execute EVM issuerMint
            let amount_u256 = call_evm::U256::from(*amount);
            let mint_result = evm_executor
                .evm_call_issuer_mint(sender, contract_addr, evm_state, *to, amount_u256)
                .map_err(|e| {
                    ConsensusError::InvalidBlock(format!(
                        "EvmIssuerMint: EVM call failed: {e:?}"
                    ))
                })?;

            if !mint_result.success {
                return Err(ConsensusError::InvalidBlock(
                    "EvmIssuerMint: EVM issuerMint reverted".into(),
                ));
            }

            // 7. Update evm_supply (cap already verified)
            registry
                .add_evm_supply(*asset_id, *amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("EvmIssuerMint: {e}")))?;

            Ok(InstructionResult::Success)
        }
        _ => Err(ConsensusError::InvalidBlock("not an asset instruction".into())),
    }
}
