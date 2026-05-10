//! Prover key registry with versioned key rotation and sunset support.
//!
//! Replaces the static `OnceLock<RealProver>` singleton with a runtime-updatable
//! `RwLock`-based registry. This enables governance-driven key rotation without
//! restarting the prover service.
//!
//! # Lifecycle
//!
//! 1. **Initial load**: At boot, `ProverRegistry::global()` is seeded with keys
//!    from `/var/lib/callchain/shielded_keys` (version 0).
//! 2. **Rotation**: A governance proposal triggers `register()` with new ceremony
//!    output. The new version becomes `current`.
//! 3. **Sunset**: Old versions are retained for a configurable grace period so
//!    in-flight proofs (submitted before rotation but not yet mined) remain valid.
//! 4. **Cleanup**: After the sunset period, `sunset_older_than()` removes expired
//!    versions to free memory.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

#[cfg(feature = "production-keys")]
use crate::ceremony::{KeyLoadError, ProductionKeys};

/// Monotonically increasing key version identifier.
///
/// Version 0 is the genesis key set loaded at boot. Each rotation increments
/// by one. Proofs include the version they were generated with so validators
/// can look up the correct verifying key.
pub type KeyVersion = u32;

/// A key set tagged with versioning metadata.
#[derive(Clone, Debug)]
#[cfg(feature = "production-keys")]
pub struct VersionedKeys {
    pub version: KeyVersion,
    pub keys: ProductionKeys,
    pub registered_at: Instant,
}

/// Global registry for production proving/verifying keys.
///
/// All versions are retained until explicitly sunset. The `current` version
/// is the one used for generating new proofs. Older versions are used for
/// verifying in-flight proofs.
///
/// # Thread safety
///
/// `register()` takes a write lock; `current_version()` / `get()` / `current()`
/// take read locks. In practice registration is rare (governance-driven) and
/// reads are frequent (every shielded tx verification), so read contention is
/// minimal.
#[cfg(feature = "production-keys")]
pub struct ProverRegistry {
    versions: RwLock<HashMap<KeyVersion, VersionedKeys>>,
    current: RwLock<KeyVersion>,
}

#[cfg(feature = "production-keys")]
impl ProverRegistry {
    /// Global singleton registry.
    ///
    /// On first call, attempts to load production keys from
    /// `/var/lib/callchain/shielded_keys` as version 0. If loading fails,
    /// the registry starts empty and `current()` will return `None`.
    pub fn global() -> &'static Self {
        static INSTANCE: OnceLock<ProverRegistry> = OnceLock::new();
        INSTANCE.get_or_init(|| {
            let mut versions = HashMap::new();
            let current_version: KeyVersion = 0;

            match ProductionKeys::load("/var/lib/callchain/shielded_keys") {
                Ok(keys) => {
                    versions.insert(
                        current_version,
                        VersionedKeys {
                            version: current_version,
                            keys,
                            registered_at: Instant::now(),
                        },
                    );
                }
                Err(e) => {
                    eprintln!(
                        "WARNING: production ZK keys not found at /var/lib/callchain/shielded_keys: {e}. \
                         Registry started empty. Proofs cannot be verified until keys are registered."
                    );
                }
            }

            Self {
                versions: RwLock::new(versions),
                current: RwLock::new(current_version),
            }
        })
    }

    /// Create an empty registry (for testing or custom boot sequences).
    pub fn empty() -> Self {
        Self {
            versions: RwLock::new(HashMap::new()),
            current: RwLock::new(0),
        }
    }

    /// Register a new key set and advance the current version.
    ///
    /// `version` must be greater than the current version or the call is rejected.
    /// This prevents accidental downgrade attacks.
    pub fn register(&self, version: KeyVersion, keys: ProductionKeys) -> Result<(), RegistryError> {
        let current = self.current.read().map_err(|_| RegistryError::LockPoisoned)?;
        let versions = self.versions.read().map_err(|_| RegistryError::LockPoisoned)?;
        let is_empty = versions.is_empty();
        drop(versions);
        // Allow version 0 as the initial seed when registry is empty.
        // Otherwise require strictly monotonic increase.
        if !is_empty && version <= *current {
            return Err(RegistryError::VersionNotMonotonic {
                requested: version,
                current: *current,
            });
        }
        drop(current);

        let vk = VersionedKeys {
            version,
            keys,
            registered_at: Instant::now(),
        };

        let mut versions = self
            .versions
            .write()
            .map_err(|_| RegistryError::LockPoisoned)?;
        versions.insert(version, vk);

        let mut current = self
            .current
            .write()
            .map_err(|_| RegistryError::LockPoisoned)?;
        *current = version;

        Ok(())
    }

    /// Get the current key version.
    pub fn current_version(&self) -> KeyVersion {
        *self.current.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Get the key set for a specific version.
    pub fn get(&self, version: KeyVersion) -> Option<ProductionKeys> {
        let versions = self.versions.read().ok()?;
        versions.get(&version).map(|v| v.keys.clone())
    }

    /// Get the current key set.
    pub fn current(&self) -> Option<ProductionKeys> {
        let version = self.current_version();
        self.get(version)
    }

    /// Remove all versions older than `max_age` from now.
    ///
    /// The current version is never removed, even if it exceeds the age.
    /// Returns the number of versions removed.
    pub fn sunset_older_than(&self, max_age: Duration) -> Result<usize, RegistryError> {
        let current = self.current_version();
        let now = Instant::now();

        let mut versions = self
            .versions
            .write()
            .map_err(|_| RegistryError::LockPoisoned)?;

        let to_remove: Vec<KeyVersion> = versions
            .iter()
            .filter(|(v, vk)| **v != current && now.duration_since(vk.registered_at) > max_age)
            .map(|(v, _)| *v)
            .collect();

        let count = to_remove.len();
        for v in to_remove {
            versions.remove(&v);
        }

        Ok(count)
    }

    /// List all retained versions.
    pub fn versions(&self) -> Vec<KeyVersion> {
        let versions = match self.versions.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        versions.keys().copied().collect()
    }
}

/// Errors that can occur during registry operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("lock poisoned")]
    LockPoisoned,
    #[error("version {requested} is not greater than current {current}")]
    VersionNotMonotonic { requested: KeyVersion, current: KeyVersion },
    #[error("key load error: {0}")]
    KeyLoad(#[from] KeyLoadError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_global_has_version_zero_or_empty() {
        let registry = ProverRegistry::global();
        let versions = registry.versions();
        // Either version 0 loaded successfully, or registry is empty
        assert!(versions.is_empty() || versions.contains(&0));
    }

    #[test]
    fn test_registry_empty_starts_at_zero() {
        let registry = ProverRegistry::empty();
        assert_eq!(registry.current_version(), 0);
        assert!(registry.current().is_none());
    }

    #[test]
    #[cfg(feature = "production-keys")]
    fn test_registry_register_advances_version() {
        let registry = ProverRegistry::empty();

        // Seed with version 0 via dev setup (no production dir needed)
        let keys = crate::prover::RealProver::setup();
        let prod_keys = crate::ceremony::ProductionKeys {
            vk: crate::ceremony::ProductionVerifyingKeys {
                transfer: keys.transfer_keys().1.clone(),
                deposit: keys.deposit_keys().1.clone(),
                withdraw: keys.withdraw_keys().1.clone(),
            },
            pk: Some(crate::ceremony::ProductionProvingKeys {
                transfer: keys.transfer_keys().0.clone(),
                deposit: keys.deposit_keys().0.clone(),
                withdraw: keys.withdraw_keys().0.clone(),
            }),
        };

        registry.register(0, prod_keys.clone()).unwrap();
        assert_eq!(registry.current_version(), 0);

        // Register version 1
        registry.register(1, prod_keys).unwrap();
        assert_eq!(registry.current_version(), 1);

        // Reject non-monotonic version
        let keys = crate::prover::RealProver::setup();
        let bad_keys = crate::ceremony::ProductionKeys {
            vk: crate::ceremony::ProductionVerifyingKeys {
                transfer: keys.transfer_keys().1.clone(),
                deposit: keys.deposit_keys().1.clone(),
                withdraw: keys.withdraw_keys().1.clone(),
            },
            pk: None,
        };
        let result = registry.register(1, bad_keys);
        assert!(matches!(result, Err(RegistryError::VersionNotMonotonic { .. })));
    }

    #[test]
    #[cfg(feature = "production-keys")]
    fn test_registry_sunset_keeps_current() {
        let registry = ProverRegistry::empty();
        let keys = crate::prover::RealProver::setup();
        let prod_keys = crate::ceremony::ProductionKeys {
            vk: crate::ceremony::ProductionVerifyingKeys {
                transfer: keys.transfer_keys().1.clone(),
                deposit: keys.deposit_keys().1.clone(),
                withdraw: keys.withdraw_keys().1.clone(),
            },
            pk: None,
        };

        registry.register(0, prod_keys.clone()).unwrap();
        registry.register(1, prod_keys).unwrap();

        // Sunset everything older than 0s
        std::thread::sleep(Duration::from_millis(10));
        let removed = registry.sunset_older_than(Duration::from_secs(0)).unwrap();
        assert_eq!(removed, 1);

        // Current (version 1) should still exist
        assert!(registry.get(1).is_some());
        // Version 0 should be gone
        assert!(registry.get(0).is_none());
    }
}
