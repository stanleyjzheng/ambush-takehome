use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::{
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

/// Creates and signs a transaction with the given instructions and blockhash.
fn create_transaction(
    keypair: &Keypair,
    instructions: &[Instruction],
    blockhash: Hash,
) -> Transaction {
    let message = Message::new(instructions, Some(&keypair.pubkey()));
    let mut tx = Transaction::new_unsigned(message);
    tx.try_sign(&[keypair], blockhash)
        .expect("Failed to sign transaction");
    tx
}

/// Sends the transaction to a specific RPC endpoint.
/// This function disables preflight checks by setting skip_preflight to true.
/// It spawns a blocking task since RpcClient is synchronous.
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
            // Other fields default (no extra retries, etc.)
            ..Default::default()
        };
        // Note: The duplicate-prevention in your on-chain program ensures that
        // only one of these transactions will eventually land.
        client.send_transaction_with_config(&transaction, config)
    })
    .await?;

    result.map_err(|e| e.into())
}

/// Spams the network by submitting a transaction to each combination of the provided
/// RPC endpoints and blockhashes. It stops spamming when one transaction succeeds
/// or after the specified timeout.
async fn spam_transactions(
    rpc_urls: Vec<String>,
    blockhashes: Vec<Hash>,
    keypair: &Keypair,
    instructions: Vec<Instruction>,
    timeout_duration: Duration,
) -> Option<Signature> {
    // Use a channel to receive the first successful signature.
    let (result_tx, mut result_rx) = mpsc::channel(1);

    // For each blockhash, create a transaction and send it to each RPC endpoint.
    for blockhash in &blockhashes {
        let transaction = create_transaction(keypair, &instructions, *blockhash);
        for rpc_url in &rpc_urls {
            let rpc_url_clone = rpc_url.clone();
            let transaction_clone = transaction.clone();
            let mut result_tx_clone = result_tx.clone();
            // Spawn a task for each (RPC, blockhash) combination.
            tokio::spawn(async move {
                if let Ok(sig) = send_transaction_to_rpc(&rpc_url_clone, transaction_clone).await {
                    // If sending is successful, send the signature back.
                    let _ = result_tx_clone.send(sig).await;
                }
            });
        }
    }
    // Drop the original sender so that the channel will close if no task succeeds.
    drop(result_tx);

    // Wait until we get a signature or timeout after the specified duration.
    match timeout(timeout_duration, result_rx.recv()).await {
        Ok(Some(signature)) => Some(signature),
        _ => None,
    }
}

#[tokio::main]
async fn main() {
    // Load the keypair from id.json (expanding the "~" using shellexpand).
    let keypair_path = shellexpand::tilde("~/.config/solana/id.json").to_string();
    let keypair = read_keypair_file(&keypair_path)
        .expect("Failed to read keypair from ~/.config/solana/id.json");

    // Define RPC endpoints. Make sure these are HTTP(S) endpoints that support TPU.
    // You can include as many as you like.
    let rpc_urls = vec![
        "https://api.devnet.solana.com".to_string(),
        // "https://api.mainnet-beta.solana.com".to_string(),
        // Add more endpoints if desired.
    ];

    // For demonstration, we get one recent blockhash and duplicate it.
    // In a real scenario, you might pass in multiple blockhashes.
    let primary_rpc = RpcClient::new(rpc_urls[0].clone());
    let (recent_blockhash, _) = primary_rpc
        .get_recent_blockhash()
        .expect("Failed to get recent blockhash");
    let blockhashes = vec![recent_blockhash, recent_blockhash];

    // Define the transaction instructions.
    // Replace the empty vector below with your actual instructions.
    let instructions: Vec<Instruction> = vec![];

    // Set the total time to spam transactions (10 seconds).
    let timeout_duration = Duration::from_secs(10);

    // Start spamming transactions.
    if let Some(signature) = spam_transactions(
        rpc_urls,
        blockhashes,
        &keypair,
        instructions,
        timeout_duration,
    )
    .await
    {
        println!("Transaction landed with signature: {}", signature);
    } else {
        println!("Transaction was not confirmed within the timeout period.");
    }
}
