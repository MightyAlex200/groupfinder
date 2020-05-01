#[macro_use]
extern crate lazy_static;

use rand::random;
use serde::Deserialize;
use serde_json as json;
use std::{sync::{Arc, atomic::{Ordering, AtomicU32}}, time::Duration, collections::BTreeMap};
use tokio::time::delay_for;
use tokio::prelude::*;
use tokio::sync::Semaphore;
use regex::Regex;

#[derive(Deserialize)]
struct FundsResponse {
    robux: u32,
}

type GroupId = u32;

const COOLDOWN_TIME: Duration = Duration::from_secs(60);
const ROBUX_FILE: &str = "robux.txt";
const API_KEY_FILE: &str = "api.key";
const RECONNECT_THRESHOLD: i32 = 5;

lazy_static! {
    static ref ROBUX_SEMAPHORE: Semaphore = Semaphore::new(1);
    static ref ROBUX_REGEX: Regex = Regex::new(r"^Group (\d+) has (\d+) robux.$").unwrap();
    static ref API_KEY: String = std::fs::read_to_string(API_KEY_FILE).unwrap();
}

fn robux_format_str(gid: u32, robux: u32) -> String {
    format!("Group {} has {} robux.", gid, robux)
}

fn is_rate_limited(group_info: &json::Value) -> bool {
    let mes = group_info.pointer("/errors/0/message");
    if let Some(v) = mes {
        v.as_str() == Some("TooManyRequests")
    } else {
        false
    }
}

fn funds_check_address(id: GroupId) -> String {
    format!(
        "https://economy.roblox.com/v1/groups/{}/currency?_={}",
        id,
        &*API_KEY
    )
}

fn owner_check_address(id: GroupId) -> String {
    format!("https://groups.roblox.com/v1/groups/{}?_={}", id, &*API_KEY)
}

fn generate_random_group_id() -> GroupId {
    random::<u32>() % 5_000_000
}

async fn get_proxies_list() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    const PROXIES_LOC: &str = "proxies.json";
    let bytes = tokio::fs::read(PROXIES_LOC).await?;
    Ok(json::from_slice(&bytes)?)
}

async fn rate_limited(proxy_index: usize) {
    println!(
        "Proxy {} is rate limited, waiting {} seconds",
        proxy_index,
        COOLDOWN_TIME.as_secs()
    );
    delay_for(COOLDOWN_TIME).await;
}

async fn write_to_robux_file(gid: u32, robux: u32) -> std::io::Result<()> {
    let _lock = ROBUX_SEMAPHORE.acquire().await;
    let file = tokio::fs::read_to_string(ROBUX_FILE).await;
    let mut vec: Vec<(u32, u32)>;
    match file {
        Ok(file) => {
            let mut map = file
                .lines()
                .filter_map(|s| ROBUX_REGEX.captures(s))
                .filter_map(|c| c.get(1).and_then(|g| c.get(2).map(|r| (g.as_str().parse().unwrap(), r.as_str().parse().unwrap()))))
                .collect::<BTreeMap<u32, u32>>();
            map.insert(gid, robux);
            vec = map.into_iter().collect::<Vec<_>>();
            vec.sort_by_key(|&(_, r)| r);
            vec.reverse();
        }
        Err(_) => {
            vec = vec![(gid, robux)];
        }
    }
    
    let s = vec.into_iter().map(|(g, r)| robux_format_str(g, r)).collect::<Vec<_>>().join("\n");
    tokio::fs::File::create(ROBUX_FILE).await?.write_all(s.as_bytes()).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proxies = get_proxies_list().await?;
    let proxies_len = proxies.len();
    let mut handles = Vec::new();

    lazy_static::initialize(&API_KEY);

    let groups_checked = Arc::new(AtomicU32::new(0));
    let proxies_connected: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));

    let groups_checked_clone = groups_checked.clone();
    let proxies_connected_clone = proxies_connected.clone();
    tokio::spawn(async move {
        loop {
            println!("{} groups checked", groups_checked_clone.load(Ordering::Relaxed));
            let proxies_connected = proxies_connected_clone.load(Ordering::Relaxed);
            println!("{}/{} proxies connected {}%", proxies_connected, proxies_len, proxies_connected as usize * 100 / proxies_len);
            delay_for(Duration::from_secs(30)).await;
        }
    });

    for (i, proxy_url) in proxies.into_iter().enumerate() {
        let groups_checked_clone = groups_checked.clone();
        let proxies_connected_clone = proxies_connected.clone();
        handles.push(tokio::spawn(async move {
            let mut groups_checked = 0;
            loop {
                let mut connect_error = false;
                for connection_attempt in 0..5 {
                    proxies_connected_clone.fetch_add(1, Ordering::Relaxed);
                    let err = async {
                        let client = reqwest::ClientBuilder::new()
                            .proxy(reqwest::Proxy::all(&proxy_url)?)
                            .build()?;
                        // Infinite loop of group scraping
                        loop {
                            let random_group_id = generate_random_group_id();
                            let res = client
                                .get(&funds_check_address(random_group_id))
                                .send()
                                .await?;
                            let funds_value: json::Value = match json::from_str(&res.text().await?) {
                                Ok(f) => f,
                                Err(_) => continue,
                            };
                            if is_rate_limited(&funds_value) {
                                rate_limited(i).await;
                                continue;
                            }
                            let funds: FundsResponse = match json::from_value(funds_value) {
                                Ok(f) => f,
                                Err(_) => continue,
                            };
                            if funds.robux > 0 {
                                let res = client
                                    .get(&owner_check_address(random_group_id))
                                    .send()
                                    .await?;
                                let owner: json::Value = match json::from_str(&res.text().await?) {
                                    Ok(o) => o,
                                    Err(_) => continue,
                                };
                                if is_rate_limited(&owner) {
                                    rate_limited(i).await;
                                    continue;
                                }
                                let not_locked = owner.get("isLocked").is_none();
                                let public = owner["publicEntryAllowed"] == json::Value::Bool(true);
                                let no_owner = owner["owner"].is_null();
                                if not_locked && public && no_owner {
                                    println!("{}", robux_format_str(random_group_id, funds.robux));
                                    if let Err(e) = write_to_robux_file(random_group_id, funds.robux).await {
                                        println!("Error writing file: {}", e);
                                    }
                                }
                            }
                            groups_checked_clone.fetch_add(1, Ordering::Relaxed);
                            groups_checked += 1;
                        }
                        // Type hint
                        #[allow(unreachable_code)]
                        Ok::<(), Box<dyn std::error::Error>>(())
                    }
                    .await
                    .unwrap_err();
                    proxies_connected_clone.fetch_sub(1, Ordering::Relaxed);
                    let hyper_error = err.source().and_then(|s| s.downcast_ref::<hyper::Error>());
                    connect_error = hyper_error.map_or(false, |e| e.is_connect());
                    if !connect_error {
                        println!(
                            "Proxy {} error in connection attempt {}: {:?}",
                            i, connection_attempt, err
                        );
                    }
                }
                if !connect_error {
                    println!("Proxy {} disconnected", i);
                }
                if groups_checked < RECONNECT_THRESHOLD {
                    break
                } else {
                    println!("Proxy {} disconnected, but it has scanned {} groups. Attempting to reconnect", i, groups_checked);
                }
            }
        }));
    }
    for handle in handles {
        handle.await.expect("Failed to unify with joinhandle");
    }
    println!("All proxies disconnected");
    Ok(())
}
