//! Consensus digest wrapper — bridges BlockHash to commonware-consensus traits.
//!
//! `BlockHash` is an alias for `alloy_primitives::B256` (a foreign type), so we
//! cannot implement commonware traits directly on it. This thin wrapper
//! implements `commonware_cryptography::Digest` and all prerequisite traits.

use bytes::{Buf, BufMut};
use commonware_codec::{Error as CodecError, FixedSize, Read, ReadExt, Write};
use commonware_cryptography::Digest;
use commonware_math::algebra::Random;
use commonware_utils::{Array, Span};
use rand_core::CryptoRngCore;
use std::ops::Deref;

const DIGEST_LENGTH: usize = 32;

/// Wrapper around a 32-byte hash for use with `commonware-consensus`.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Default, Debug)]
#[repr(transparent)]
pub struct ConsensusDigest(pub [u8; DIGEST_LENGTH]);

impl ConsensusDigest {
    /// All-zero digest.
    pub const ZERO: Self = Self([0u8; DIGEST_LENGTH]);
}

impl std::fmt::Display for ConsensusDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}

impl From<call_primitives::BlockHash> for ConsensusDigest {
    fn from(value: call_primitives::BlockHash) -> Self {
        Self(value.0)
    }
}

impl From<ConsensusDigest> for call_primitives::BlockHash {
    fn from(value: ConsensusDigest) -> Self {
        Self(value.0)
    }
}

impl From<[u8; DIGEST_LENGTH]> for ConsensusDigest {
    fn from(value: [u8; DIGEST_LENGTH]) -> Self {
        Self(value)
    }
}

impl AsRef<[u8]> for ConsensusDigest {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Deref for ConsensusDigest {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl Write for ConsensusDigest {
    fn write(&self, buf: &mut impl BufMut) {
        self.0.write(buf);
    }
}

impl Read for ConsensusDigest {
    type Cfg = ();

    fn read_cfg(buf: &mut impl Buf, _: &()) -> Result<Self, CodecError> {
        let array = <[u8; DIGEST_LENGTH]>::read(buf)?;
        Ok(Self(array))
    }
}

impl FixedSize for ConsensusDigest {
    const SIZE: usize = DIGEST_LENGTH;
}

impl Span for ConsensusDigest {}

impl Array for ConsensusDigest {}

impl Digest for ConsensusDigest {
    const EMPTY: Self = Self([0u8; DIGEST_LENGTH]);
}

impl Random for ConsensusDigest {
    fn random(mut rng: impl CryptoRngCore) -> Self {
        let mut array = [0u8; DIGEST_LENGTH];
        rng.fill_bytes(&mut array);
        Self(array)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consensus_digest_size() {
        assert_eq!(ConsensusDigest::SIZE, 32);
    }

    #[test]
    fn test_consensus_digest_empty() {
        assert_eq!(ConsensusDigest::EMPTY.0, [0u8; 32]);
    }

    #[test]
    fn test_consensus_digest_roundtrip_blockhash() {
        let hash = call_primitives::BlockHash::repeat_byte(0xAB);
        let digest: ConsensusDigest = hash.into();
        let back: call_primitives::BlockHash = digest.into();
        assert_eq!(hash, back);
    }
}
