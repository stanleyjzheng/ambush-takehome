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

    let versioned_tx: solana_sdk::transaction::VersionedTransaction =
        bincode::deserialize(&tx_bytes).map_err(|e| {
            eprintln!("Bincode deserialize error: {:?}", e);
            (
                StatusCode::BAD_REQUEST,
                format!("Failed to deserialize transaction: {}", e),
            )
        })?;

    let signature = state
        .tx_submitter
        .submit_versioned_transaction(versioned_tx)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(SubmitTransactionResponse {
        signature: signature.to_string(),
    }))
}
