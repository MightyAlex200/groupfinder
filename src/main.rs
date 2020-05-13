#[macro_use]
extern crate lazy_static;

mod scraping;
mod ui;

use iced::Application;

pub type GroupId = u32;

fn main() {
    let settings = iced::Settings {
        window: iced::window::Settings {
            size: (1000, 600),
            ..Default::default()
        },
        ..Default::default()
    };
    ui::GroupScraper::run(settings)
}
