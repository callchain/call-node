//! T6.1 — Commonware Simplex BFT trait implementations.
//!
//! Bridges the commonware-consensus engine to the Callchain tokio world via
//! mpsc/oneshot channels. The engine runs in a background OS thread; these
//! trait implementations forward requests to tokio and await replies.

use crate::block_cache::BlockCache;
use crate::digest::ConsensusDigest;
use commonware_consensus::{
    simplex::{
        types::{Activity, Context},
        Plan,
    },
    Automaton, CertifiableAutomaton, Relay, Reporter,
};
use commonware_cryptography::Digest;
use commonware_cryptography::ed25519;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

/// Information sent from the `Reporter` when a block is finalized.
#[derive(Debug, Clone)]
pub struct FinalizationInfo {
    /// Digest of the finalized block.
    pub digest: ConsensusDigest,
    /// Consensus round in which the block was finalized.
    pub round: u64,
    /// Consensus view in which the block was finalized.
    pub view: u64,
}

// ─────────────────────────────────────────────────────────────────────────────
// CallAutomaton
// ─────────────────────────────────────────────────────────────────────────────

/// Channel pair for a propose request.
/// The BFT engine sends `(context, reply_tx)` and tokio must build a block,
/// cache it, and send the digest back via `reply_tx`.
pub type ProposeRequest = (
    Context<ConsensusDigest, ed25519::PublicKey>,
    oneshot::Sender<ConsensusDigest>,
);

/// Channel pair for a verify request.
/// The BFT engine sends `(context, digest, reply_tx)` and tokio must look up
/// the block, validate it, and send `true/false` back.
pub type VerifyRequest = (
    Context<ConsensusDigest, ed25519::PublicKey>,
    ConsensusDigest,
    oneshot::Sender<bool>,
);

/// Automaton implementation that bridges `propose` / `verify` to tokio.
#[derive(Clone)]
pub struct CallAutomaton {
    propose_tx: mpsc::Sender<ProposeRequest>,
    verify_tx: mpsc::Sender<VerifyRequest>,
}

impl CallAutomaton {
    /// Create a new automaton with the given outbound channels.
    pub fn new(
        propose_tx: mpsc::Sender<ProposeRequest>,
        verify_tx: mpsc::Sender<VerifyRequest>,
    ) -> Self {
        Self {
            propose_tx,
            verify_tx,
        }
    }
}

impl Automaton for CallAutomaton {
    type Context = Context<ConsensusDigest, ed25519::PublicKey>;
    type Digest = ConsensusDigest;

    async fn genesis(&mut self, _epoch: commonware_consensus::types::Epoch) -> Self::Digest {
        ConsensusDigest::EMPTY
    }

    async fn propose(
        &mut self,
        context: Self::Context,
    ) -> oneshot::Receiver<Self::Digest> {
        let (tx, rx) = oneshot::channel();
        // If tokio has dropped, the channel close is harmless — consensus
        // will treat it as "unable to propose".
        let _ = self.propose_tx.send((context, tx)).await;
        rx
    }

    async fn verify(
        &mut self,
        context: Self::Context,
        payload: Self::Digest,
    ) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        let _ = self.verify_tx.send((context, payload, tx)).await;
        rx
    }
}

impl CertifiableAutomaton for CallAutomaton {}

// ─────────────────────────────────────────────────────────────────────────────
// CallRelay
// ─────────────────────────────────────────────────────────────────────────────

/// Relay implementation that forwards block broadcasts to tokio.
#[derive(Clone)]
pub struct CallRelay {
    block_cache: Arc<Mutex<BlockCache>>,
    broadcast_tx: mpsc::Sender<Vec<u8>>,
}

impl CallRelay {
    /// Create a new relay with the given block cache and broadcast channel.
    pub fn new(
        block_cache: Arc<Mutex<BlockCache>>,
        broadcast_tx: mpsc::Sender<Vec<u8>>,
    ) -> Self {
        Self {
            block_cache,
            broadcast_tx,
        }
    }
}

impl Relay for CallRelay {
    type Digest = ConsensusDigest;
    type PublicKey = ed25519::PublicKey;
    type Plan = Plan<ed25519::PublicKey>;

    async fn broadcast(&mut self, payload: Self::Digest, _plan: Self::Plan) {
        let block = {
            let cache = self.block_cache.lock().unwrap();
            cache.get(&payload).cloned()
        };
        if let Some(block) = block {
            match serde_json::to_vec(&block) {
                Ok(bytes) => {
                    tracing::info!(digest = %payload, bytes = bytes.len(), "BFT relay: sending block to broadcast channel");
                    let _ = self.broadcast_tx.send(bytes).await;
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "failed to serialize block for broadcast");
                }
            }
        } else {
            tracing::warn!(digest = %payload, "block not in cache for broadcast");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CallReporter
// ─────────────────────────────────────────────────────────────────────────────

/// Reporter implementation that forwards finalization events to tokio.
#[derive(Clone)]
pub struct CallReporter {
    finalize_tx: mpsc::Sender<FinalizationInfo>,
}

impl CallReporter {
    /// Create a new reporter with the given finalize channel.
    pub fn new(finalize_tx: mpsc::Sender<FinalizationInfo>) -> Self {
        Self { finalize_tx }
    }
}

impl Reporter for CallReporter {
    type Activity = Activity<commonware_consensus::simplex::scheme::ed25519::Scheme, ConsensusDigest>;

    async fn report(&mut self, activity: Self::Activity) {
        match activity {
            Activity::Finalization(ref fin) => {
                let info = FinalizationInfo {
                    digest: fin.proposal.payload,
                    round: fin.proposal.round.epoch().get(),
                    view: fin.proposal.round.view().get(),
                };
                let _ = self.finalize_tx.send(info).await;
            }
            _ => {
                // Other activities (votes, notarizations, faults) are ignored
                // for now. We can wire slashing/rewards later.
            }
        }
    }
}
