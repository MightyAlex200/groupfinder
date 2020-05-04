#[macro_use]
extern crate lazy_static;

use iced::{widget, Application, Command, Element, Length, Subscription};
use iced_futures::BoxStream;
use rand::random;
use regex::Regex;
use serde::Deserialize;
use serde_json as json;
use std::{
    collections::BTreeMap,
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};
use tokio::prelude::*;
use tokio::sync::Semaphore;
use tokio::time::delay_for;

#[derive(Deserialize)]
struct FundsResponse {
    robux: u32,
}

type GroupId = u32;

const COOLDOWN_TIME: Duration = Duration::from_secs(60);
const ROBUX_FILE: &str = "robux.txt";
const API_KEY_FILE: &str = "api.key";
const RECONNECT_THRESHOLD: i32 = 5;
const PROXIES_LOC: &str = "proxies.json";
const PREMIUM_ROBUX_PER_MONTH: u16 = 2200;
const PREMIUM_ROBUX_PER_SECOND: f64 = {
    let seconds_in_minute = 60;
    let seconds_in_hour = seconds_in_minute * 60;
    let seconds_in_day = seconds_in_hour * 24;
    let seconds_in_month = seconds_in_day * 30;
    PREMIUM_ROBUX_PER_MONTH as f64 / seconds_in_month as f64
};

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
        id, &*API_KEY
    )
}

fn owner_check_address(id: GroupId) -> String {
    format!("https://groups.roblox.com/v1/groups/{}?_={}", id, &*API_KEY)
}

fn generate_random_group_id() -> GroupId {
    random::<u32>() % 5_000_000
}

async fn get_proxies_list() -> Result<Vec<String>, std::io::ErrorKind> {
    let bytes = tokio::fs::read(PROXIES_LOC).await.map_err(|e| e.kind())?;
    Ok(json::from_slice(&bytes).map_err(|e| Into::<std::io::Error>::into(e).kind())?)
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
                .filter_map(|c| {
                    c.get(1).and_then(|g| {
                        c.get(2)
                            .map(|r| (g.as_str().parse().unwrap(), r.as_str().parse().unwrap()))
                    })
                })
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

async fn generate_proxies_list() -> Result<Vec<String>, ()> {
    const PROXIES_LIST_URL: &str = "https://api.proxyscrape.com/?request=getproxies&proxytype=socks5&timeout=10000&country=all";
    let list = reqwest::get(PROXIES_LIST_URL)
        .await
        .map_err(drop)?
        .text()
        .await
        .map_err(drop)?
        .split("\r\n")
        .map(|s| format!("socks5://{}", s))
        .collect();
    tokio::fs::write(PROXIES_LOC, json::to_string(&list).unwrap())
        .await
        .ok();
    Ok(list)
}

struct GroupScraper {
    proxies_list: Option<Result<Vec<String>, std::io::ErrorKind>>,
    groups: BTreeMap<(Option<String>, GroupId), u32>,
    running: bool,
    proxies_connected: u32,
    start_time: Instant,
    premium_groups: bool,
    // States
    proxies_scroll_state: widget::scrollable::State,
    new_proxies_button_state: widget::button::State,
    groups_list_state: widget::scrollable::State,
    start_button_state: widget::button::State,
}

#[derive(Debug, Clone)]
enum Msg {
    ProxyListLoaded(Result<Vec<String>, std::io::ErrorKind>),
    GenerateProxies,
    GroupFound {
        group: (Option<String>, u32),
        robux: u32,
    },
    SetRunning,
    ProxyConnected,
    ProxyDisconnected,
    SetPremiumGroups(bool),
}

struct Scraping(Vec<String>);

impl<H, I> iced_futures::subscription::Recipe<H, I> for Scraping
where
    H: Hasher,
{
    type Output = Msg;

    fn hash(&self, state: &mut H) {
        self.0.hash(state);
    }

    fn stream(self: Box<Self>, _input: BoxStream<I>) -> BoxStream<Self::Output> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        for (i, proxy_url) in self.0.into_iter().enumerate() {
            let txc = tx.clone();
            tokio::spawn(async move {
                let mut groups_checked = 0;
                loop {
                    let mut connect_error = false;
                    for connection_attempt in 0..5 {
                        let mut proxy_connected = false;
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
                                if !proxy_connected {
                                    proxy_connected = true;
                                    txc.send(Msg::ProxyConnected).ok();
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
                                if funds.robux > 0 {
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
                                    let no_owner = owner["owner"].is_null();
                                    if not_locked && public && no_owner {
                                        let group_name =
                                            owner["name"].as_str().map(|s| s.to_string());
                                        println!(
                                            "{}",
                                            robux_format_str(random_group_id, funds.robux)
                                        );
                                        txc.send(Msg::GroupFound {
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
                            }
                            // Type hint
                            #[allow(unreachable_code)]
                            Ok::<(), Box<dyn std::error::Error>>(())
                        }
                        .await
                        .unwrap_err();
                        if proxy_connected {
                            txc.send(Msg::ProxyDisconnected).ok();
                        }
                        let hyper_error =
                            err.source().and_then(|s| s.downcast_ref::<hyper::Error>());
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

impl Application for GroupScraper {
    type Executor = iced::executor::Default;
    type Message = Msg;
    type Flags = ();
    fn new((): Self::Flags) -> (Self, Command<Self::Message>) {
        let scraper = Self {
            proxies_list: None,
            groups: BTreeMap::new(),
            running: false,
            proxies_connected: 0,
            start_time: Instant::now(),
            premium_groups: false,
            proxies_scroll_state: Default::default(),
            new_proxies_button_state: Default::default(),
            groups_list_state: Default::default(),
            start_button_state: Default::default(),
        };
        let command = Command::perform(get_proxies_list(), |res| Msg::ProxyListLoaded(res));
        (scraper, command)
    }
    fn title(&self) -> String {
        "Group Scraper".to_string()
    }
    fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
        match message {
            Msg::ProxyListLoaded(res) => {
                self.proxies_list = Some(res);
                Command::none()
            }
            Msg::GenerateProxies => Command::perform(generate_proxies_list(), |proxies| {
                Msg::ProxyListLoaded(proxies.map_err(|_| std::io::ErrorKind::Other))
            }),
            Msg::GroupFound {
                group: (name, gid),
                robux,
            } => {
                self.groups.insert((name, gid), robux);
                Command::none()
            }
            Msg::SetRunning => {
                self.running = true;
                Command::none()
            }
            Msg::ProxyConnected => {
                self.proxies_connected += 1;
                Command::none()
            }
            Msg::ProxyDisconnected => {
                self.proxies_connected -= 1;
                Command::none()
            }
            Msg::SetPremiumGroups(b) => {
                self.premium_groups = b;
                Command::none()
            }
        }
    }
    fn view(&mut self) -> Element<'_, Self::Message> {
        let proxies_widget: Element<_> = match &self.proxies_list {
            None => widget::Text::new("Loading proxies").into(),
            Some(Err(error)) => {
                widget::Text::new(format!("Loading proxies.json failed: {:?}", error)).into()
            }
            Some(Ok(proxies)) => {
                let proxy_list = proxies.into_iter().fold(
                    widget::Scrollable::new(&mut self.proxies_scroll_state),
                    |s, p| s.push(widget::Text::new(p)),
                );
                let proxy_connections = widget::Text::new(format!(
                    "{} proxies connected ({}%)",
                    self.proxies_connected,
                    ((self.proxies_connected as f32 / proxies.len() as f32) * 100.0) as u8
                ));
                widget::Column::new()
                    .push(proxy_connections)
                    .push(proxy_list)
                    .height(iced::Length::Fill)
                    .into()
            }
        };
        let new_proxies_button = widget::Button::new(
            &mut self.new_proxies_button_state,
            widget::Text::new("Generate new proxies list"),
        )
        .on_press(Msg::GenerateProxies);
        let proxies_column = widget::Column::new()
            .push(proxies_widget)
            .push(new_proxies_button)
            .align_items(iced::Align::Center);

        let robux_found: u32 = self.groups.iter().map(|(_, r)| r).sum();
        let time_elapsed = self.start_time.elapsed().as_secs_f32();
        let robux_per_second = robux_found as f32 / time_elapsed as f32;
        let robux_count = widget::Text::new(format!(
            "Total robux found: {}\n{}% better than premium",
            robux_found,
            ((robux_per_second / PREMIUM_ROBUX_PER_SECOND as f32) - 1. * 100.) as i16
        ))
        .horizontal_alignment(iced::HorizontalAlignment::Center);
        let mut groups_list = self.groups.iter().collect::<Vec<_>>();
        groups_list.sort_by_key(|(_, &r)| r);
        groups_list.reverse();
        let groups_list = groups_list
            .into_iter()
            .fold(
                widget::Scrollable::new(&mut self.groups_list_state),
                |list, ((name, _), r)| {
                    list.push(widget::Text::new(format!(
                        "Group \"{}\": {} robux",
                        name.as_ref()
                            .map(|s| &s[..])
                            .unwrap_or("(unknown group name)"),
                        r
                    )))
                },
            )
            .height(iced::Length::Fill);
        let start_button = widget::Button::new(
            &mut self.start_button_state,
            widget::Text::new(if self.running { "Running..." } else { "Start" }),
        )
        .on_press(Msg::SetRunning);
        let premium_checkbox =
            widget::Checkbox::new(self.premium_groups, "Detect premium groups", |checked| {
                Msg::SetPremiumGroups(checked)
            });
        let start_row = widget::Row::new()
            .push(start_button)
            .push(widget::Space::new(Length::Units(16), Length::Units(0)))
            .push(premium_checkbox)
            .align_items(iced::Align::Center);
        let robux_column = widget::Column::new()
            .push(robux_count)
            .push(groups_list)
            .push(start_row)
            .width(iced::Length::Fill)
            .align_items(iced::Align::Center);
        widget::Row::new()
            .push(proxies_column)
            .push(robux_column)
            .into()
    }
    fn subscription(&self) -> Subscription<Self::Message> {
        match (self.running, &self.proxies_list) {
            (true, Some(Ok(list))) => iced::Subscription::from_recipe(Scraping(list.clone())),
            _ => iced::Subscription::none(),
        }
    }
}

fn main() {
    let settings = iced::Settings {
        window: iced::window::Settings {
            resizable: false,
            ..Default::default()
        },
        ..Default::default()
    };
    GroupScraper::run(settings)
}
