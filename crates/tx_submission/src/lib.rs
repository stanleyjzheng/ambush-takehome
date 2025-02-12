use reqwest::Client;
use serde::{Deserialize, Serialize};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::signer::Signer;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::Instruction,
    message::Message,
    signature::{read_keypair_file, Keypair, Signature},
    transaction::Transaction,
};
use std::error::Error;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

#[derive(Serialize)]
struct PriorityFeeRequest {
    jsonrpc: String,
    id: String,
    method: String,
    params: Vec<PriorityFeeParams>,
}

#[derive(Serialize)]
struct PriorityFeeParams {
    transaction: String,
    options: PriorityFeeOptions,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PriorityFeeOptions {
    recommended: bool,
    include_all_priority_fee_levels: bool,
}

#[derive(Deserialize)]
struct PriorityFeeResponse {
    result: PriorityFeeResult,
}

#[derive(Deserialize)]
struct PriorityFeeResult {
    #[serde(rename = "priorityFeeLevels")]
    priority_fee_levels: PriorityFeeLevels,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PriorityFeeLevels {
    min: f64,
    low: f64,
    medium: f64,
    high: f64,
    very_high: f64,
    unsafe_max: f64,
}

async fn get_priority_fee(
    client: &Client,
    helius_api_key: &str,
    transaction: &Transaction,
) -> Result<u64, Box<dyn Error + Send + Sync>> {
    let transaction_base58 = bs58::encode(bincode::serialize(transaction)?).into_string();

    let request = PriorityFeeRequest {
        jsonrpc: "2.0".to_string(),
        id: "helius-example".to_string(),
        method: "getPriorityFeeEstimate".to_string(),
        params: vec![PriorityFeeParams {
            transaction: transaction_base58,
            options: PriorityFeeOptions {
                recommended: true,
                include_all_priority_fee_levels: true,
            },
        }],
    };

    let url = format!("https://mainnet.helius-rpc.com/?api-key={}", helius_api_key);
    let response: PriorityFeeResponse = client
        .post(&url)
        .json(&request)
        .send()
        .await?
        .json()
        .await?;

    // TODO: adjust high/very high depending on the previous tx results
    Ok(response.result.priority_fee_levels.high as u64)
}

/// Creates and signs a transaction with the given instructions, blockhash, and priority fee.
async fn create_transaction_with_priority_fee(
    keypair: &Keypair,
    instructions: &[Instruction],
    blockhash: Hash,
    priority_fee: u64,
) -> Transaction {
    // Add compute budget instruction for priority fee
    let mut final_instructions = vec![ComputeBudgetInstruction::set_compute_unit_price(
        priority_fee,
    )];
    final_instructions.extend_from_slice(instructions);

    let message = Message::new(&final_instructions, Some(&keypair.pubkey()));
    let mut tx = Transaction::new_unsigned(message);
    tx.try_sign(&[keypair], blockhash)
        .expect("Failed to sign transaction");
    tx
}

/// Sends the transaction to a specific RPC endpoint.
async fn send_transaction_to_rpc(
    rpc_url: &str,
    transaction: Transaction,
) -> Result<Signature, Box<dyn Error + Send + Sync>> {
    let rpc_url = rpc_url.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let client = RpcClient::new(rpc_url);
        let config = RpcSendTransactionConfig {
            skip_preflight: true,
            preflight_commitment: None,
            ..Default::default()
        };
        client.send_transaction_with_config(&transaction, config)
    })
    .await?;

    result.map_err(|e| e.into())
}

async fn spam_transactions(
    rpc_urls: Vec<String>,
    blockhashes: Vec<Hash>,
    keypair: &Keypair,
    instructions: Vec<Instruction>,
    timeout_duration: Duration,
    helius_api_key: &str,
) -> Option<Signature> {
    let http_client = Client::new();
    let (result_tx, mut result_rx) = mpsc::channel(1);

    for blockhash in &blockhashes {
        // Create a base transaction to estimate priority fees
        let base_transaction = create_transaction_with_priority_fee(
            keypair,
            &instructions,
            *blockhash,
            0, // temporary zero fee for estimation
        )
        .await;

        // Get priority fee estimate
        let priority_fee =
            match get_priority_fee(&http_client, helius_api_key, &base_transaction).await {
                Ok(fee) => fee,
                Err(e) => {
                    eprintln!("Failed to get priority fee: {}", e);
                    continue;
                }
            };

        // Create final transaction with priority fee
        let transaction =
            create_transaction_with_priority_fee(keypair, &instructions, *blockhash, priority_fee)
                .await;

        for rpc_url in &rpc_urls {
            let rpc_url_clone = rpc_url.clone();
            let transaction_clone = transaction.clone();
            let mut result_tx_clone = result_tx.clone();

            tokio::spawn(async move {
                if let Ok(sig) = send_transaction_to_rpc(&rpc_url_clone, transaction_clone).await {
                    let _ = result_tx_clone.send(sig).await;
                }
            });
        }
    }
    drop(result_tx);

    match timeout(timeout_duration, result_rx.recv()).await {
        Ok(Some(signature)) => Some(signature),
        _ => None,
    }
}

#[tokio::main]
async fn main() {
    dotenv::dotenv().ok();

    let helius_api_key =
        std::env::var("HELIUS_API_KEY").expect("HELIUS_API_KEY must be set in environment");

    let keypair_path = shellexpand::tilde("~/.config/solana/id.json").to_string();
    let keypair = read_keypair_file(&keypair_path)
        .expect("Failed to read keypair from ~/.config/solana/id.json");

    let rpc_urls = vec!["https://api.mainnet-beta.solana.com".to_string()];

    let primary_rpc = RpcClient::new(rpc_urls[0].clone());
    let recent_blockhash = primary_rpc
        .get_latest_blockhash()
        .expect("Failed to get recent blockhash");
    let blockhashes = vec![recent_blockhash];

    let instructions: Vec<Instruction> = vec![];
    let timeout_duration = Duration::from_secs(10);

    if let Some(signature) = spam_transactions(
        rpc_urls,
        blockhashes,
        &keypair,
        instructions,
        timeout_duration,
        &helius_api_key,
    )
    .await
    {
        println!("Transaction landed with signature: {}", signature);
    } else {
        println!("Transaction was not confirmed within the timeout period.");
    }
}
