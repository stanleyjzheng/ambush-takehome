use anyhow::{anyhow, Result};
use clap::Parser;
use dotenv::dotenv;
use solana_client::rpc_client::RpcClient;
use std::env;

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct SendTransactionArgs {
    #[arg(long, help = "Base58 encoded transaction to send")]
    transaction: String,
}

impl SendTransactionArgs {
    pub fn process(&self, client: &RpcClient) -> Result<()> {
        let bytes = bs58::decode(&self.transaction)
            .into_vec()
            .map_err(|_| anyhow!("Failed to decode base58 transaction"))?;

        let transaction = bincode::deserialize::<solana_sdk::transaction::Transaction>(&bytes)
            .map_err(|e| anyhow!("Invalid transaction data: {}", e))?;

        let signature = client
            .send_transaction(&transaction)
            .map_err(|e| anyhow!("Transaction failed to send: {:?}", e))?;

        println!("Transaction sent with signature: {}", signature);

        Ok(())
    }
}

fn main() -> Result<()> {
    dotenv().ok();

    let rpc_url = env::var("RPC_URL").unwrap();
    let client = RpcClient::new(rpc_url);

    let args = SendTransactionArgs::parse();
    args.process(&client)
}
