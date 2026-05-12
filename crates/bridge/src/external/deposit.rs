//! External bridge deposit logic.

use crate::external::types::ExternalBridgeOp;
use crate::{BridgeConfig, BridgeError};
use alloy_primitives::{Address, B256};
use call_primitives::U256;
use call_protocol::storage_backend::StorageBackend;

/// Result of processing an external deposit.
#[derive(Debug, Clone)]
pub enum ExternalDepositResult {
    /// Deposit is queued for the challenge period.
    Queued {
        source_tx_hash: B256,
        challenge_period_blocks: u64,
        finalized_at_block: u64,
    },
}

// ── EVM bridge state helpers ──────────────────────────────────────────

const BRIDGE_ADDRESS: alloy_primitives::Address =
    alloy_primitives::address!("0000000000000000000000000000000000000103");

fn slot_bridge_processed(tx_hash: [u8; 32]) -> U256 {
    call_precompile::storage::storage_slot(&[b"processed", &tx_hash])
}

fn slot_bridge_pending_count() -> U256 {
    call_precompile::storage::storage_slot(&[b"pending_count"])
}

fn slot_bridge_pending_hash(index: u64) -> U256 {
    call_precompile::storage::storage_slot(&[b"pending_list"]) + U256::from(index)
}

fn slot_bridge_pending_status(tx_hash: [u8; 32]) -> U256 {
    call_precompile::storage::storage_slot(&[b"pending_status", &tx_hash])
}

fn slot_bridge_pending_recipient(tx_hash: [u8; 32]) -> U256 {
    call_precompile::storage::storage_slot(&[b"pending_recipient", &tx_hash])
}

fn slot_bridge_pending_asset(tx_hash: [u8; 32]) -> U256 {
    call_precompile::storage::storage_slot(&[b"pending_asset", &tx_hash])
}

fn slot_bridge_pending_amount(tx_hash: [u8; 32]) -> U256 {
    call_precompile::storage::storage_slot(&[b"pending_amount", &tx_hash])
}

fn slot_bridge_pending_block(tx_hash: [u8; 32]) -> U256 {
    call_precompile::storage::storage_slot(&[b"pending_block", &tx_hash])
}

fn slot_bridge_daily_used(asset_id: u64) -> U256 {
    call_precompile::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"daily"])
}

fn slot_bridge_daily_day(asset_id: u64) -> U256 {
    call_precompile::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"daily_day"])
}

fn slot_bridge_external_paused() -> U256 {
    call_precompile::storage::storage_slot(&[b"external_paused"])
}

fn read_bridge_processed<B: StorageBackend>(backend: &mut B, tx_hash: [u8; 32]) -> bool {
    backend.load(BRIDGE_ADDRESS, slot_bridge_processed(tx_hash)) != U256::ZERO
}

fn read_bridge_pending_count<B: StorageBackend>(backend: &mut B) -> u64 {
    call_precompile::u256_to_u64(backend.load(BRIDGE_ADDRESS, slot_bridge_pending_count()))
}

fn read_bridge_daily_used<B: StorageBackend>(backend: &mut B, asset_id: u64) -> u128 {
    call_precompile::u256_to_u128(backend.load(BRIDGE_ADDRESS, slot_bridge_daily_used(asset_id)))
}

fn read_bridge_daily_day<B: StorageBackend>(backend: &mut B, asset_id: u64) -> u64 {
    call_precompile::u256_to_u64(backend.load(BRIDGE_ADDRESS, slot_bridge_daily_day(asset_id)))
}

fn read_bridge_external_paused<B: StorageBackend>(backend: &mut B) -> bool {
    backend.load(BRIDGE_ADDRESS, slot_bridge_external_paused()) != U256::ZERO
}

fn seed_bridge_pending<B: StorageBackend>(
    backend: &mut B,
    tx_hash: [u8; 32],
    recipient: Address,
    asset_id: u64,
    amount: u128,
    block: u64,
) {
    let count = read_bridge_pending_count(backend);
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_pending_hash(count),
        U256::from_be_slice(&tx_hash),
    );
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_pending_count(),
        call_precompile::u64_to_u256(count + 1),
    );
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_pending_status(tx_hash),
        U256::from(1u8),
    );
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_pending_recipient(tx_hash),
        call_precompile::address_to_u256(recipient),
    );
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_pending_asset(tx_hash),
        call_precompile::u64_to_u256(asset_id),
    );
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_pending_amount(tx_hash),
        call_precompile::u128_to_u256(amount),
    );
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_pending_block(tx_hash),
        call_precompile::u64_to_u256(block),
    );
}

fn seed_bridge_processed<B: StorageBackend>(backend: &mut B, tx_hash: [u8; 32], block_height: u64) {
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_processed(tx_hash),
        call_precompile::u64_to_u256(block_height),
    );
}

fn update_bridge_daily<B: StorageBackend>(backend: &mut B, asset_id: u64, used: u128, day: u64) {
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_daily_used(asset_id),
        call_precompile::u128_to_u256(used),
    );
    backend.store(
        BRIDGE_ADDRESS,
        slot_bridge_daily_day(asset_id),
        call_precompile::u64_to_u256(day),
    );
}

/// Check and update daily limit using EVM storage.
fn check_and_update_daily_limit<B: StorageBackend>(
    backend: &mut B,
    asset_id: u64,
    amount: u128,
    daily_limit: u128,
    current_block: u64,
    blocks_per_day: u64,
) -> Result<(), BridgeError> {
    let reset_at = read_bridge_daily_day(backend, asset_id);
    let used = if current_block >= reset_at + blocks_per_day {
        0
    } else {
        read_bridge_daily_used(backend, asset_id)
    };
    if used + amount > daily_limit {
        return Err(BridgeError::ExceedsDailyLimit(asset_id, used, daily_limit));
    }
    let new_day = if current_block >= reset_at + blocks_per_day {
        current_block
    } else {
        reset_at
    };
    update_bridge_daily(backend, asset_id, used + amount, new_day);
    Ok(())
}

/// EVM-based version of external deposit processing.
/// Writes all state (replay protection, daily limit, pending queue) to EVM storage.
pub fn process_external_deposit_evm<B: StorageBackend>(
    op: &ExternalBridgeOp,
    backend: &mut B,
    config: &BridgeConfig,
    validators: &[Address],
    current_block: u64,
    source_contract: Option<Address>,
) -> Result<ExternalDepositResult, BridgeError> {
    let ExternalBridgeOp::Deposit {
        source_chain,
        source_tx_hash,
        asset_id,
        recipient,
        amount,
        signatures: _,
        ..
    } = op
    else {
        return Err(BridgeError::EvmExecutionFailed("not a deposit op".into()));
    };

    if read_bridge_external_paused(backend) {
        return Err(BridgeError::ExternalBridgePaused);
    }

    if let Some(contract) = source_contract {
        super::types::verify_bridge_contract(config, source_chain.chain_id(), &contract)?;
    }

    if !config.allowed_assets.contains(asset_id) {
        return Err(BridgeError::ExternalAssetNotAllowed(*asset_id));
    }

    if read_bridge_processed(backend, **source_tx_hash) {
        return Err(BridgeError::EvmExecutionFailed(
            "source tx already processed".into(),
        ));
    }

    super::types::verify_bridge_signatures(op, validators, config.min_validator_signatures)?;

    if *amount > config.max_per_tx {
        return Err(BridgeError::ExceedsMaxPerTx(
            *asset_id,
            *amount,
            config.max_per_tx,
        ));
    }

    check_and_update_daily_limit(
        backend,
        *asset_id,
        *amount,
        config.daily_limit_per_asset,
        current_block,
        config.blocks_per_day,
    )?;

    let fee = config.bridge_fee;
    let net_amount = if fee >= *amount {
        return Err(BridgeError::BridgeFeeExceedsAmount(fee, *amount));
    } else {
        amount - fee
    };

    seed_bridge_pending(
        backend,
        **source_tx_hash,
        *recipient,
        *asset_id,
        net_amount,
        current_block,
    );
    seed_bridge_processed(backend, **source_tx_hash, current_block);

    Ok(ExternalDepositResult::Queued {
        source_tx_hash: *source_tx_hash,
        challenge_period_blocks: config.challenge_period_blocks,
        finalized_at_block: current_block + config.challenge_period_blocks,
    })
}

/// EVM-based version of light client deposit processing.
#[cfg(feature = "light-client-bridge")]
pub fn process_light_client_deposit_evm<B: StorageBackend>(
    light_client: &mut call_light_client::EthLightClient,
    op: &ExternalBridgeOp,
    backend: &mut B,
    config: &BridgeConfig,
    current_block: u64,
) -> Result<ExternalDepositResult, BridgeError> {
    let ExternalBridgeOp::LightClientDeposit {
        source_chain: _,
        header,
        tx_proof,
        receipt_proof,
        recipient,
        asset_id,
        amount,
    } = op
    else {
        return Err(BridgeError::EvmExecutionFailed(
            "not a light client deposit op".into(),
        ));
    };

    light_client
        .submit_header(header.clone())
        .map_err(|e| BridgeError::MptProofError(e.to_string()))?;

    let block_number = header
        .number()
        .ok_or_else(|| BridgeError::MptProofError("header missing block number".into()))?;

    let tx_hash = header.block_hash;
    light_client
        .verify_tx_inclusion(block_number, tx_hash, tx_proof)
        .map_err(|e| BridgeError::MptProofError(e.to_string()))?;

    let bridge_event = light_client
        .verify_receipt_and_parse_bridge_event(block_number, receipt_proof)
        .map_err(|e| BridgeError::MptProofError(e.to_string()))?;

    // Verify the deposit block has been consensus-finalized by beacon chain BLS.
    // Without this check, an attacker could construct a valid parent-hash chain
    // that was never accepted by Ethereum consensus.
    if !light_client.is_consensus_verified(block_number) {
        return Err(BridgeError::NotConsensusVerified(block_number));
    }

    if bridge_event.recipient != *recipient {
        return Err(BridgeError::MptProofError("recipient mismatch".into()));
    }
    if bridge_event.asset_id != *asset_id {
        return Err(BridgeError::MptProofError("asset_id mismatch".into()));
    }
    if bridge_event.amount != *amount {
        return Err(BridgeError::MptProofError("amount mismatch".into()));
    }

    let source_tx_hash = bridge_event.source_tx_hash;

    if !config.allowed_assets.contains(asset_id) {
        return Err(BridgeError::ExternalAssetNotAllowed(*asset_id));
    }

    if read_bridge_processed(backend, *source_tx_hash) {
        return Err(BridgeError::EvmExecutionFailed(
            "source tx already processed".into(),
        ));
    }

    if *amount > config.max_per_tx {
        return Err(BridgeError::ExceedsMaxPerTx(
            *asset_id,
            *amount,
            config.max_per_tx,
        ));
    }

    check_and_update_daily_limit(
        backend,
        *asset_id,
        *amount,
        config.daily_limit_per_asset,
        current_block,
        config.blocks_per_day,
    )?;

    seed_bridge_pending(
        backend,
        *source_tx_hash,
        *recipient,
        *asset_id,
        *amount,
        current_block,
    );
    seed_bridge_processed(backend, *source_tx_hash, current_block);

    Ok(ExternalDepositResult::Queued {
        source_tx_hash,
        challenge_period_blocks: config.challenge_period_blocks,
        finalized_at_block: current_block + config.challenge_period_blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{keccak256, Address, B256};
    use call_light_client::{EthHeader, GenesisState, MptProofNode, ReceiptProof, TxInclusionProof};
    use crate::ExternalChain;

    // ── Mock storage backend ─────────────────────────────────────────────

    #[derive(Default)]
    struct MockStorage {
        slots: std::collections::HashMap<(Address, U256), U256>,
    }

    impl StorageBackend for MockStorage {
        fn load(&mut self, address: Address, slot: U256) -> U256 {
            self.slots.get(&(address, slot)).copied().unwrap_or(U256::ZERO)
        }
        fn store(&mut self, address: Address, slot: U256, value: U256) {
            self.slots.insert((address, slot), value);
        }
    }

    // ── Minimal RLP helpers for test construction ────────────────────────

    fn rlp_short(data: &[u8]) -> Vec<u8> {
        if data.len() == 1 && data[0] < 0x80 {
            data.to_vec()
        } else if data.len() < 56 {
            let mut out = vec![0x80 + data.len() as u8];
            out.extend_from_slice(data);
            out
        } else {
            let len_bytes = data.len().to_be_bytes();
            let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(8);
            let num = 8 - skip;
            let mut out = vec![0xB7 + num as u8];
            out.extend_from_slice(&len_bytes[skip..]);
            out.extend_from_slice(data);
            out
        }
    }

    fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
        let total: usize = items.iter().map(|i| i.len()).sum();
        let mut out = Vec::with_capacity(1 + total);
        if total < 56 {
            out.push(0xC0 + total as u8);
        } else {
            let len_bytes = total.to_be_bytes();
            let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(8);
            let num = 8 - skip;
            out.push(0xF7 + num as u8);
            out.extend_from_slice(&len_bytes[skip..]);
        }
        for item in items {
            out.extend_from_slice(item);
        }
        out
    }

    fn rlp_u64(v: u64) -> Vec<u8> {
        if v == 0 {
            vec![0x80]
        } else {
            let bytes = v.to_be_bytes();
            let start = bytes.iter().position(|&b| b != 0).unwrap_or(8);
            let data = &bytes[start..];
            if data.len() == 1 && data[0] < 0x80 {
                data.to_vec()
            } else {
                let mut out = vec![0x80 + data.len() as u8];
                out.extend_from_slice(data);
                out
            }
        }
    }

    fn rlp_u128(v: u128) -> Vec<u8> {
        if v == 0 {
            vec![0x80]
        } else {
            let bytes = v.to_be_bytes();
            let start = bytes.iter().position(|&b| b != 0).unwrap_or(16);
            let data = &bytes[start..];
            if data.len() == 1 && data[0] < 0x80 {
                data.to_vec()
            } else {
                let mut out = vec![0x80 + data.len() as u8];
                out.extend_from_slice(data);
                out
            }
        }
    }

    // ── MPT leaf builder (compact encoding) ──────────────────────────────

    fn compact_encode(nibbles: &[u8], is_leaf: bool) -> Vec<u8> {
        let odd = nibbles.len() % 2 != 0;
        let first = if odd {
            let ty = if is_leaf { 3 } else { 1 };
            (nibbles[0] << 4) | ty
        } else if is_leaf {
            0x20
        } else {
            0x00
        };
        let mut out = vec![first];
        let start = if odd { 1 } else { 0 };
        for i in (start..nibbles.len()).step_by(2) {
            if i + 1 < nibbles.len() {
                out.push((nibbles[i] << 4) | nibbles[i + 1]);
            }
        }
        out
    }

    fn make_leaf_node_rlp(key_nibbles: &[u8], value: &[u8]) -> Vec<u8> {
        let compact = compact_encode(key_nibbles, true);
        rlp_list(&[rlp_short(&compact), rlp_short(value)])
    }

    // ── Header builder ───────────────────────────────────────────────────

    fn make_test_header_rlp(
        parent_hash: B256,
        block_number: u64,
        tx_root: B256,
        receipt_root: B256,
    ) -> Vec<u8> {
        let mut fields: Vec<Vec<u8>> = Vec::new();
        fields.push(rlp_short(&parent_hash.0)); // 0 parent_hash
        fields.push(vec![0x80]); // 1 sha3_uncles
        fields.push(vec![0x80]); // 2 miner
        fields.push(rlp_short(&[0u8; 32])); // 3 state_root
        fields.push(rlp_short(&tx_root.0)); // 4 transactions_root
        fields.push(rlp_short(&receipt_root.0)); // 5 receipts_root
        fields.push(vec![0x80]); // 6 logs_bloom
        fields.push(rlp_short(&[0x01])); // 7 difficulty
        fields.push(rlp_u64(block_number)); // 8 number
        fields.push(rlp_short(&[0x01])); // 9 gas_limit
        fields.push(vec![0x80]); // 10 gas_used
        fields.push(rlp_short(&[0x01])); // 11 timestamp
        fields.push(vec![0x80]); // 12 extra_data
        fields.push(rlp_short(&[0u8; 32])); // 13 mix_hash
        fields.push(vec![0x88, 0, 0, 0, 0, 0, 0, 0, 0]); // 14 nonce
        fields.push(vec![0x80]); // 15 base_fee
        rlp_list(&fields)
    }

    // ── Bridge event receipt builder ─────────────────────────────────────

    fn make_bridge_receipt_rlp(
        recipient: Address,
        source_tx_hash: B256,
        asset_id: u64,
        amount: u128,
    ) -> Vec<u8> {
        // BridgeDeposit event signature
        let event_sig = B256::new([
            0x3a, 0x95, 0x7f, 0x16, 0x8e, 0x13, 0xa0, 0x27, 0xb5, 0x3e, 0x7f, 0x6f, 0xe9, 0x58,
            0xa0, 0xbc, 0xa9, 0x0d, 0x51, 0x5b, 0x66, 0xbf, 0x4b, 0x5a, 0x6c, 0x61, 0x60, 0x84,
            0x79, 0x13, 0x54, 0x06,
        ]);

        // Topics: [event_sig, source_tx_hash, recipient_padded]
        let mut recipient_padded = [0u8; 32];
        recipient_padded[12..].copy_from_slice(recipient.as_slice());
        let topics_rlp = rlp_list(&[
            rlp_short(&event_sig.0),
            rlp_short(&source_tx_hash.0),
            rlp_short(&recipient_padded),
        ]);

        // Data: [source_chain, source_block, sender, asset_id, amount]
        let data_rlp = rlp_list(&[
            rlp_short(&[0x01]),                   // source_chain = 1
            rlp_u64(60003),                       // source_block
            vec![0x80],                           // sender (empty)
            rlp_u64(asset_id),                     // asset_id from parameter
            rlp_u128(amount),                     // amount
        ]);

        // Log: [address, topics, data]
        let address = vec![0xC0u8; 20];
        let log_rlp = rlp_list(&[rlp_short(&address), topics_rlp, data_rlp]);
        let logs_rlp = rlp_list(&[log_rlp]);

        // Receipt: [status, gas_used, bloom, logs]
        rlp_list(&[
            rlp_short(&[0x01]),       // status
            rlp_short(&[0x52, 0x08]), // gas_used = 21000
            vec![0x80],               // bloom
            logs_rlp,
        ])
    }

    #[test]
    fn test_light_client_deposit_rejects_unverified_block() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut light_client = call_light_client::EthLightClient::init(genesis);

        let recipient = Address::repeat_byte(0x42);
        let source_tx_hash = B256::repeat_byte(0xAB);
        let asset_id = 1u64;
        let amount = 1000u128;

        // Build receipt with bridge event
        let receipt_rlp = make_bridge_receipt_rlp(recipient, source_tx_hash, asset_id, amount);
        let receipt_leaf = make_leaf_node_rlp(&[], &receipt_rlp);
        let receipt_root = keccak256(&receipt_leaf);

        // Build tx leaf (single tx trie, empty key matches any key)
        let tx_hash = B256::repeat_byte(0xDE);
        let tx_leaf = make_leaf_node_rlp(&[], &tx_hash.0);
        let tx_root = keccak256(&tx_leaf);

        // Build header
        let header_rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
        let header = EthHeader::from_rlp(header_rlp);

        let tx_proof = TxInclusionProof::new(vec![MptProofNode::new(tx_leaf)]);
        let receipt_proof = ReceiptProof::new(0, vec![MptProofNode::new(receipt_leaf)]);

        let op = ExternalBridgeOp::LightClientDeposit {
            source_chain: ExternalChain::EthereumMainnet,
            header,
            tx_proof,
            receipt_proof,
            recipient,
            asset_id,
            amount,
        };

        let config = BridgeConfig {
            allowed_assets: vec![1],
            ..Default::default()
        };
        let mut backend = MockStorage::default();

        // Block 1001 has NOT been consensus-verified (no finalized block set)
        let result = process_light_client_deposit_evm(
            &mut light_client,
            &op,
            &mut backend,
            &config,
            1, // current Callchain block
        );

        assert!(
            matches!(result, Err(BridgeError::NotConsensusVerified(1001))),
            "expected NotConsensusVerified(1001), got {:?}",
            result
        );
    }

    #[test]
    fn test_light_client_deposit_accepts_verified_block() {
        let anchor = B256::repeat_byte(0xAA);
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: 1000,
            state_root: B256::ZERO,
        };
        let mut light_client = call_light_client::EthLightClient::init(genesis);

        let recipient = Address::repeat_byte(0x42);
        let source_tx_hash = B256::repeat_byte(0xAB);
        let asset_id = 1u64;
        let amount = 1000u128;

        // Build receipt with bridge event
        let receipt_rlp = make_bridge_receipt_rlp(recipient, source_tx_hash, asset_id, amount);
        let receipt_leaf = make_leaf_node_rlp(&[], &receipt_rlp);
        let receipt_root = keccak256(&receipt_leaf);

        let tx_hash = B256::repeat_byte(0xDE);
        let tx_leaf = make_leaf_node_rlp(&[], &tx_hash.0);
        let tx_root = keccak256(&tx_leaf);

        let header_rlp = make_test_header_rlp(anchor, 1001, tx_root, receipt_root);
        let header = EthHeader::from_rlp(header_rlp);
        let header_hash = header.block_hash;

        let tx_proof = TxInclusionProof::new(vec![MptProofNode::new(tx_leaf)]);
        let receipt_proof = ReceiptProof::new(0, vec![MptProofNode::new(receipt_leaf)]);

        let op = ExternalBridgeOp::LightClientDeposit {
            source_chain: ExternalChain::EthereumMainnet,
            header,
            tx_proof,
            receipt_proof,
            recipient,
            asset_id,
            amount,
        };

        let config = BridgeConfig {
            allowed_assets: vec![1],
            ..Default::default()
        };
        let mut backend = MockStorage::default();

        // Finalize block 1001 — now it should pass consensus verification
        light_client.set_finalized_block(1001, header_hash);

        let result = process_light_client_deposit_evm(
            &mut light_client,
            &op,
            &mut backend,
            &config,
            1,
        );

        assert!(
            result.is_ok(),
            "expected Ok after consensus verification, got {:?}",
            result
        );

        let queued = result.unwrap();
        match queued {
            ExternalDepositResult::Queued { source_tx_hash: stx, .. } => {
                assert_eq!(stx, source_tx_hash);
            }
        }
    }

    fn make_light_client_deposit_op(
        anchor: B256,
        block_number: u64,
        recipient: Address,
        source_tx_hash: B256,
        asset_id: u64,
        amount: u128,
    ) -> (ExternalBridgeOp, call_light_client::EthLightClient, EthHeader) {
        let genesis = GenesisState {
            anchor_hash: anchor,
            anchor_block: block_number - 1,
            state_root: B256::ZERO,
        };
        let light_client = call_light_client::EthLightClient::init(genesis);

        let receipt_rlp = make_bridge_receipt_rlp(recipient, source_tx_hash, asset_id, amount);
        let receipt_leaf = make_leaf_node_rlp(&[], &receipt_rlp);
        let receipt_root = keccak256(&receipt_leaf);

        let tx_hash = B256::repeat_byte(0xDE);
        let tx_leaf = make_leaf_node_rlp(&[], &tx_hash.0);
        let tx_root = keccak256(&tx_leaf);

        let header_rlp = make_test_header_rlp(anchor, block_number, tx_root, receipt_root);
        let header = EthHeader::from_rlp(header_rlp);

        let tx_proof = TxInclusionProof::new(vec![MptProofNode::new(tx_leaf)]);
        let receipt_proof = ReceiptProof::new(0, vec![MptProofNode::new(receipt_leaf)]);

        let op = ExternalBridgeOp::LightClientDeposit {
            source_chain: ExternalChain::EthereumMainnet,
            header: header.clone(),
            tx_proof,
            receipt_proof,
            recipient,
            asset_id,
            amount,
        };

        (op, light_client, header)
    }

    #[test]
    fn test_light_client_deposit_rejects_not_allowed_asset() {
        let anchor = B256::repeat_byte(0xAA);
        let recipient = Address::repeat_byte(0x42);
        let source_tx_hash = B256::repeat_byte(0xAB);

        let (op, mut light_client, header) = make_light_client_deposit_op(
            anchor, 1001, recipient, source_tx_hash, 99, 1000,
        );

        light_client.set_finalized_block(1001, header.block_hash);

        let config = BridgeConfig {
            allowed_assets: vec![1],
            ..Default::default()
        };
        let mut backend = MockStorage::default();

        let result = process_light_client_deposit_evm(
            &mut light_client, &op, &mut backend, &config, 1,
        );

        assert!(
            matches!(result, Err(BridgeError::ExternalAssetNotAllowed(99))),
            "expected ExternalAssetNotAllowed(99), got {:?}",
            result
        );
    }

    #[test]
    fn test_light_client_deposit_rejects_duplicate_header() {
        let anchor = B256::repeat_byte(0xAA);
        let recipient = Address::repeat_byte(0x42);
        let source_tx_hash = B256::repeat_byte(0xAB);
        let asset_id = 1u64;
        let amount = 1000u128;

        let (op, mut light_client, header) = make_light_client_deposit_op(
            anchor, 1001, recipient, source_tx_hash, asset_id, amount,
        );

        light_client.set_finalized_block(1001, header.block_hash);

        let config = BridgeConfig {
            allowed_assets: vec![1],
            ..Default::default()
        };
        let mut backend = MockStorage::default();

        let result = process_light_client_deposit_evm(
            &mut light_client, &op, &mut backend, &config, 1,
        );
        assert!(result.is_ok(), "first deposit should succeed");

        // Second deposit with the SAME header triggers duplicate rejection
        let result = process_light_client_deposit_evm(
            &mut light_client, &op, &mut backend, &config, 2,
        );
        assert!(
            matches!(
                result,
                Err(BridgeError::MptProofError(ref msg)) if msg.contains("duplicate header")
            ),
            "expected duplicate header error, got {:?}",
            result
        );
    }

    #[test]
    fn test_light_client_deposit_rejects_exceeds_daily_limit() {
        let anchor = B256::repeat_byte(0xAA);
        let recipient = Address::repeat_byte(0x42);
        let source_tx_hash = B256::repeat_byte(0xAB);
        let asset_id = 1u64;
        let amount = 5000u128;

        let (op, mut light_client, header) = make_light_client_deposit_op(
            anchor, 1001, recipient, source_tx_hash, asset_id, amount,
        );

        light_client.set_finalized_block(1001, header.block_hash);

        let config = BridgeConfig {
            allowed_assets: vec![1],
            daily_limit_per_asset: 1000,
            blocks_per_day: 10,
            ..Default::default()
        };
        let mut backend = MockStorage::default();

        let result = process_light_client_deposit_evm(
            &mut light_client, &op, &mut backend, &config, 1,
        );
        assert!(
            matches!(result, Err(BridgeError::ExceedsDailyLimit(1, 0, 1000))),
            "expected ExceedsDailyLimit, got {:?}",
            result
        );
    }

    #[test]
    fn test_light_client_deposit_evm_storage_state() {
        let anchor = B256::repeat_byte(0xAA);
        let recipient = Address::repeat_byte(0x42);
        let source_tx_hash = B256::repeat_byte(0xAB);
        let asset_id = 1u64;
        let amount = 1000u128;

        let (op, mut light_client, header) = make_light_client_deposit_op(
            anchor, 1001, recipient, source_tx_hash, asset_id, amount,
        );

        light_client.set_finalized_block(1001, header.block_hash);

        let config = BridgeConfig {
            allowed_assets: vec![1],
            challenge_period_blocks: 50,
            ..Default::default()
        };
        let mut backend = MockStorage::default();
        let current_block = 10u64;

        let result = process_light_client_deposit_evm(
            &mut light_client, &op, &mut backend, &config, current_block,
        );
        assert!(result.is_ok(), "deposit should succeed");

        assert!(
            read_bridge_processed(&mut backend, *source_tx_hash),
            "source tx should be marked as processed"
        );
        assert_eq!(read_bridge_pending_count(&mut backend), 1);

        let status = backend.load(
            BRIDGE_ADDRESS,
            slot_bridge_pending_status(*source_tx_hash),
        );
        assert_eq!(status, U256::from(1u8));

        let stored_recipient = call_precompile::u256_to_address(
            backend.load(BRIDGE_ADDRESS, slot_bridge_pending_recipient(*source_tx_hash))
        );
        assert_eq!(stored_recipient, recipient);

        let stored_asset = call_precompile::u256_to_u64(
            backend.load(BRIDGE_ADDRESS, slot_bridge_pending_asset(*source_tx_hash))
        );
        assert_eq!(stored_asset, asset_id);

        let stored_amount = call_precompile::u256_to_u128(
            backend.load(BRIDGE_ADDRESS, slot_bridge_pending_amount(*source_tx_hash))
        );
        assert_eq!(stored_amount, amount);

        let stored_block = call_precompile::u256_to_u64(
            backend.load(BRIDGE_ADDRESS, slot_bridge_pending_block(*source_tx_hash))
        );
        assert_eq!(stored_block, current_block);
    }
}
