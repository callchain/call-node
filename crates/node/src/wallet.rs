//! Wallet CLI subcommands — generate keys, query balances, send payments.

#![allow(clippy::print_stdout)]

use call_primitives::Address;
use reqwest::Client;
use std::error::Error;

fn rpc_body(method: &str, params: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1,
    })
    .to_string()
}

async fn rpc_call(
    client: &Client,
    rpc_url: &str,
    body: String,
) -> Result<Option<serde_json::Value>, Box<dyn Error + Send + Sync>> {
    let resp = client
        .post(rpc_url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await?
        .text()
        .await?;

    let parsed: serde_json::Value = serde_json::from_str(&resp)?;
    if let Some(error) = parsed.get("error") {
        tracing::error!(error = %error, "RPC error");
        return Ok(None);
    }
    Ok(parsed.get("result").cloned())
}

/// Generate a new secp256k1 keypair and output the address.
pub fn generate_keys() -> Result<(), Box<dyn Error + Send + Sync>> {
    let secret_key = k256::ecdsa::SigningKey::random(&mut rand::thread_rng());
    let public_key = secret_key.verifying_key();

    let uncompressed = public_key.to_encoded_point(false);
    let pk_bytes = uncompressed.as_bytes();
    let hash = call_crypto::keccak256(&pk_bytes[1..]);
    let address = Address::from_slice(&hash.0[12..]);

    let secret_hex = hex::encode(secret_key.to_bytes());
    let pubkey_hex = hex::encode(&pk_bytes[1..]);
    let address_hex = hex::encode(address.as_slice());

    println!("{{");
    println!("  \"secretKey\": \"0x{secret_hex}\",");
    println!("  \"publicKey\": \"0x{pubkey_hex}\",");
    println!("  \"address\": \"0x{address_hex}\"");
    println!("}}");

    Ok(())
}

/// Derive address from a public key.
pub fn derive_address(pubkey_hex: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let bytes = hex::decode(pubkey_hex.trim_start_matches("0x"))?;
    let hash = call_crypto::keccak256(&bytes);
    let address = Address::from_slice(&hash.0[12..]);
    println!("0x{}", hex::encode(address.as_slice()));
    Ok(())
}

/// Query balance via RPC.
pub async fn query_balance(
    address: &str,
    asset_id: u64,
    rpc_url: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let client = Client::new();
    let body = rpc_body(
        "call_protocolBalance",
        serde_json::json!([asset_id, address]),
    );

    if let Some(result) = rpc_call(&client, rpc_url, body).await? {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

    Ok(())
}

/// Send a payment via RPC.
#[allow(clippy::too_many_arguments)]
pub async fn send_payment(
    from_key: &str,
    to: &str,
    asset_id: u64,
    amount: u128,
    nonce: u64,
    rpc_url: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let secret_bytes = hex::decode(from_key.trim_start_matches("0x"))?;
    let secret_key = k256::ecdsa::SigningKey::from_slice(&secret_bytes)?;
    let public_key = secret_key.verifying_key();
    let uncompressed = public_key.to_encoded_point(false);
    let pk_bytes = uncompressed.as_bytes();
    let hash = call_crypto::keccak256(&pk_bytes[1..]);
    let sender = Address::from_slice(&hash.0[12..]);

    let params = serde_json::json!([{
        "from": format!("0x{}", hex::encode(sender.as_slice())),
        "to": to,
        "assetId": asset_id,
        "amount": amount,
        "nonce": nonce,
    }]);

    let client = Client::new();
    let body = rpc_body("call_sendPayment", params);

    if let Some(result) = rpc_call(&client, rpc_url, body).await? {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

    Ok(())
}

/// Query node server info.
pub async fn server_info(rpc_url: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let client = Client::new();
    let body = rpc_body("server_info", serde_json::json!([]));

    if let Some(result) = rpc_call(&client, rpc_url, body).await? {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

    Ok(())
}

/// Query mempool stats.
pub async fn mempool_stats(rpc_url: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let client = Client::new();
    let body = rpc_body("txpool_status", serde_json::json!([]));

    if let Some(result) = rpc_call(&client, rpc_url, body).await? {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }

    Ok(())
}
