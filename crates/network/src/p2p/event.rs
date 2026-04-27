//! Network Events emitted by the P2P layer.

use crate::p2p::message::{
    BlockAnnouncement, OraclePriceRequest, OraclePriceSubmission, SyncRequest, UpgradeAnnouncement,
};
use call_primitives::TxHash;

/// Events emitted by the P2P network layer
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// New peer connected
    PeerConnected { peer_id: String },
    /// Peer disconnected
    PeerDisconnected { peer_id: String },
    /// Transaction received
    TransactionReceived {
        peer_id: String,
        hash: TxHash,
        data: Vec<u8>,
    },
    /// Block announcement received
    BlockAnnouncementReceived {
        peer_id: String,
        announcement: BlockAnnouncement,
    },
    /// Sync request received
    SyncRequestReceived {
        peer_id: String,
        request: SyncRequest,
    },
    /// Oracle price request received from proposer
    OraclePriceRequestReceived {
        peer_id: String,
        request: OraclePriceRequest,
    },
    /// Oracle price submission received
    OraclePriceSubmissionReceived {
        peer_id: String,
        submission: OraclePriceSubmission,
    },
    /// Protocol upgrade announcement received
    UpgradeAnnouncementReceived {
        peer_id: String,
        announcement: UpgradeAnnouncement,
    },
}
