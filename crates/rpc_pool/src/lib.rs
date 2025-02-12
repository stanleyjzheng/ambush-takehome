use std::collections::{HashMap, VecDeque};
use std::env;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use tokio::time;

#[derive(Debug, Clone)]
pub struct CachedBlockhash {
    pub blockhash: String,
    pub expiry: Instant,
}

#[derive(Default, Clone)]
pub struct BlockhashCache {
    hashes: VecDeque<CachedBlockhash>,
}

impl BlockhashCache {
    pub fn add(&mut self, blockhash: String) {
        let now = Instant::now();
        let expiry = now + Duration::from_secs(30);
        if !self.hashes.iter().any(|x| x.blockhash == blockhash) {
            self.hashes.push_back(CachedBlockhash { blockhash, expiry });
            if self.hashes.len() > 10 {
                self.hashes.pop_front();
            }
        }
    }

    pub fn prune(&mut self) {
        let now = Instant::now();
        while let Some(front) = self.hashes.front() {
            if front.expiry <= now {
                self.hashes.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn get_all(&self) -> Vec<CachedBlockhash> {
        self.hashes.iter().cloned().collect()
    }
}

#[derive(Default, Clone)]
pub struct RpcLatencyTracker {
    latencies: HashMap<String, Duration>,
}

impl RpcLatencyTracker {
    pub fn update(&mut self, url: String, latency: Duration) {
        self.latencies.insert(url, latency);
    }

    pub fn get_sorted_rpcs(&self) -> Vec<String> {
        let mut sorted_rpcs: Vec<_> = self.latencies.iter().collect();
        sorted_rpcs.sort_by_key(|&(_, latency)| *latency);
        sorted_rpcs
            .into_iter()
            .map(|(url, _)| url.clone())
            .collect()
    }
}

#[derive(Clone)]
pub struct RpcPool {
    pub cache: Arc<Mutex<BlockhashCache>>,
    pub latency_tracker: Arc<Mutex<RpcLatencyTracker>>,
}

impl RpcPool {
    pub async fn new() -> Self {
        dotenv::dotenv().ok();
        let rpc_urls = env::var("RPC_URLS").expect("RPC_URLS not set");
        let endpoints: Vec<String> = rpc_urls.split(',').map(|s| s.trim().to_string()).collect();

        let clients = endpoints
            .into_iter()
            .map(|url| {
                let client =
                    RpcClient::new_with_commitment(url.clone(), CommitmentConfig::confirmed());
                (url, client)
            })
            .collect();

        let cache = Arc::new(Mutex::new(BlockhashCache::default()));
        let latency_tracker = Arc::new(Mutex::new(RpcLatencyTracker::default()));
        let pool = Self {
            cache: cache.clone(),
            latency_tracker: latency_tracker.clone(),
        };

        // Start background tasks with cloned references instead of moving pool
        Self::start_background_tasks(cache, latency_tracker, clients).await;
        pool
    }

    async fn start_background_tasks(
        cache: Arc<Mutex<BlockhashCache>>,
        latency_tracker: Arc<Mutex<RpcLatencyTracker>>,
        clients: Vec<(String, RpcClient)>,
    ) {
        tokio::spawn(async move {
            // Rest of the implementation remains the same
            let mut ticker = time::interval(Duration::from_secs(3));
            loop {
                ticker.tick().await;
                {
                    let mut cache_lock = cache.lock().unwrap();
                    cache_lock.prune();
                }

                let tasks = clients.iter().map(|(url, client)| {
                    let url = url.clone();
                    let cache = cache.clone();
                    let latency_tracker = latency_tracker.clone();

                    async move {
                        let start = Instant::now();
                        match client.get_latest_blockhash().await {
                            Ok(bh) => {
                                cache.lock().unwrap().add(bh.to_string());
                            }
                            Err(e) => eprintln!("RPC error {}: {}", url, e),
                        }
                        let latency = start.elapsed();
                        latency_tracker.lock().unwrap().update(url.clone(), latency);
                    }
                });

                futures::future::join_all(tasks).await;
            }
        });
    }
}
