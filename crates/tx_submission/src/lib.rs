use anyhow::{anyhow, Context, Result};
use cached::proc_macro::cached;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::instruction::AccountMeta;
use solana_sdk::message::MessageHeader;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::read_keypair_file;
use solana_sdk::signer::Signer;
use solana_sdk::system_instruction;
use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    instruction::{CompiledInstruction, Instruction},
    message::{Message, VersionedMessage},
    signature::{Keypair, Signature},
    transaction::{Transaction, VersionedTransaction},
};
use std::env;
use std::error::Error;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;
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
#[allow(dead_code)]
struct PriorityFeeLevels {
    min: f64,
    low: f64,
    medium: f64,
    high: f64,
    very_high: f64,
    unsafe_max: f64,
}

/// Creates and signs a transaction with the given instructions, blockhash, and priority fee.
async fn create_transaction_with_priority_fee(
    keypair: &Keypair,
    instructions: &[Instruction],
    blockhash: Hash,
    priority_fee: u64,
) -> Transaction {
    println!("instructions {:?}", instructions);
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
            skip_preflight: false,
            preflight_commitment: None,
            ..Default::default()
        };
        client.send_transaction_with_config(&transaction, config)
    })
    .await?;

    result.map_err(|e| e.into())
}

#[cached(
    result = true,
    time = 300,
    key = "String", // Key is a String (base58 transaction)
    convert = r#"{ bs58::encode(bincode::serialize(transaction)?).into_string().clone() }"#
)]
async fn get_priority_fee(
    client: &Client,
    helius_api_key: &str,
    transaction: &Transaction,
) -> Result<u64> {
    let transaction_base58 = bs58::encode(bincode::serialize(transaction)?).into_string();

    let request = PriorityFeeRequest {
        jsonrpc: "2.0".to_string(),
        id: "placeholder".to_string(),
        method: "getPriorityFeeEstimate".to_string(),
        params: vec![PriorityFeeParams {
            transaction: transaction_base58,
            options: PriorityFeeOptions {
                include_all_priority_fee_levels: true,
            },
        }],
    };

    let url = format!("https://mainnet.helius-rpc.com/?api-key={}", helius_api_key);

    let response_text = client
        .post(&url)
        .json(&request)
        .send()
        .await?
        .text()
        .await?;

    let response: PriorityFeeResponse = serde_json::from_str(&response_text)
        .with_context(|| format!("Failed to parse response: {}", response_text))?;

    Ok(response.result.priority_fee_levels.high as u64)
}

async fn wait_for_confirmation(
    rpc_url: &str,
    signature: &Signature,
    timeout_duration: Duration,
) -> Result<bool> {
    let start = std::time::Instant::now();
    let client = RpcClient::new(rpc_url.to_string());

    while start.elapsed() < timeout_duration {
        match client.get_signature_status(signature) {
            Ok(Some(status)) => {
                return Ok(status.is_ok());
            }
            Ok(None) => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
            Err(e) => {
                eprintln!("Error checking signature status: {}", e);
                break;
            }
        }
    }
    Ok(false)
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
    let should_stop = Arc::new(AtomicBool::new(false));

    for blockhash in &blockhashes {
        // Create a base transaction to estimate priority fees
        let base_transaction =
            create_transaction_with_priority_fee(keypair, &instructions, *blockhash, 0).await;

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
            let result_tx_clone = result_tx.clone();
            let should_stop_clone = should_stop.clone();

            tokio::spawn(async move {
                // Check if we should stop before sending
                if should_stop_clone.load(Ordering::Relaxed) {
                    return;
                }

                if let Ok(sig) = send_transaction_to_rpc(&rpc_url_clone, transaction_clone).await {
                    // Check if the transaction is confirmed
                    if let Ok(true) =
                        wait_for_confirmation(&rpc_url_clone, &sig, Duration::from_secs(30)).await
                    {
                        let _ = result_tx_clone.send((sig, true)).await;
                    } else {
                        let _ = result_tx_clone.send((sig, false)).await;
                    }
                }
            });
        }
    }
    drop(result_tx);

    let mut confirmed_signature = None;

    while let Ok(Some((signature, is_confirmed))) =
        timeout(timeout_duration, result_rx.recv()).await
    {
        if is_confirmed {
            confirmed_signature = Some(signature);
            should_stop.store(true, Ordering::Relaxed);
            break;
        }
    }

    confirmed_signature
}

pub struct TransactionSubmitter {
    rpc_pool: Arc<rpc_pool::RpcPool>,
    helius_api_key: String,
    keypair: Keypair,
}

impl TransactionSubmitter {
    pub async fn new(rpc_pool: Arc<rpc_pool::RpcPool>) -> Result<Self> {
        dotenv::dotenv().ok();
        let helius_api_key = env::var("HELIUS_API_KEY")?;
        let keypair_path = shellexpand::tilde("~/.config/solana/id.json").to_string();
        let keypair = read_keypair_file(&keypair_path)
            .map_err(|e| anyhow!("Failed to read keypair from {}: {}", keypair_path, e))?;

        Ok(Self {
            rpc_pool,
            helius_api_key,
            keypair,
        })
    }

    pub async fn submit_versioned_transaction(
        &self,
        original_tx: solana_sdk::transaction::VersionedTransaction,
    ) -> Result<Signature> {
        let blockhashes: Vec<Hash> = {
            let cache = self.rpc_pool.cache.lock().unwrap();
            cache
                .get_all()
                .into_iter()
                .filter_map(|bh| bh.blockhash.parse().ok())
                .collect()
        };

        let rpc_urls = {
            let tracker = self.rpc_pool.latency_tracker.lock().unwrap();
            tracker.get_sorted_rpcs()
        };

        spam_versioned_transactions(
            rpc_urls,
            blockhashes,
            original_tx,
            &self.keypair,
            Duration::from_secs(30),
            &self.helius_api_key,
        )
        .await
        .context("Failed to submit and confirm transaction")
    }
}

fn instruction_to_compiled(
    instruction: &Instruction,
    account_keys: &mut Vec<Pubkey>,
) -> CompiledInstruction {
    // For each account in the instruction, find its index in `account_keys`.
    // If it isn’t present, append it.
    let accounts: Vec<u8> = instruction
        .accounts
        .iter()
        .map(
            |meta| match account_keys.iter().position(|key| key == &meta.pubkey) {
                Some(idx) => idx as u8,
                None => {
                    account_keys.push(meta.pubkey);
                    (account_keys.len() - 1) as u8
                }
            },
        )
        .collect();

    // Do the same for the program id.
    let program_id_index = match account_keys
        .iter()
        .position(|key| key == &instruction.program_id)
    {
        Some(idx) => idx as u8,
        None => {
            account_keys.push(instruction.program_id);
            (account_keys.len() - 1) as u8
        }
    };

    CompiledInstruction {
        program_id_index,
        accounts,
        data: instruction.data.clone(),
    }
}

#[derive(Clone)]
struct AccountInfo {
    pubkey: Pubkey,
    is_signer: bool,
    is_writable: bool,
}

fn add_account_meta(account_infos: &mut Vec<AccountInfo>, meta: &AccountMeta) -> u8 {
    if let Some(pos) = account_infos.iter().position(|a| a.pubkey == meta.pubkey) {
        // If the account already exists, update its flags if needed.
        account_infos[pos].is_signer |= meta.is_signer;
        account_infos[pos].is_writable |= meta.is_writable;
        pos as u8
    } else {
        account_infos.push(AccountInfo {
            pubkey: meta.pubkey,
            is_signer: meta.is_signer,
            is_writable: meta.is_writable,
        });
        (account_infos.len() - 1) as u8
    }
}

fn instruction_to_compiled_with_metadata(
    instruction: &Instruction,
    account_infos: &mut Vec<AccountInfo>,
) -> CompiledInstruction {
    let accounts: Vec<u8> = instruction
        .accounts
        .iter()
        .map(|meta| add_account_meta(account_infos, meta))
        .collect();

    // For the program id, assume it is not a signer and not writable.
    let program_id_meta = AccountMeta {
        pubkey: instruction.program_id,
        is_signer: false,
        is_writable: false,
    };
    let program_id_index = add_account_meta(account_infos, &program_id_meta);

    CompiledInstruction {
        program_id_index,
        accounts,
        data: instruction.data.clone(),
    }
}

fn finalize_account_keys_and_header(
    account_infos: Vec<AccountInfo>,
) -> (Vec<Pubkey>, MessageHeader) {
    // Split into signers and non–signers.
    let mut signers: Vec<AccountInfo> = account_infos
        .clone()
        .into_iter()
        .filter(|a| a.is_signer)
        .collect();
    let mut non_signers: Vec<AccountInfo> =
        account_infos.into_iter().filter(|a| !a.is_signer).collect();

    // Sort signers: writable first, then read–only.
    signers.sort_by_key(|a| !a.is_writable);
    // Sort non–signers: writable first, then read–only.
    non_signers.sort_by_key(|a| !a.is_writable);

    // Concatenate to form the final account keys in order.
    let ordered_accounts: Vec<AccountInfo> =
        signers.into_iter().chain(non_signers.into_iter()).collect();
    let account_keys = ordered_accounts
        .iter()
        .map(|a| a.pubkey)
        .collect::<Vec<_>>();

    let num_required_signatures = ordered_accounts.iter().filter(|a| a.is_signer).count() as u8;
    let num_readonly_signed_accounts = ordered_accounts
        .iter()
        .filter(|a| a.is_signer && !a.is_writable)
        .count() as u8;
    let num_readonly_unsigned_accounts = ordered_accounts
        .iter()
        .filter(|a| !a.is_signer && !a.is_writable)
        .count() as u8;

    let header = MessageHeader {
        num_required_signatures,
        num_readonly_signed_accounts,
        num_readonly_unsigned_accounts,
    };

    (account_keys, header)
}

async fn spam_versioned_transactions(
    rpc_urls: Vec<String>,
    blockhashes: Vec<Hash>,
    original_tx: VersionedTransaction,
    keypair: &Keypair,
    timeout_duration: Duration,
    helius_api_key: &str,
) -> Option<Signature> {
    let http_client = Client::new();
    let (result_tx, mut result_rx) = mpsc::channel(1);
    let should_stop = Arc::new(AtomicBool::new(false));

    for blockhash in &blockhashes {
        // Create a version of the transaction with this blockhash but no priority fee yet
        let mut base_tx = original_tx.clone();

        // Update the blockhash
        match &mut base_tx.message {
            VersionedMessage::Legacy(message) => {
                message.recent_blockhash = *blockhash;
            }
            VersionedMessage::V0(message) => {
                message.recent_blockhash = *blockhash;
            }
        }

        // Convert VersionedTransaction to Transaction for priority fee estimation
        let priority_tx = match &base_tx.message {
            VersionedMessage::Legacy(message) => Transaction::new_unsigned(message.clone()),
            VersionedMessage::V0(message) => {
                let legacy_message = Message::new(
                    &message
                        .instructions
                        .iter()
                        .map(|ci| {
                            let program_id = message.account_keys[ci.program_id_index as usize];
                            let accounts = ci
                                .accounts
                                .iter()
                                .map(|&idx| {
                                    let pubkey = message.account_keys[idx as usize];
                                    let is_signer = (idx as usize)
                                        < message.header.num_required_signatures as usize;
                                    let is_writable = message.is_maybe_writable(idx as usize, None);
                                    AccountMeta {
                                        pubkey,
                                        is_signer,
                                        is_writable,
                                    }
                                })
                                .collect();
                            Instruction {
                                program_id,
                                accounts,
                                data: ci.data.clone(),
                            }
                        })
                        .collect::<Vec<_>>(),
                    Some(&message.account_keys[0]),
                );
                Transaction::new_unsigned(legacy_message)
            }
        };

        // Get priority fee estimate using the converted transaction
        let priority_fee = match get_priority_fee(&http_client, helius_api_key, &priority_tx).await
        {
            Ok(fee) => fee,
            Err(e) => {
                eprintln!("Failed to get priority fee: {}", e);
                continue;
            }
        };

        // Create the final transaction with priority fee
        let mut final_tx = base_tx.clone();
        let priority_fee_ix = ComputeBudgetInstruction::set_compute_unit_price(priority_fee);

        let mut hasher = Sha256::new();
        hasher.update(format!("{:?}", original_tx));
        let original_hash = hasher.finalize();

        let duplication_ix = build_de_duplication_instructions(
            &keypair.pubkey(),
            original_hash.as_slice(),
            &RpcClient::new(rpc_urls[0].clone()),
            1,
        )
        .expect("Failed to build de-duplication instructions");

        // Add priority fee instruction as the first instruction after removing existing ones
        match &mut final_tx.message {
            VersionedMessage::Legacy(message) => {
                let mut account_infos: Vec<AccountInfo> = message
                    .account_keys
                    .iter()
                    .enumerate()
                    .map(|(i, key)| {
                        let is_signer = i < message.header.num_required_signatures as usize;
                        let is_writable = i
                            < (message.header.num_required_signatures as usize
                                - message.header.num_readonly_signed_accounts as usize);
                        AccountInfo {
                            pubkey: *key,
                            is_signer,
                            is_writable,
                        }
                    })
                    .collect();

                let filtered_instructions: Vec<CompiledInstruction> = message
                    .instructions
                    .iter()
                    .filter(|ci| {
                        let program_id = message.account_keys[ci.program_id_index as usize];
                        if program_id == solana_sdk::compute_budget::id() {
                            ci.data.first().map(|b| *b != 3).unwrap_or(true)
                        } else {
                            true
                        }
                    })
                    .cloned()
                    .collect();

                let mut new_compiled = vec![instruction_to_compiled_with_metadata(
                    &priority_fee_ix,
                    &mut account_infos,
                )];
                new_compiled.extend_from_slice(&filtered_instructions);
                new_compiled.extend(
                    duplication_ix
                        .iter()
                        .map(|ix| instruction_to_compiled_with_metadata(ix, &mut account_infos)),
                );

                let (final_account_keys, new_header) =
                    finalize_account_keys_and_header(account_infos);
                message.account_keys = final_account_keys;
                message.header = new_header;
                message.instructions = new_compiled;
            }
            VersionedMessage::V0(message) => {
                let mut account_infos: Vec<AccountInfo> = message
                    .account_keys
                    .iter()
                    .enumerate()
                    .map(|(i, key)| {
                        let is_signer = i < message.header.num_required_signatures as usize;
                        let is_writable = i
                            < (message.header.num_required_signatures as usize
                                - message.header.num_readonly_signed_accounts as usize);
                        AccountInfo {
                            pubkey: *key,
                            is_signer,
                            is_writable,
                        }
                    })
                    .collect();

                let filtered_instructions: Vec<CompiledInstruction> = message
                    .instructions
                    .iter()
                    .filter(|ci| {
                        let program_id = message.account_keys[ci.program_id_index as usize];
                        if program_id == solana_sdk::compute_budget::id() {
                            ci.data.first().map(|b| *b != 3).unwrap_or(true)
                        } else {
                            true
                        }
                    })
                    .cloned()
                    .collect();

                let mut new_compiled = vec![instruction_to_compiled_with_metadata(
                    &priority_fee_ix,
                    &mut account_infos,
                )];
                new_compiled.extend_from_slice(&filtered_instructions);
                new_compiled.extend(
                    duplication_ix
                        .iter()
                        .map(|ix| instruction_to_compiled_with_metadata(ix, &mut account_infos)),
                );

                let (final_account_keys, new_header) =
                    finalize_account_keys_and_header(account_infos);
                message.account_keys = final_account_keys;
                message.header = new_header;
                message.instructions = new_compiled;
            }
        }

        // Re-sign the transaction
        let final_tx =
            VersionedTransaction::try_new(final_tx.message, &[keypair]).expect("Failed to sign tx");

        for rpc_url in &rpc_urls {
            let rpc_url_clone = rpc_url.clone();
            let tx_clone = final_tx.clone();
            let result_tx_clone = result_tx.clone();
            let should_stop_clone = should_stop.clone();

            tokio::spawn(async move {
                if should_stop_clone.load(Ordering::Relaxed) {
                    return;
                }

                if let Ok(sig) = send_versioned_transaction_to_rpc(&rpc_url_clone, tx_clone).await {
                    if let Ok(true) =
                        wait_for_confirmation(&rpc_url_clone, &sig, Duration::from_secs(30)).await
                    {
                        let _ = result_tx_clone.send((sig, true)).await;
                    } else {
                        let _ = result_tx_clone.send((sig, false)).await;
                    }
                }
            });
        }
    }

    // Rest of the implementation remains the same
    drop(result_tx);

    let mut confirmed_signature = None;
    while let Ok(Some((signature, is_confirmed))) =
        timeout(timeout_duration, result_rx.recv()).await
    {
        if is_confirmed {
            confirmed_signature = Some(signature);
            should_stop.store(true, Ordering::Relaxed);
            break;
        }
    }

    confirmed_signature
}

/// Builds a set of instructions to interact with the duplicate-prevention contract.
///
/// - `signer_pubkey`: The public key of the transaction sender.
/// - `tx_id`: A 32-byte SHA-256 hash that uniquely identifies this transaction.
/// - `rpc_client`: An RPC client used to check if the PDA account exists.
/// - `rent_exemption_lamports`: The amount of lamports required for rent exemption (for 1 byte).
///
/// Returns a vector of instructions:
///   - (Optionally) a system instruction to create the PDA account if it does not exist.
///   - The instruction that calls your contract with `tx_id` as its instruction data.
///
/// The on-chain program expects two accounts:
///   0. The signer (writable & signer)
///   1. The PDA (writable, not a signer)
pub fn build_de_duplication_instructions(
    signer_pubkey: &Pubkey,
    tx_id: &[u8],
    rpc_client: &RpcClient,
    rent_exemption_lamports: u64,
) -> Result<Vec<Instruction>, Box<dyn Error>> {
    // 1. Fetch the program ID from an environment variable.
    //    Make sure you have set DUPLICATE_TX_PREVENTION_PROGRAM_ID
    let program_id_str = env::var("DUPLICATE_TX_PREVENTION_PROGRAM_ID")
        .map_err(|_| "Environment variable DUPLICATE_TX_PREVENTION_PROGRAM_ID is not set.")?;
    let program_id = Pubkey::from_str(&program_id_str)?;

    // 2. Derive the PDA.
    //    Both client and on-chain program use the same seeds: a fixed seed, the signer, and the tx_id.
    let (pda, _bump) =
        Pubkey::find_program_address(&[b"unique", signer_pubkey.as_ref(), tx_id], &program_id);

    let mut instructions = Vec::new();

    // 3. Check if the PDA exists. If not, we add an instruction to create it.
    if rpc_client.get_account(&pda).is_err() {
        // The PDA account needs to be created with 1 byte of space and assigned to the program.
        let create_pda_ix = system_instruction::create_account(
            signer_pubkey,           // Funding account (must be a signer)
            &pda,                    // The PDA account to be created
            rent_exemption_lamports, // Rent-exempt lamports for 1 byte
            1,                       // Account space (in bytes)
            &program_id,             // Owner of the new account (your program)
        );
        instructions.push(create_pda_ix);
    }

    // 4. Build the contract interaction instruction.
    //    The instruction data starts with the 32-byte tx_id.
    let instruction_data = tx_id.to_vec();
    // You may append additional payload data here if your contract expects more.

    let contract_ix = Instruction {
        // The program ID will be added to the transaction’s account keys.
        // When the transaction is compiled, the index of the program ID in the account list
        // will be used as `program_id_index` in the compiled instruction.
        program_id,
        accounts: vec![
            // The on-chain program expects:
            //   0: Signer account (writable, signer)
            AccountMeta::new(*signer_pubkey, true),
            //   1: PDA account (writable, not a signer)
            AccountMeta::new(pda, false),
        ],
        data: instruction_data,
    };
    instructions.push(contract_ix);

    Ok(instructions)
}

async fn send_versioned_transaction_to_rpc(
    rpc_url: &str,
    transaction: VersionedTransaction,
) -> Result<Signature, Box<dyn Error + Send + Sync>> {
    let rpc_url = rpc_url.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let client = RpcClient::new(rpc_url);
        let config = RpcSendTransactionConfig {
            skip_preflight: false,
            preflight_commitment: None,
            // Remove the encoding configuration since it's not needed
            ..Default::default()
        };
        client.send_transaction_with_config(&transaction, config)
    })
    .await?;

    result.map_err(|e| e.into())
}
