//! External bridge types and shared utilities.

use alloy_primitives::{Address, B256};
use call_primitives::{AssetId, Signature};
use call_crypto::{keccak256, recover_secp256k1_signer};
use crate::{BridgeConfig, BridgeError};

#[cfg(feature = "light-client-bridge")]
use call_light_client::{EthHeader, TxInclusionProof, ReceiptProof};

/// Supported external chains (per spec §5.6)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalChain {
    EthereumMainnet,
    Arbitrum,
}

impl ExternalChain {
    /// Chain ID for the external chain
    pub fn chain_id(&self) -> u64 {
        match self {
            ExternalChain::EthereumMainnet => 1,
            ExternalChain::Arbitrum => 42161,
        }
    }
}

/// External bridge operation (per spec §5.6)
#[derive(Debug, Clone)]
pub enum ExternalBridgeOp {
    /// Deposit from external chain → Callchain
    Deposit {
        source_chain: ExternalChain,
        source_tx_hash: B256,
        source_block_number: u64,
        sender: Vec<u8>,
        recipient: Address,
        asset_id: AssetId,
        amount: u128,
        signatures: Vec<BridgeSignature>,
    },
    /// Withdraw from Callchain → external chain
    Withdraw {
        target_chain: ExternalChain,
        target_address: Vec<u8>,
        asset_id: AssetId,
        sender: Address,
        amount: u128,
    },
    /// Deposit from external chain via light client verification
    /// (no validator signatures needed — verified via MPT proofs)
    #[cfg(feature = "light-client-bridge")]
    LightClientDeposit {
        source_chain: ExternalChain,
        header: EthHeader,
        tx_proof: TxInclusionProof,
        receipt_proof: ReceiptProof,
        recipient: Address,
        asset_id: AssetId,
        amount: u128,
    },
}

/// Bridge deposit proof embedded in the legacy `BridgeDeposit` instruction.
/// The `proof: Vec<u8>` field is expected to be `serde_json::to_vec` of this struct.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BridgeDepositProof {
    pub source_tx_hash: [u8; 32],
    pub source_block_number: u64,
    pub external_sender: Vec<u8>,
    pub signatures: Vec<(u32, Vec<u8>)>,
}

/// Bridge contract registry: authorized Ethereum-side bridge contracts per chain.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BridgeContractRegistry {
    /// chain_id -> authorized contract addresses
    contracts: std::collections::HashMap<u64, Vec<Address>>,
}

impl BridgeContractRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Authorize a bridge contract address for a chain.
    pub fn authorize(&mut self, chain_id: u64, contract: Address) {
        self.contracts.entry(chain_id).or_default().push(contract);
    }

    /// Revoke a bridge contract address for a chain.
    pub fn revoke(&mut self, chain_id: u64, contract: Address) {
        if let Some(list) = self.contracts.get_mut(&chain_id) {
            list.retain(|c| c != &contract);
        }
    }

    /// Check if a contract is authorized for the given chain.
    pub fn is_authorized(&self, chain_id: u64, contract: &Address) -> bool {
        self.contracts
            .get(&chain_id)
            .map(|list| list.contains(contract))
            .unwrap_or(false)
    }

    /// Get authorized contracts for a chain.
    pub fn get_contracts(&self, chain_id: u64) -> Option<&Vec<Address>> {
        self.contracts.get(&chain_id)
    }
}

/// Verify that a bridge contract is authorized for the given chain.
/// If no contracts are registered for the chain, the check passes (permissive default).
pub fn verify_bridge_contract(
    config: &BridgeConfig,
    chain_id: u64,
    contract: &Address,
) -> Result<(), BridgeError> {
    let contracts = config.authorized_contracts.get(&chain_id);
    match contracts {
        Some(list) if !list.is_empty() => {
            if !list.contains(contract) {
                return Err(BridgeError::UnauthorizedBridgeContract(chain_id, *contract));
            }
        }
        _ => {
            // No authorized contracts configured for this chain — permissive default.
            // In production, operators should populate authorized_contracts.
        }
    }
    Ok(())
}

/// Validator bridge signature
#[derive(Debug, Clone)]
pub struct BridgeSignature {
    /// Validator index in the validator set
    pub validator_index: u32,
    /// secp256k1 signature (65 bytes: r || s || v)
    pub signature: Signature,
}

/// Hash of a bridge event for signing
pub fn bridge_event_hash(
    source_chain: &ExternalChain,
    source_tx_hash: B256,
    source_block_number: u64,
    sender: &[u8],
    recipient: Address,
    asset_id: AssetId,
    amount: u128,
) -> B256 {
    let mut buf = Vec::new();
    buf.extend_from_slice(&source_chain.chain_id().to_be_bytes());
    buf.extend_from_slice(&source_tx_hash.0);
    buf.extend_from_slice(&source_block_number.to_be_bytes());
    buf.extend_from_slice(sender);
    buf.extend_from_slice(recipient.as_slice());
    buf.extend_from_slice(&asset_id.to_be_bytes());
    buf.extend_from_slice(&amount.to_be_bytes());
    keccak256(&buf)
}

/// Verify bridge signatures from validators (per spec §5.6.1)
///
/// Requires at least `min_signatures` (default 14 = 2/3 of 21 subset)
/// valid secp256k1 signatures from distinct validators.
pub fn verify_bridge_signatures(
    op: &ExternalBridgeOp,
    validators: &[Address],
    min_signatures: u64,
) -> Result<(), BridgeError> {
    let ExternalBridgeOp::Deposit {
        source_chain,
        source_tx_hash,
        source_block_number,
        sender,
        recipient,
        asset_id,
        amount,
        signatures,
    } = op
    else {
        return Ok(()); // Withdraw doesn't need signature verification
    };

    // Check minimum count
    if (signatures.len() as u64) < min_signatures {
        return Err(BridgeError::InsufficientSignatures(
            signatures.len() as u64,
            min_signatures,
        ));
    }

    // Compute the event hash that was signed
    let event_hash = bridge_event_hash(
        source_chain,
        *source_tx_hash,
        *source_block_number,
        sender,
        *recipient,
        *asset_id,
        *amount,
    );

    // Verify each signature and track which validators signed
    let mut seen_validators = std::collections::HashSet::<u32>::new();

    for (i, bridge_sig) in signatures.iter().enumerate() {
        // Check for duplicate validators
        if !seen_validators.insert(bridge_sig.validator_index) {
            return Err(BridgeError::InvalidSignature(
                i as u64,
                "duplicate validator".into(),
            ));
        }

        // Recover signer from signature and event hash
        let recovered = recover_secp256k1_signer(&event_hash.0, &bridge_sig.signature)
            .map_err(|e| BridgeError::InvalidSignature(i as u64, format!("{e:?}")))?;

        // Verify the recovered address is in the validator set
        if !validators.contains(&recovered) {
            return Err(BridgeError::InvalidSignature(
                i as u64,
                "signer not in validator set".into(),
            ));
        }
    }

    // Final check: enough unique valid signatures
    if (seen_validators.len() as u64) < min_signatures {
        return Err(BridgeError::InsufficientSignatures(
            seen_validators.len() as u64,
            min_signatures,
        ));
    }

    Ok(())
}
