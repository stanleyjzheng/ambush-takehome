use std::collections::{HashMap, VecDeque};
use std::env;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dotenv::dotenv;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::time;

//
// A record for a fetched blockhash along with its expiry.
//
#[derive(Debug, Clone)]
struct CachedBlockhash {
    blockhash: String,
    expiry: Instant,
}

/// A simple cache that stores up to 10 unique blockhashes that expire after 30 seconds.
#[derive(Default)]
struct BlockhashCache {
    hashes: VecDeque<CachedBlockhash>,
}

impl BlockhashCache {
    /// Adds a new blockhash if it’s not already present.
    fn add(&mut self, blockhash: String) {
        let now = Instant::now();
        let expiry = now + Duration::from_secs(30);
        if !self.hashes.iter().any(|x| x.blockhash == blockhash) {
            self.hashes.push_back(CachedBlockhash { blockhash, expiry });
            // Keep only the 10 most recent entries.
            if self.hashes.len() > 10 {
                self.hashes.pop_front();
            }
        }
    }

    /// Removes expired blockhashes from the cache.
    fn prune(&mut self) {
        let now = Instant::now();
        while let Some(front) = self.hashes.front() {
            if front.expiry <= now {
                self.hashes.pop_front();
            } else {
                break;
            }
        }
    }

    /// Returns all currently cached blockhashes.
    fn get_all(&self) -> Vec<CachedBlockhash> {
        self.hashes.iter().cloned().collect()
    }
}

/// A struct to store and sort RPC latencies.
#[derive(Default)]
struct RpcLatencyTracker {
    latencies: HashMap<String, Duration>,
}

impl RpcLatencyTracker {
    /// Updates the latency for a given RPC URL.
    fn update(&mut self, url: String, latency: Duration) {
        self.latencies.insert(url, latency);
    }

    /// Returns a list of RPC URLs sorted by lowest latency.
    fn get_sorted_rpcs(&self) -> Vec<String> {
        let mut sorted_rpcs: Vec<_> = self.latencies.iter().collect();
        sorted_rpcs.sort_by_key(|&(_, latency)| *latency);
        sorted_rpcs.iter().map(|(url, _)| (*url).clone()).collect()
    }

    /// Logs the current latencies.
    fn log_latencies(&self) {
        println!("RPC Latencies (sorted by fastest):");
        let mut sorted_latencies: Vec<_> = self.latencies.iter().collect();
        sorted_latencies.sort_by_key(|&(_, latency)| *latency);

        for (url, latency) in sorted_latencies {
            println!("{}: {} ms", url, latency.as_millis());
        }
    }
}

#[tokio::main]
async fn main() {
    // Load environment variables from .env
    dotenv().ok();
    env_logger::init();

    // Fetch RPC URLs from the .env file.
    let rpc_urls = env::var("RPC_URLS").expect("RPC_URLS not set in .env");
    let endpoints: Vec<String> = rpc_urls
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    if endpoints.is_empty() {
        eprintln!("No RPC endpoints provided in RPC_URLS.");
        return;
    }

    // Create a vector of (endpoint, RpcClient) pairs.
    let clients: Vec<(String, RpcClient)> = endpoints
        .into_iter()
        .map(|url| {
            let client = RpcClient::new_with_commitment(url.clone(), CommitmentConfig::confirmed());
            (url, client)
        })
        .collect();

    // Shared cache for storing blockhashes.
    let cache = Arc::new(Mutex::new(BlockhashCache::default()));

    // Shared latency tracker.
    let latency_tracker = Arc::new(Mutex::new(RpcLatencyTracker::default()));

    // Create a Tokio interval timer that ticks every 3 seconds.
    let mut ticker = time::interval(Duration::from_secs(3));

    loop {
        ticker.tick().await;

        // Prune expired blockhashes.
        {
            let mut cache_lock = cache.lock().unwrap();
            cache_lock.prune();
        }

        // Fetch latest blockhash from all RPCs asynchronously.
        let latency_tracker_clone = Arc::clone(&latency_tracker);
        let tasks: Vec<_> = clients
            .iter()
            .map(|(url, client)| {
                let url = url.clone();
                let cache = Arc::clone(&cache);
                let latency_tracker_clone = Arc::clone(&latency_tracker_clone);

                async move {
                    let start = Instant::now();
                    match client.get_latest_blockhash().await {
                        Ok(blockhash) => {
                            let blockhash_str = blockhash.to_string();
                            // Update the shared cache with the new blockhash.
                            let mut cache_lock = cache.lock().unwrap();
                            cache_lock.add(blockhash_str);
                        }
                        Err(e) => {
                            eprintln!("Error from {}: {:?}", url, e);
                        }
                    }
                    let latency = Instant::now() - start;

                    // Update latency tracker.
                    let mut tracker = latency_tracker_clone.lock().unwrap();
                    tracker.update(url.clone(), latency);

                    (url, latency)
                }
            })
            .collect();

        // Run all RPC requests concurrently.
        let _ = futures::future::join_all(tasks).await;

        // Log and sort latencies.
        let tracker = latency_tracker.lock().unwrap();
        tracker.log_latencies();
        let sorted_rpcs = tracker.get_sorted_rpcs();

        println!("Sorted RPCs by latency:");
        for url in &sorted_rpcs {
            println!("{}", url);
        }

        // Print cached blockhashes with time remaining until expiry.
        let current_hashes = {
            let cache_lock = cache.lock().unwrap();
            cache_lock.get_all()
        };

        println!("Cached blockhashes (time until expiry in ms):");
        for entry in current_hashes {
            let remaining = entry
                .expiry
                .saturating_duration_since(Instant::now())
                .as_millis();
            println!("{} expires in {} ms", entry.blockhash, remaining);
        }
    }
}
