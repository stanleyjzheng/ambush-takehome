use axum::{
    extract::State as AxumState,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
use clap::Parser;
use serde::{Deserialize, Serialize};
use solana_sdk::instruction::Instruction;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Parser)]
struct CliArgs {
    #[clap(short, long, default_value_t = 3000)]
    port: u16,
}

#[derive(Clone)]
struct AppState {
    tx_submitter: Arc<tx_submission::TransactionSubmitter>,
}

#[derive(Serialize)]
struct SubmitTransactionResponse {
    signature: String,
}

#[derive(Deserialize)]
struct SubmitTransactionRequest {
    program_id: String,
    accounts: Vec<AccountMetaRequest>,
    data: Vec<u8>,
}

#[derive(Deserialize)]
struct AccountMetaRequest {
    pubkey: String,
    is_signer: bool,
    is_writable: bool,
}

#[derive(Deserialize)]
struct SubmitBase64TransactionRequest {
    base64_tx: String,
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    dotenv::dotenv().ok();
    let args = CliArgs::parse();

    let rpc_pool = Arc::new(rpc_pool::RpcPool::new().await);
    let tx_submitter = Arc::new(tx_submission::TransactionSubmitter::new(rpc_pool).await?);

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/submit", post(handle_submit_transaction))
        .route("/submit_base64", post(handle_submit_base64_transaction)) // New endpoint
        .with_state(AppState { tx_submitter });

    let addr = SocketAddr::from(([127, 0, 0, 1], args.port));
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn health_check() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

async fn handle_submit_transaction(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<SubmitTransactionRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let program_id = payload.program_id.parse().map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid program ID: {}", e),
        )
    })?;

    let accounts = payload
        .accounts
        .into_iter()
        .map(|acc| {
            acc.pubkey
                .parse()
                .map(|pubkey| solana_sdk::instruction::AccountMeta {
                    pubkey,
                    is_signer: acc.is_signer,
                    is_writable: acc.is_writable,
                })
                .map_err(|e| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("Invalid account pubkey: {}", e),
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let instruction = Instruction {
        program_id,
        accounts,
        data: payload.data,
    };

    state
        .tx_submitter
        .submit_transaction(vec![instruction])
        .await
        .map(|sig| {
            Json(SubmitTransactionResponse {
                signature: sig.to_string(),
            })
        })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn handle_submit_base64_transaction(
    AxumState(state): AxumState<AppState>,
    Json(payload): Json<SubmitBase64TransactionRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let tx_bytes = base64::engine::general_purpose::STANDARD
        .decode(&payload.base64_tx)
        .map_err(|e| {
            eprintln!("Base64 decode error: {:?}", e);
            (
                StatusCode::BAD_REQUEST,
                format!("Invalid base64 encoding: {}", e),
            )
        })?;
    eprintln!("Decoded bytes length: {}", tx_bytes.len());

    // Deserialize as a VersionedTransaction
    let versioned_tx: solana_sdk::transaction::VersionedTransaction =
        bincode::deserialize(&tx_bytes).map_err(|e| {
            eprintln!("Bincode deserialize error: {:?}", e);
            eprintln!(
                "First few bytes: {:?}",
                &tx_bytes[..std::cmp::min(tx_bytes.len(), 16)]
            );
            (
                StatusCode::BAD_REQUEST,
                format!("Failed to deserialize transaction: {}", e),
            )
        })?;

    // Extract instructions from the VersionedMessage field.
    // For both Legacy and V0 messages, we determine:
    // - is_signer: by checking if the account index is less than num_required_signatures.
    // - is_writable: using is_maybe_writable.
    let instructions: Vec<solana_sdk::instruction::Instruction> = match &versioned_tx.message {
        solana_sdk::message::VersionedMessage::Legacy(message) => {
            message
                .instructions
                .iter()
                .map(|instr| {
                    let program_id = message.account_keys[instr.program_id_index as usize];
                    let accounts = instr
                        .accounts
                        .iter()
                        .map(|&index| {
                            let pubkey = message.account_keys[index as usize];
                            // For both legacy and v0 messages, the first `num_required_signatures`
                            // in account_keys are signers.
                            let is_signer = (index as usize)
                                < (message.header.num_required_signatures as usize);
                            let is_writable = message.is_maybe_writable(index as usize, None);
                            solana_sdk::instruction::AccountMeta {
                                pubkey,
                                is_signer,
                                is_writable,
                            }
                        })
                        .collect();
                    solana_sdk::instruction::Instruction {
                        program_id,
                        accounts,
                        data: instr.data.clone(),
                    }
                })
                .collect()
        }
        solana_sdk::message::VersionedMessage::V0(message_v0) => {
            message_v0
                .instructions
                .iter()
                .map(|instr| {
                    let program_id = message_v0.account_keys[instr.program_id_index as usize];
                    let accounts = instr
                        .accounts
                        .iter()
                        .map(|&index| {
                            let pubkey = message_v0.account_keys[index as usize];
                            // In a V0 message, the ordering is the same:
                            let is_signer = (index as usize)
                                < (message_v0.header.num_required_signatures as usize);
                            let is_writable = message_v0.is_maybe_writable(index as usize, None);
                            solana_sdk::instruction::AccountMeta {
                                pubkey,
                                is_signer,
                                is_writable,
                            }
                        })
                        .collect();
                    solana_sdk::instruction::Instruction {
                        program_id,
                        accounts,
                        data: instr.data.clone(),
                    }
                })
                .collect()
        }
    };

    // Submit the transaction instructions
    let signature = state
        .tx_submitter
        .submit_transaction(instructions)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(SubmitTransactionResponse {
        signature: signature.to_string(),
    }))
}
