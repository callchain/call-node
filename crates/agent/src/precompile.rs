//! Agent precompile entry point (0x209).
//!
//! Thin wrapper that routes EVM calls to [`AgentStorage`] backed by
//! [`StorageProvider`]. Business logic lives in [`AgentStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use crate::{slot_agent_balance, AgentError, AgentStorage};
use alloy_sol_types::{sol, SolCall};
use call_asset::AssetStorage;
use call_precompile::storage::StorageProvider;
use call_precompile::{dispatch, require_caller, u128_to_u256, StorageRef, AGENT_ADDRESS};
use call_primitives::Address;
use revm_precompile::{PrecompileError, PrecompileResult};

sol! {
    interface IProtocolAgent {
        function registerAgent(string name, string url, address agentAddress) external;
        function grantBalance(uint64 agentId, uint64 assetId, uint128 amount) external;
        function revokeBalance(uint64 agentId, uint64 assetId) external;
        function pay(uint64 assetId, address to, uint128 amount) external;
        function batchPay(uint64 assetId, address[] to, uint128[] amounts) external;
        function revokeAgent(uint64 agentId) external;
        function getAgentOwner(uint64 agentId) external view returns (address);
        function getAgentAddress(uint64 agentId) external view returns (address);
        function getAgentBalance(uint64 agentId, uint64 assetId) external view returns (uint128);
        function getAgentName(uint64 agentId) external view returns (bytes32);
        function getAgentUrl(uint64 agentId) external view returns (bytes32);
        function getAgentPerms(uint64 agentId) external view returns (uint256);
        function createSession(address delegate, uint128 perTxLimit, uint128 dailyLimit, uint64 expiresAt) external returns (uint64);
        function revokeSession(uint64 sessionId) external;
        function isSessionValid(uint64 sessionId) external view returns (uint64);
        function executeSessionTransfer(uint64 sessionId, uint64 assetId, address to, uint128 amount) external;
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
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::registerAgentCall, _>(
            calldata,
            6000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AgentStorage::new(sr);
                let block_number = storage.block_number();
                store
                    .register_agent(
                        &call.name,
                        &call.url,
                        call.agentAddress,
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
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::grantBalanceCall, _>(
            calldata,
            6000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;

                // Step 1: validate agent exists and caller is owner
                {
                    let mut agent_store = AgentStorage::new(sr);
                    if !agent_store.agent_exists(call.agentId) {
                        return Err(PrecompileError::Other(
                            AgentError::NotFound.to_string().into(),
                        ));
                    }
                    agent_store
                        .check_owner(call.agentId, caller)
                        .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                }

                // Step 2: deduct balance from caller
                {
                    let mut asset_store = AssetStorage::new(sr);
                    asset_store
                        .deduct_balance(call.assetId, caller, call.amount)
                        .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                }

                // Step 3: add balance to agent
                {
                    let mut agent_store = AgentStorage::new(sr);
                    let agent_bal = agent_store
                        .read_agent_balance(call.agentId, call.assetId)
                        .checked_add(call.amount)
                        .ok_or_else(|| {
                            PrecompileError::Other(AgentError::BalanceOverflow.to_string().into())
                        })?;
                    storage.sstore(
                        AGENT_ADDRESS,
                        slot_agent_balance(call.agentId, call.assetId),
                        u128_to_u256(agent_bal),
                    )?;
                }

                Ok(())
            },
        )
    }

    fn revoke_balance(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::revokeBalanceCall, _>(
            calldata,
            6000,
            storage,
            |call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AgentStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                store
                    .revoke_balance(&mut asset_store, call.agentId, call.assetId, caller)
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
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::payCall, _>(
            calldata,
            30000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let block_number = storage.block_number();
                let mut agent_store = AgentStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                agent_store
                    .pay(
                        &mut asset_store,
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
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::batchPayCall, _>(
            calldata,
            30000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let block_number = storage.block_number();
                let mut agent_store = AgentStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                agent_store
                    .batch_pay(
                        &mut asset_store,
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

    fn revoke_agent(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::revokeAgentCall, _>(
            calldata,
            20000,
            storage,
            |call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AgentStorage::new(sr);
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
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentOwnerCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = AgentStorage::new(sr);
                Ok(store.read_owner(call.agentId))
            },
        )
    }

    fn get_agent_balance(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentBalanceCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = AgentStorage::new(sr);
                Ok(store.read_agent_balance(call.agentId, call.assetId))
            },
        )
    }

    fn get_agent_name(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentNameCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = AgentStorage::new(sr);
                Ok(store.read_name(call.agentId))
            },
        )
    }

    fn get_agent_url(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentUrlCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = AgentStorage::new(sr);
                Ok(store.read_url(call.agentId))
            },
        )
    }

    fn get_agent_address(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentAddressCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = AgentStorage::new(sr);
                Ok(store.read_agent_address(call.agentId))
            },
        )
    }

    fn get_agent_perms(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::getAgentPermsCall, _, _>(
            calldata,
            2000,
            storage,
            |call, _storage| {
                let mut store = AgentStorage::new(sr);
                Ok(store.read_perms(call.agentId))
            },
        )
    }

    fn create_session(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate::<IProtocolAgent::createSessionCall, _, _>(
            calldata,
            10000,
            storage,
            |call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AgentStorage::new(sr);
                let session_id = store
                    .create_session(
                        call.delegate,
                        call.perTxLimit,
                        call.dailyLimit,
                        call.expiresAt,
                        caller,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(session_id)
            },
        )
    }

    fn revoke_session(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::revokeSessionCall, _>(
            calldata,
            6000,
            storage,
            |call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AgentStorage::new(sr);
                store
                    .revoke_session(call.sessionId, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn is_session_valid(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAgent::isSessionValidCall, _, _>(
            calldata,
            2000,
            storage,
            |call, storage| {
                let mut store = AgentStorage::new(sr);
                let block_number = storage.block_number();
                let valid = store.is_session_valid(call.sessionId, block_number);
                Ok(if valid { 1u64 } else { 0u64 })
            },
        )
    }

    fn execute_session_transfer(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAgent::executeSessionTransferCall, _>(
            calldata,
            30000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let block_number = storage.block_number();

                let mut agent_store = AgentStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                agent_store
                    .execute_session_transfer(
                        &mut asset_store,
                        call.sessionId,
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
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolAgent::registerAgentCall::SELECTOR => {
                self.register_agent(calldata, msg_sender, storage, sr)
            }
            IProtocolAgent::grantBalanceCall::SELECTOR => {
                self.grant_balance(calldata, msg_sender, storage, sr)
            }
            IProtocolAgent::revokeBalanceCall::SELECTOR => {
                self.revoke_balance(calldata, msg_sender, storage, sr)
            }
            IProtocolAgent::payCall::SELECTOR => self.pay(calldata, msg_sender, storage, sr),
            IProtocolAgent::batchPayCall::SELECTOR => {
                self.batch_pay(calldata, msg_sender, storage, sr)
            }
            IProtocolAgent::revokeAgentCall::SELECTOR => {
                self.revoke_agent(calldata, msg_sender, storage, sr)
            }
            IProtocolAgent::getAgentOwnerCall::SELECTOR => {
                self.get_agent_owner(calldata, storage, sr)
            }
            IProtocolAgent::getAgentBalanceCall::SELECTOR => {
                self.get_agent_balance(calldata, storage, sr)
            }
            IProtocolAgent::getAgentNameCall::SELECTOR => {
                self.get_agent_name(calldata, storage, sr)
            }
            IProtocolAgent::getAgentUrlCall::SELECTOR => self.get_agent_url(calldata, storage, sr),
            IProtocolAgent::getAgentAddressCall::SELECTOR => {
                self.get_agent_address(calldata, storage, sr)
            }
            IProtocolAgent::getAgentPermsCall::SELECTOR => {
                self.get_agent_perms(calldata, storage, sr)
            }
            IProtocolAgent::createSessionCall::SELECTOR => {
                self.create_session(calldata, msg_sender, storage, sr)
            }
            IProtocolAgent::revokeSessionCall::SELECTOR => {
                self.revoke_session(calldata, msg_sender, storage, sr)
            }
            IProtocolAgent::isSessionValidCall::SELECTOR => {
                self.is_session_valid(calldata, storage, sr)
            }
            IProtocolAgent::executeSessionTransferCall::SELECTOR => {
                self.execute_session_transfer(calldata, msg_sender, storage, sr)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{slot_balance, u128_to_u256, u256_to_u128, StatefulPrecompile, ASSET_ADDRESS};
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

        // registerAgent(name, url, agentAddress)
        let input = IProtocolAgent::registerAgentCall {
            name: "TestAgent".into(),
            url: "http://test.com".into(),
            agentAddress: Address::repeat_byte(0xBB),
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

        // getAgentAddress(0)
        let input = IProtocolAgent::getAgentAddressCall { agentId: 0 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let addr = Address::from_slice(&result.bytes[12..32]);
        assert_eq!(addr, Address::repeat_byte(0xBB));
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

        let agent = Address::repeat_byte(0xCC);
        let mut precompile = AgentPrecompile;

        // registerAgent
        let input = IProtocolAgent::registerAgentCall {
            name: "Agent".into(),
            url: "url".into(),
            agentAddress: agent,
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

        // pay(assetId=1, to=recipient, amount=1_000) — called by agent
        let input = IProtocolAgent::payCall {
            assetId: crate::CALL_ASSET_ID,
            to: recipient,
            amount: 1_000,
        }
        .abi_encode();
        let result = precompile.call(&input, agent, &mut provider);
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

        // Owner balance should be restored: 10_000 - 5_000 + 4_000 = 9_000
        let owner_slot = slot_balance(crate::CALL_ASSET_ID, sender);
        let owner_bal = provider.sload(ASSET_ADDRESS, owner_slot).unwrap();
        assert_eq!(u256_to_u128(owner_bal), 9_000);
    }

    #[test]
    fn test_agent_precompile_session_lifecycle() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        // Seed owner balance
        let owner_slot = slot_balance(crate::CALL_ASSET_ID, owner);
        provider
            .sstore(ASSET_ADDRESS, owner_slot, u128_to_u256(10_000))
            .unwrap();

        let mut precompile = AgentPrecompile;

        // registerAgent
        let input = IProtocolAgent::registerAgentCall {
            name: "Agent".into(),
            url: "url".into(),
            agentAddress: Address::repeat_byte(0xCC),
        }
        .abi_encode();
        precompile.call(&input, owner, &mut provider).unwrap();

        // createSession(delegate, perTxLimit=1_000, dailyLimit=2_000, expiresAt=100)
        let input = IProtocolAgent::createSessionCall {
            delegate,
            perTxLimit: 1_000,
            dailyLimit: 2_000,
            expiresAt: 100,
        }
        .abi_encode();
        let result = precompile.call(&input, owner, &mut provider);
        assert!(result.is_ok(), "createSession failed: {:?}", result.err());
        let session_id = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&result.unwrap().bytes[24..32]);
            buf
        });
        assert_eq!(session_id, 0);

        // isSessionValid(sessionId=0) at block 50
        let input = IProtocolAgent::isSessionValidCall {
            sessionId: 0,
        }
        .abi_encode();
        provider.set_block_number(50);
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let valid = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&result.bytes[24..32]);
            buf
        });
        assert_eq!(valid, 1);

        // executeSessionTransfer(sessionId=0, assetId=1, to=recipient, amount=800)
        let input = IProtocolAgent::executeSessionTransferCall {
            sessionId: 0,
            assetId: crate::CALL_ASSET_ID,
            to: recipient,
            amount: 800,
        }
        .abi_encode();
        let result = precompile.call(&input, delegate, &mut provider);
        assert!(
            result.is_ok(),
            "executeSessionTransfer failed: {:?}",
            result.err()
        );

        // Owner balance should be 9_200 (10_000 - 800)
        let owner_slot = slot_balance(crate::CALL_ASSET_ID, owner);
        let owner_bal = provider.sload(ASSET_ADDRESS, owner_slot).unwrap();
        assert_eq!(u256_to_u128(owner_bal), 9_200);

        // Recipient balance should be 800
        let recipient_slot = slot_balance(crate::CALL_ASSET_ID, recipient);
        let recipient_bal = provider.sload(ASSET_ADDRESS, recipient_slot).unwrap();
        assert_eq!(u256_to_u128(recipient_bal), 800);

        // revokeSession(sessionId=0)
        let input = IProtocolAgent::revokeSessionCall {
            sessionId: 0,
        }
        .abi_encode();
        let result = precompile.call(&input, owner, &mut provider);
        assert!(result.is_ok(), "revokeSession failed: {:?}", result.err());

        // isSessionValid should now return 0
        let input = IProtocolAgent::isSessionValidCall {
            sessionId: 0,
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let valid = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&result.bytes[24..32]);
            buf
        });
        assert_eq!(valid, 0);
    }

    #[test]
    fn test_agent_precompile_session_expired_rejected() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        let owner_slot = slot_balance(crate::CALL_ASSET_ID, owner);
        provider
            .sstore(ASSET_ADDRESS, owner_slot, u128_to_u256(10_000))
            .unwrap();

        let mut precompile = AgentPrecompile;

        let input = IProtocolAgent::registerAgentCall {
            name: "Agent".into(),
            url: "url".into(),
            agentAddress: Address::repeat_byte(0xCC),
        }
        .abi_encode();
        precompile.call(&input, owner, &mut provider).unwrap();

        let input = IProtocolAgent::createSessionCall {
            delegate,
            perTxLimit: 1_000,
            dailyLimit: 2_000,
            expiresAt: 100,
        }
        .abi_encode();
        precompile.call(&input, owner, &mut provider).unwrap();

        // Block 101 > expiresAt 100
        provider.set_block_number(101);

        let input = IProtocolAgent::executeSessionTransferCall {
            sessionId: 0,
            assetId: crate::CALL_ASSET_ID,
            to: recipient,
            amount: 500,
        }
        .abi_encode();
        let result = precompile.call(&input, delegate, &mut provider);
        assert!(
            result.is_err(),
            "expected expired session to be rejected"
        );
    }

    #[test]
    fn test_agent_address_can_pay() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let owner = Address::repeat_byte(0x22);
        let agent = Address::repeat_byte(0xCC);
        let recipient = Address::repeat_byte(0x44);

        let owner_slot = slot_balance(crate::CALL_ASSET_ID, owner);
        provider
            .sstore(ASSET_ADDRESS, owner_slot, u128_to_u256(10_000))
            .unwrap();

        let mut precompile = AgentPrecompile;

        // registerAgent with agentAddress
        let input = IProtocolAgent::registerAgentCall {
            name: "Agent".into(),
            url: "url".into(),
            agentAddress: agent,
        }
        .abi_encode();
        precompile.call(&input, owner, &mut provider).unwrap();

        // grantBalance
        let input = IProtocolAgent::grantBalanceCall {
            agentId: 0,
            assetId: crate::CALL_ASSET_ID,
            amount: 5_000,
        }
        .abi_encode();
        precompile.call(&input, owner, &mut provider).unwrap();

        // pay called by agent address (not owner)
        let input = IProtocolAgent::payCall {
            assetId: crate::CALL_ASSET_ID,
            to: recipient,
            amount: 800,
        }
        .abi_encode();
        let result = precompile.call(&input, agent, &mut provider);
        assert!(result.is_ok(), "agent pay failed: {:?}", result.err());

        // Agent balance should be 4_200
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
        assert_eq!(bal, 4_200);

        // Recipient balance should be 800
        let recipient_slot = slot_balance(crate::CALL_ASSET_ID, recipient);
        let recipient_bal = provider.sload(ASSET_ADDRESS, recipient_slot).unwrap();
        assert_eq!(u256_to_u128(recipient_bal), 800);
    }
}
