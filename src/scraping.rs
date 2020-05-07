use crate::{ui, GroupId};
use futures_core::stream::BoxStream;
use rand::random;
use regex::Regex;
use serde::Deserialize;
use serde_json as json;
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    time::Duration,
};
use tokio::prelude::*;
use tokio::{sync::Semaphore, time::delay_for};

lazy_static! {
    static ref ROBUX_SEMAPHORE: Semaphore = Semaphore::new(1);
    static ref ROBUX_REGEX: Regex = Regex::new(r"^Group (\d+) has (\d+) robux.$").unwrap();
    static ref API_KEY: String = std::fs::read_to_string(API_KEY_FILE).unwrap();
}

const COOLDOWN_TIME: Duration = Duration::from_secs(60);
const ROBUX_FILE: &str = "robux.txt";
const API_KEY_FILE: &str = "api.key";
const RECONNECT_THRESHOLD: i32 = 5;
const MAX_GROUP_ID: GroupId = 5_000_000;

#[derive(Deserialize)]
struct FundsResponse {
    robux: u32,
}

fn generate_random_group_id() -> GroupId {
    random::<GroupId>() % MAX_GROUP_ID
}

fn funds_check_address(id: GroupId) -> String {
    format!(
        "https://economy.roblox.com/v1/groups/{}/currency?_={}",
        id, &*API_KEY
    )
}

fn is_rate_limited(group_info: &json::Value) -> bool {
    let mes = group_info.pointer("/errors/0/message");
    if let Some(v) = mes {
        v.as_str() == Some("TooManyRequests")
    } else {
        false
    }
}

async fn rate_limited(proxy_index: usize) {
    println!(
        "Proxy {} is rate limited, waiting {} seconds",
        proxy_index,
        COOLDOWN_TIME.as_secs()
    );
    delay_for(COOLDOWN_TIME).await;
}

fn owner_check_address(id: GroupId) -> String {
    format!("https://groups.roblox.com/v1/groups/{}?_={}", id, &*API_KEY)
}

fn robux_format_str(gid: GroupId, robux: u32) -> String {
    format!("Group {} has {} robux.", gid, robux)
}

async fn write_to_robux_file(gid: GroupId, robux: u32) -> std::io::Result<()> {
    let _lock = ROBUX_SEMAPHORE.acquire().await;
    let file = tokio::fs::read_to_string(ROBUX_FILE).await;
    let mut vec: Vec<(GroupId, u32)>;
    match file {
        Ok(file) => {
            let mut map = file
                .lines()
                .filter_map(|s| ROBUX_REGEX.captures(s))
                .filter_map(|c| {
                    c.get(1).and_then(|g| {
                        c.get(2)
                            .map(|r| (g.as_str().parse().unwrap(), r.as_str().parse().unwrap()))
                    })
                })
                .collect::<BTreeMap<GroupId, u32>>();
            map.insert(gid, robux);
            vec = map.into_iter().collect::<Vec<_>>();
            vec.sort_by_key(|&(_, r)| r);
            vec.reverse();
        }
        Err(_) => {
            vec = vec![(gid, robux)];
        }
    }

    let s = vec
        .into_iter()
        .map(|(g, r)| robux_format_str(g, r))
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::File::create(ROBUX_FILE)
        .await?
        .write_all(s.as_bytes())
        .await?;
    Ok(())
}

fn get_from_watch<T: Clone>(recv: &tokio::sync::watch::Receiver<T>) -> T {
    recv.borrow().clone()
}

fn disconnecting(proxy_number: usize) {
    println!("Disconnecting from proxy {}", proxy_number);
}

pub struct Scraping {
    pub proxy_list: Vec<String>,
    pub running: tokio::sync::watch::Receiver<bool>,
    pub premium_groups: tokio::sync::watch::Receiver<bool>,
    pub minimum_robux: tokio::sync::watch::Receiver<u16>,
}

impl<H, I> iced_futures::subscription::Recipe<H, I> for Scraping
where
    H: Hasher,
{
    type Output = ui::Msg;

    fn hash(&self, state: &mut H) {
        self.proxy_list.hash(state);
        get_from_watch(&self.premium_groups).hash(state);
    }

    fn stream(self: Box<Self>, _input: BoxStream<I>) -> BoxStream<Self::Output> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        for (i, proxy_url) in self.proxy_list.into_iter().enumerate() {
            let txc = tx.clone();
            let running = self.running.clone();
            let premium_groups = self.premium_groups.clone();
            let minimum_robux = self.minimum_robux.clone();
            tokio::spawn(async move {
                let mut groups_checked = 0;
                loop {
                    let r = get_from_watch(&running);
                    if !r {
                        disconnecting(i);
                        break;
                    }
                    let mut connect_error = false;
                    let mut break_main = false; // Label breaks do not work in async ¯\_(ツ)_/¯
                    for connection_attempt in 0..5 {
                        let mut proxy_connected = false;
                        let res = async {
                            let client = reqwest::ClientBuilder::new()
                                .proxy(reqwest::Proxy::all(&proxy_url)?)
                                .build()?;
                            // Infinite loop of group scraping
                            loop {
                                let r = get_from_watch(&running);
                                if !r {
                                    break_main = true;
                                    break;
                                }
                                let random_group_id = generate_random_group_id();
                                let res = client
                                    .get(&funds_check_address(random_group_id))
                                    .send()
                                    .await?;
                                if !proxy_connected {
                                    proxy_connected = true;
                                    txc.send(ui::Msg::ProxyConnected).ok();
                                }
                                let funds_value: json::Value =
                                    match json::from_str(&res.text().await?) {
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
                                let minimum_robux = get_from_watch(&minimum_robux);
                                if funds.robux >= minimum_robux as u32 {
                                    let res = client
                                        .get(&owner_check_address(random_group_id))
                                        .send()
                                        .await?;
                                    let owner: json::Value =
                                        match json::from_str(&res.text().await?) {
                                            Ok(o) => o,
                                            Err(_) => continue,
                                        };
                                    if is_rate_limited(&owner) {
                                        rate_limited(i).await;
                                        continue;
                                    }
                                    let not_locked = owner.get("isLocked").is_none();
                                    let public =
                                        owner["publicEntryAllowed"] == json::Value::Bool(true);
                                    let premium =
                                        owner["isBuildersClubOnly"] == json::Value::Bool(true);
                                    let accepting_premium_groups = get_from_watch(&premium_groups);
                                    let no_owner = owner["owner"].is_null();
                                    if not_locked
                                        && public
                                        && no_owner
                                        && (!premium || accepting_premium_groups)
                                    {
                                        let group_name =
                                            owner["name"].as_str().map(|s| s.to_string());
                                        println!(
                                            "{}",
                                            robux_format_str(random_group_id, funds.robux)
                                        );
                                        txc.send(ui::Msg::GroupFound {
                                            group: (group_name, random_group_id),
                                            robux: funds.robux,
                                        })
                                        .ok();
                                        if let Err(e) =
                                            write_to_robux_file(random_group_id, funds.robux).await
                                        {
                                            println!("Error writing file: {}", e);
                                        }
                                    }
                                }
                                groups_checked += 1;
                                txc.send(ui::Msg::GroupChecked).ok();
                            }
                            Ok::<(), Box<dyn std::error::Error>>(())
                        }
                        .await;
                        if break_main {
                            break;
                        }
                        if proxy_connected {
                            txc.send(ui::Msg::ProxyDisconnected).ok();
                        }
                        if let Err(err) = res {
                            let hyper_error =
                                err.source().and_then(|s| s.downcast_ref::<hyper::Error>());
                            connect_error = hyper_error.map_or(false, |e| e.is_connect());
                            if !connect_error {
                                println!(
                                    "Proxy {} error in connection attempt {}: {:?}",
                                    i, connection_attempt, err
                                );
                            }
                        } else {
                            break;
                        }
                    }
                    if break_main {
                        disconnecting(i);
                        break;
                    }
                    if !connect_error {
                        println!("Proxy {} disconnected", i);
                    }
                    if groups_checked < RECONNECT_THRESHOLD {
                        break;
                    } else {
                        println!("Proxy {} disconnected, but it has scanned {} groups. Attempting to reconnect", i, groups_checked);
                    }
                }
            });
        }

        Box::pin(rx)
    }
}
