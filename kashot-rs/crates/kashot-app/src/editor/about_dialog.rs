//! About window — version, attribution, and a link to the GitHub repo.

use iced::widget::{button, column, container, text};
use iced::{Alignment, Color, Element, Length};

use super::message::Message;

pub fn view<'a>() -> Element<'a, Message> {
    let title = text("Kashot").size(36);
    let version = text(format!("Version {}", env!("CARGO_PKG_VERSION")))
        .size(14)
        .color(Color::from_rgb8(110, 110, 120));
    let love = text("With love from PrateekSingh ❤")
        .size(13)
        .color(Color::from_rgb8(120, 118, 240));
    let copyright = text(format!("© {} PrateekSingh. All rights reserved.", year()))
        .size(11)
        .color(Color::from_rgb8(110, 110, 120));
    let link = text("github.com/singhpratech/kashot")
        .size(13)
        .color(Color::from_rgb8(120, 118, 240));

    let body = column![title, version, love, copyright, link]
        .spacing(10)
        .padding(40)
        .align_x(Alignment::Center);

    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

fn year() -> i32 {
    chrono::Local::now().format("%Y").to_string().parse::<i32>().unwrap_or(2026)
}
