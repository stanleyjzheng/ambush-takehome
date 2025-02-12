use axum::{
    extract::State as AxumState,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
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
struct AccountMetaRequest {
    pubkey: String,
    is_signer: bool,
    is_writable: bool,
}

#[derive(Deserialize)]
struct SubmitTransactionRequest {
    program_id: String,
    accounts: Vec<AccountMetaRequest>,
    data: Vec<u8>,
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
