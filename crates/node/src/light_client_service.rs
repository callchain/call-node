//! Independent tokio task for light client header gossip.

use crate::light_client::{BlockSignatures, HeaderAnnouncement, LightClient};
use crate::network_handler::LIGHT_CLIENT_CHANNEL;
use call_consensus::BlockHeader;
use call_governance::precompile::GOVERNANCE_ADDRESS;
use call_network::Network;
use call_precompile::storage::storage_slot;
use call_primitives::U256;
use std::sync::Arc;
use tokio::sync::mpsc;

pub enum LightClientEvent {
    LocalBlock {
        header: BlockHeader,
        signatures: BlockSignatures,
    },
    PeerAnnouncement {
        header: BlockHeader,
        signatures: BlockSignatures,
    },
}

pub struct LightClientService {
    pub event_rx: mpsc::UnboundedReceiver<LightClientEvent>,
    pub network: Arc<dyn Network>,
    pub light_client: LightClient,
    pub db_env: Arc<reth_db::DatabaseEnv>,
    pub epoch_length: u64,
}

impl LightClientService {
    pub async fn run(mut self) {
        let mut last_applied_key_version = call_shielded::current_prover_key_version();

        while let Some(event) = self.event_rx.recv().await {
            match event {
                LightClientEvent::LocalBlock { header, signatures } => {
                    if let Err(e) = self.light_client.sync_incremental(&header, &signatures) {
                        tracing::warn!(
                            error = %e,
                            height = header.height,
                            "light_client: local block verification failed"
                        );
                        continue;
                    }
                    if self.epoch_length > 0 && header.height % self.epoch_length == 0 {
                        if let Err(e) = self.light_client.refresh_validator_set(&self.db_env) {
                            tracing::warn!(
                                error = %e,
                                "light_client: validator refresh failed"
                            );
                        }
                        self.check_prover_key_rotation(&mut last_applied_key_version);
                    }
                    let announcement = HeaderAnnouncement { header, signatures };
                    let msg = match postcard::to_allocvec(&announcement) {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!(error = %e, "light_client: serialize failed");
                            continue;
                        }
                    };
                    self.network.broadcast(LIGHT_CLIENT_CHANNEL, msg).await;
                }
                LightClientEvent::PeerAnnouncement { header, signatures } => {
                    if let Err(e) = self.light_client.sync_incremental(&header, &signatures) {
                        tracing::debug!(
                            error = %e,
                            height = header.height,
                            "light_client: peer header rejected"
                        );
                    } else {
                        tracing::debug!(
                            height = header.height,
                            "light_client: peer header verified"
                        );
                    }
                }
            }
        }
        tracing::info!("light_client: service shutting down");
    }

    /// Check the governance precompile for a pending prover key rotation
    /// and apply it if the version is newer than what we have.
    fn check_prover_key_rotation(&self, last_applied: &mut u32) {
        let provider = match call_evm::provider::InMemoryStateProvider::from_db(&self.db_env) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "prover_key_rotation: failed to load state");
                return;
            }
        };

        let pending_slot = storage_slot(&[b"prover_key_rotation", b"pending"]);
        let version_slot = storage_slot(&[b"prover_key_rotation", b"version"]);

        let pending = provider.get_storage(&GOVERNANCE_ADDRESS, pending_slot);
        if pending == U256::ZERO {
            return;
        }

        let version = provider
            .get_storage(&GOVERNANCE_ADDRESS, version_slot)
            .to::<u32>();
        if version <= *last_applied {
            return;
        }

        tracing::info!(
            version,
            last_applied = *last_applied,
            "prover_key_rotation: detected pending rotation"
        );

        if call_shielded::try_register_prover_keys(version) {
            *last_applied = version;
            tracing::info!(version, "prover_key_rotation: applied successfully");
        }
    }
}
