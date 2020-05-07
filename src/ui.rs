use crate::GroupId;
use iced::{widget, Application, Command, Element, Length, Subscription};
use serde_json as json;
use std::time::Instant;

const PROXIES_LOC: &str = "proxies.json";
const PREMIUM499: Premium = Premium {
    robux_per_month: 450,
    price: "$4.99",
};
const PREMIUM999: Premium = Premium {
    robux_per_month: 1000,
    price: "$9.99",
};
const PREMIUM1999: Premium = Premium {
    robux_per_month: 2200,
    price: "$19.99",
};

struct Premium {
    robux_per_month: u16,
    price: &'static str,
}

impl Premium {
    fn robux_per_second(&self) -> f64 {
        const SECONDS_IN_MONTH: f64 = 60. * 60. * 24. * 30.;
        self.robux_per_month as f64 / SECONDS_IN_MONTH
    }
}

pub async fn get_proxies_list() -> Result<Vec<String>, std::io::ErrorKind> {
    let bytes = tokio::fs::read(PROXIES_LOC).await.map_err(|e| e.kind())?;
    Ok(json::from_slice(&bytes).map_err(|e| Into::<std::io::Error>::into(e).kind())?)
}

pub async fn generate_proxies_list() -> Result<Vec<String>, ()> {
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

#[derive(Debug, Clone)]
pub enum Msg {
    ProxyListLoaded(Result<Vec<String>, std::io::ErrorKind>),
    GenerateProxies,
    GroupFound {
        group: (Option<String>, GroupId),
        robux: u32,
    },
    ToggleRunning,
    ProxyConnected,
    ProxyDisconnected,
    SetPremiumGroups(bool),
    UpdateMinimumRobux(String),
    OpenGroup(GroupId),
    GroupChecked,
}

pub struct GroupInfo {
    name: Option<String>,
    id: GroupId,
    robux: u32,
    state: widget::button::State,
    visited: bool,
}

impl GroupInfo {
    fn view(&mut self) -> Element<Msg> {
        widget::Button::new(
            &mut self.state,
            widget::Text::new(format!(
                "Group \"{}\": {} robux",
                self.name
                    .as_ref()
                    .map(|s| &s[..])
                    .unwrap_or("(unknown group name)"),
                self.robux
            )),
        )
        .style(GroupButtonStyle(self.visited))
        .on_press(Msg::OpenGroup(self.id))
        .into()
    }
}

struct GroupButtonStyle(bool);

impl widget::button::StyleSheet for GroupButtonStyle {
    fn active(&self) -> widget::button::Style {
        widget::button::Style {
            background: None,
            border_radius: 1,
            text_color: if self.0 {
                iced::Color::from_rgb8(108, 19, 162)
            } else {
                iced::Color::from_rgb8(0, 39, 142)
            },
            ..Default::default()
        }
    }
}

pub struct GroupScraper {
    proxies_list: Option<Result<Vec<String>, std::io::ErrorKind>>,
    groups: Vec<GroupInfo>,
    running: bool,
    running_sender: tokio::sync::watch::Sender<bool>,
    running_receiver: tokio::sync::watch::Receiver<bool>,
    proxies_connected: u32,
    start_time: Instant,
    premium_groups: bool,
    premium_groups_sender: tokio::sync::watch::Sender<bool>,
    premium_groups_receiver: tokio::sync::watch::Receiver<bool>,
    minimum_robux: Option<u16>,
    minimum_robux_sender: tokio::sync::watch::Sender<u16>,
    minimum_robux_receiver: tokio::sync::watch::Receiver<u16>,
    groups_checked: u32,
    // States
    proxies_scroll_state: widget::scrollable::State,
    new_proxies_button_state: widget::button::State,
    groups_list_state: widget::scrollable::State,
    start_button_state: widget::button::State,
    minimum_robux_state: widget::text_input::State,
}

impl Application for GroupScraper {
    type Executor = iced::executor::Default;
    type Message = Msg;
    type Flags = ();
    fn new((): Self::Flags) -> (Self, Command<Self::Message>) {
        let (running_send, running_recv) = tokio::sync::watch::channel(false);
        let (premium_send, premium_recv) = tokio::sync::watch::channel(false);
        let (minimum_robux_send, minimum_robux_recv) = tokio::sync::watch::channel(1);
        let scraper = Self {
            proxies_list: None,
            groups: Vec::new(),
            running: false,
            running_receiver: running_recv,
            running_sender: running_send,
            proxies_connected: 0,
            start_time: Instant::now(),
            premium_groups: false,
            premium_groups_receiver: premium_recv,
            premium_groups_sender: premium_send,
            minimum_robux: None,
            minimum_robux_sender: minimum_robux_send,
            minimum_robux_receiver: minimum_robux_recv,
            groups_checked: 0,
            proxies_scroll_state: Default::default(),
            new_proxies_button_state: Default::default(),
            groups_list_state: Default::default(),
            start_button_state: Default::default(),
            minimum_robux_state: Default::default(),
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
                group: (name, id),
                robux,
            } => {
                if !self.groups.iter().any(|gi| gi.id == id) {
                    self.groups.push(GroupInfo {
                        name,
                        id,
                        robux,
                        state: Default::default(),
                        visited: false,
                    });
                    self.groups.sort_by_key(|gi| gi.robux);
                    self.groups.reverse();
                }
                Command::none()
            }
            Msg::ToggleRunning => {
                self.running = !self.running;
                if !self.running {
                    self.running_sender.broadcast(self.running).unwrap();
                }
                let (running_send, running_recv) = tokio::sync::watch::channel(false);
                self.running_sender = running_send;
                self.running_receiver = running_recv;
                self.running_sender.broadcast(self.running).unwrap();
                self.proxies_connected = 0;
                Command::none()
            }
            Msg::ProxyConnected => {
                // Sometimes messages are processed out of order and ProxyConnected are received after all proxies are disconnected
                if self.running {
                    self.proxies_connected += 1;
                }
                Command::none()
            }
            Msg::ProxyDisconnected => {
                if self.running {
                    self.proxies_connected -= 1;
                }
                Command::none()
            }
            Msg::SetPremiumGroups(b) => {
                self.premium_groups = b;
                self.premium_groups_sender.broadcast(b).unwrap();
                Command::none()
            }
            Msg::UpdateMinimumRobux(s) => {
                let new_min: Result<u16, _> = s.parse();
                self.minimum_robux = new_min.ok();
                self.minimum_robux_sender
                    .broadcast(self.minimum_robux.unwrap_or(1))
                    .unwrap();
                Command::none()
            }
            Msg::OpenGroup(gid) => {
                if let Err(err) = opener::open(&format!("https://roblox.com/groups/{}", gid)) {
                    println!("Could not open link: {}", err);
                } else {
                    if let Some(gi) = self.groups.iter_mut().find(|gi| gi.id == gid) {
                        gi.visited = true;
                    }
                }
                Command::none()
            }
            Msg::GroupChecked => {
                self.groups_checked += 1;
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

        let robux_found: u32 = self
            .groups
            .iter()
            .map(|GroupInfo { robux, .. }| robux)
            .sum();
        let time_elapsed = self.start_time.elapsed().as_secs_f32();
        let robux_per_second = robux_found as f64 / time_elapsed as f64;
        let closest_premium = if robux_per_second > PREMIUM1999.robux_per_second() {
            PREMIUM1999
        } else if robux_per_second > PREMIUM999.robux_per_second() {
            PREMIUM999
        } else {
            PREMIUM499
        };
        let best_metric =
            (((robux_per_second / closest_premium.robux_per_second()) - 1.) * 100.) as i16;
        let groups_checked = self.groups_checked;
        let robux_count = widget::Text::new(format!(
            "Total robux found: {}\n{} groups checked\n{}% better than {} premium",
            robux_found, groups_checked, best_metric, closest_premium.price,
        ))
        .horizontal_alignment(iced::HorizontalAlignment::Center);
        let mut groups_list =
            widget::Scrollable::new(&mut self.groups_list_state).height(iced::Length::Fill);
        for gi in self.groups.iter_mut() {
            groups_list = groups_list.push(gi.view());
        }
        let start_button = widget::Button::new(
            &mut self.start_button_state,
            widget::Text::new(if self.running { "Stop" } else { "Start" }),
        )
        .on_press(Msg::ToggleRunning);
        let premium_checkbox =
            widget::Checkbox::new(self.premium_groups, "Detect premium groups", |checked| {
                Msg::SetPremiumGroups(checked)
            });
        let minimum_textbox = widget::TextInput::new(
            &mut self.minimum_robux_state,
            "Minimum robux",
            &self
                .minimum_robux
                .map(|r| r.to_string())
                .unwrap_or("".to_string()),
            Msg::UpdateMinimumRobux,
        );
        let start_row = widget::Row::new()
            .push(minimum_textbox)
            .push(widget::Space::new(Length::Units(16), Length::Units(0)))
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
            (true, Some(Ok(list))) => iced::Subscription::from_recipe(crate::scraping::Scraping {
                proxy_list: list.clone(),
                running: self.running_receiver.clone(),
                premium_groups: self.premium_groups_receiver.clone(),
                minimum_robux: self.minimum_robux_receiver.clone(),
            }),
            _ => iced::Subscription::none(),
        }
    }
}
