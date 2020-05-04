#[macro_use]
extern crate lazy_static;

mod scraping;
mod ui;

use iced::Application;

pub type GroupId = u32;

fn main() {
    let settings = iced::Settings {
        window: iced::window::Settings {
            resizable: false,
            ..Default::default()
        },
        ..Default::default()
    };
    ui::GroupScraper::run(settings)
}
