//! Agent precompile entry point (0x209).
//!
//! Thin wrapper that routes EVM calls to [`AgentStorage`] backed by
//! [`JournalBackend`]. Business logic lives in [`AgentStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use crate::AgentStorage;
use alloy_sol_types::{sol, SolCall};
use call_asset::AssetStorage;
use call_precompile::storage::StorageProvider;
use call_precompile::{dispatch, journal_backend::JournalBackend, require_caller};
use call_primitives::Address;
use revm_precompile::{PrecompileError, PrecompileResult};

sol! {
    interface IProtocolAgent {
        function registerAgent(string name, string url, bytes32 pubkeyHash) external;
        function grantBalance(uint64 agentId, uint64 assetId, uint128 amount) external;
        function revokeBalance(uint64 agentId, uint64 assetId) external;
        function pay(uint64 agentId, uint64 assetId, address to, uint128 amount) external;
        function batchPay(uint64 agentId, uint64 assetId, address[] to, uint128[] amounts) external;
        function withdrawBalance(uint64 agentId, uint64 assetId, uint128 amount) external;
        function revokeAgent(uint64 agentId) external;
        function getAgentOwner(uint64 agentId) external view returns (address);
        function getAgentBalance(uint64 agentId, uint64 assetId) external view returns (uint128);
        function getAgentName(uint64 agentId) external view returns (bytes32);
        function getAgentUrl(uint64 agentId) external view returns (bytes32);
        function getAgentPerms(uint64 agentId) external view returns (uint256);
    }
}

/// Stateful agent precompile backed by EVM storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentPrecompile;

impl AgentPrecompile {
    fn register_agent(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::registerAgentCall, _>(
            calldata,
            6000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AgentStorage::new(JournalBackend::new(storage));
                let block_number = storage.block_number();
                store
                    .register_agent(
                        &call.name,
                        &call.url,
                        call.pubkeyHash.into(),
                        caller,
                        block_number,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn grant_balance(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::grantBalanceCall, _>(
            calldata,
            6000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut agent_store = AgentStorage::new(JournalBackend::new(storage));
                let mut asset_store = AssetStorage::new(JournalBackend::new(storage));
                agent_store
                    .grant_balance(
                        &mut asset_store,
                        call.agentId,
                        call.assetId,
                        call.amount,
                        caller,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn revoke_balance(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::revokeBalanceCall, _>(
            calldata,
            6000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AgentStorage::new(JournalBackend::new(storage));
                store
                    .revoke_balance(call.agentId, call.assetId, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn pay(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::payCall, _>(
            calldata,
            30000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut agent_store = AgentStorage::new(JournalBackend::new(storage));
                let mut asset_store = AssetStorage::new(JournalBackend::new(storage));
                let block_number = storage.block_number();
                agent_store
                    .pay(
                        &mut asset_store,
                        call.agentId,
                        call.assetId,
                        call.to,
                        call.amount,
                        caller,
                        block_number,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn batch_pay(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::batchPayCall, _>(
            calldata,
            30000, // base gas; per-recipient gas not scaled in dispatch
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut agent_store = AgentStorage::new(JournalBackend::new(storage));
                let mut asset_store = AssetStorage::new(JournalBackend::new(storage));
                let block_number = storage.block_number();
                agent_store
                    .batch_pay(
                        &mut asset_store,
                        call.agentId,
                        call.assetId,
                        &call.to,
                        &call.amounts,
                        caller,
                        block_number,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn withdraw_balance(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::withdrawBalanceCall, _>(
            calldata,
            50000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AgentStorage::new(JournalBackend::new(storage));
                let block_number = storage.block_number();
                store
                    .withdraw_balance(
                        call.agentId,
                        call.assetId,
                        call.amount,
                        caller,
                        block_number,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn revoke_agent(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::revokeAgentCall, _>(
            calldata,
            20000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AgentStorage::new(JournalBackend::new(storage));
                store
                    .revoke_agent(call.agentId, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn get_agent_owner(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentOwnerCall, _, _>(
            calldata,
            2000,
            storage,
            |call, storage| {
                let store = AgentStorage::new(JournalBackend::new(storage));
                Ok(store.read_owner(call.agentId))
            },
        )
    }

    fn get_agent_balance(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentBalanceCall, _, _>(
            calldata,
            2000,
            storage,
            |call, storage| {
                let store = AgentStorage::new(JournalBackend::new(storage));
                Ok(store.read_agent_balance(call.agentId, call.assetId))
            },
        )
    }

    fn get_agent_name(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentNameCall, _, _>(
            calldata,
            2000,
            storage,
            |call, storage| {
                let store = AgentStorage::new(JournalBackend::new(storage));
                Ok(store.read_name(call.agentId))
            },
        )
    }

    fn get_agent_url(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentUrlCall, _, _>(
            calldata,
            2000,
            storage,
            |call, storage| {
                let store = AgentStorage::new(JournalBackend::new(storage));
                Ok(store.read_url(call.agentId))
            },
        )
    }

    fn get_agent_perms(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentPermsCall, _, _>(
            calldata,
            2000,
            storage,
            |call, storage| {
                let store = AgentStorage::new(JournalBackend::new(storage));
                Ok(store.read_perms(call.agentId))
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for AgentPrecompile {
    #[allow(clippy::expect_used)]
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("too short".into()));
        }
        let selector: [u8; 4] = calldata[..4]
            .try_into()
            .expect("slice length checked above");
        match selector {
            IProtocolAgent::registerAgentCall::SELECTOR => {
                self.register_agent(calldata, msg_sender, storage)
            }
            IProtocolAgent::grantBalanceCall::SELECTOR => {
                self.grant_balance(calldata, msg_sender, storage)
            }
            IProtocolAgent::revokeBalanceCall::SELECTOR => {
                self.revoke_balance(calldata, msg_sender, storage)
            }
            IProtocolAgent::payCall::SELECTOR => self.pay(calldata, msg_sender, storage),
            IProtocolAgent::batchPayCall::SELECTOR => self.batch_pay(calldata, msg_sender, storage),
            IProtocolAgent::withdrawBalanceCall::SELECTOR => {
                self.withdraw_balance(calldata, msg_sender, storage)
            }
            IProtocolAgent::revokeAgentCall::SELECTOR => {
                self.revoke_agent(calldata, msg_sender, storage)
            }
            IProtocolAgent::getAgentOwnerCall::SELECTOR => self.get_agent_owner(calldata, storage),
            IProtocolAgent::getAgentBalanceCall::SELECTOR => {
                self.get_agent_balance(calldata, storage)
            }
            IProtocolAgent::getAgentNameCall::SELECTOR => self.get_agent_name(calldata, storage),
            IProtocolAgent::getAgentUrlCall::SELECTOR => self.get_agent_url(calldata, storage),
            IProtocolAgent::getAgentPermsCall::SELECTOR => self.get_agent_perms(calldata, storage),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{slot_balance, u128_to_u256, StatefulPrecompile, ASSET_ADDRESS};
    use call_primitives::Address;

    #[test]
    fn test_agent_address() {
        assert_eq!(
            call_precompile::AGENT_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000209")
        );
    }

    #[test]
    fn test_agent_precompile_register_and_get() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x22);

        let mut precompile = AgentPrecompile;

        // registerAgent(name, url, pubkeyHash)
        let input = IProtocolAgent::registerAgentCall {
            name: "TestAgent".into(),
            url: "http://test.com".into(),
            pubkeyHash: [0xBBu8; 32].into(),
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "register failed: {:?}", result.err());

        // getAgentOwner(0)
        let input = IProtocolAgent::getAgentOwnerCall { agentId: 0 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let owner = Address::from_slice(&result.bytes[12..32]);
        assert_eq!(owner, sender);

        // getAgentName(0)
        let input = IProtocolAgent::getAgentNameCall { agentId: 0 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(&result.bytes[0..9], b"TestAgent");

        // getAgentUrl(0)
        let input = IProtocolAgent::getAgentUrlCall { agentId: 0 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(&result.bytes[0..15], b"http://test.com");
    }

    #[test]
    fn test_agent_precompile_grant_pay_and_revoke() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x22);
        let recipient = Address::repeat_byte(0x33);

        // Seed sender balance
        let sender_slot = slot_balance(crate::CALL_ASSET_ID, sender);
        provider
            .sstore(ASSET_ADDRESS, sender_slot, u128_to_u256(10_000))
            .unwrap();

        let mut precompile = AgentPrecompile;

        // registerAgent
        let input = IProtocolAgent::registerAgentCall {
            name: "Agent".into(),
            url: "url".into(),
            pubkeyHash: [0xCCu8; 32].into(),
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        // grantBalance(agentId=0, assetId=1, amount=5_000)
        let input = IProtocolAgent::grantBalanceCall {
            agentId: 0,
            assetId: crate::CALL_ASSET_ID,
            amount: 5_000,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "grant failed: {:?}", result.err());

        // getAgentBalance(0, 1)
        let input = IProtocolAgent::getAgentBalanceCall {
            agentId: 0,
            assetId: crate::CALL_ASSET_ID,
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let bal = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&result.bytes[16..32]);
            buf
        });
        assert_eq!(bal, 5_000);

        // pay(agentId=0, assetId=1, to=recipient, amount=1_000)
        let input = IProtocolAgent::payCall {
            agentId: 0,
            assetId: crate::CALL_ASSET_ID,
            to: recipient,
            amount: 1_000,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "pay failed: {:?}", result.err());

        // getAgentBalance(0, 1) should be 4_000
        let input = IProtocolAgent::getAgentBalanceCall {
            agentId: 0,
            assetId: crate::CALL_ASSET_ID,
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let bal = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&result.bytes[16..32]);
            buf
        });
        assert_eq!(bal, 4_000);

        // revokeBalance(0, 1)
        let input = IProtocolAgent::revokeBalanceCall {
            agentId: 0,
            assetId: crate::CALL_ASSET_ID,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "revoke failed: {:?}", result.err());

        // getAgentBalance(0, 1) should be 0
        let input = IProtocolAgent::getAgentBalanceCall {
            agentId: 0,
            assetId: crate::CALL_ASSET_ID,
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let bal = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&result.bytes[16..32]);
            buf
        });
        assert_eq!(bal, 0);
    }
}
