//! Pin window — borderless top-most window holding a captured bitmap that the
//! user can drag around their desktop. ESC or double-click closes it.

use std::sync::Arc;

use iced::widget::{container, image, mouse_area};
use iced::{window, Color, Element, Length, Task};
use image::{ImageBuffer, Rgba};

use super::message::{Message, PinMessage};

pub struct PinState {
    pub bitmap:   Arc<ImageBuffer<Rgba<u8>, Vec<u8>>>,
    pub handle:   image::Handle,
    pub dragging: bool,
}

impl PinState {
    pub fn new(bitmap: Arc<ImageBuffer<Rgba<u8>, Vec<u8>>>) -> Self {
        let handle = image::Handle::from_rgba(
            bitmap.width(),
            bitmap.height(),
            bitmap.as_raw().clone(),
        );
        PinState { bitmap, handle, dragging: false }
    }
}

pub fn update(state: &mut PinState, id: window::Id, m: PinMessage) -> Task<Message> {
    match m {
        PinMessage::DragStart { .. } => { state.dragging = true; window::drag(id) }
        PinMessage::DragMove  { .. } => Task::none(),
        PinMessage::DragEnd          => { state.dragging = false; Task::none() }
        PinMessage::Close            => window::close::<Message>(id),
        PinMessage::Copy             => {
            let bmp = state.bitmap.clone();
            Task::perform(async move {
                tokio::task::spawn_blocking(move || {
                    kashot_platform::clipboard::copy_image_png(&bmp).map_err(|e| e.to_string())
                }).await.map_err(|e| e.to_string())?
            }, move |_| Message::Pin(id, PinMessage::Close))
        }
        PinMessage::Save => {
            let bmp = state.bitmap.clone();
            Task::perform(async move {
                let path = rfd::AsyncFileDialog::new()
                    .set_file_name("kashot_pinned.png")
                    .add_filter("PNG image", &["png"])
                    .save_file()
                    .await
                    .map(|h| h.path().to_path_buf());
                let Some(path) = path else { return Err::<std::path::PathBuf, String>("cancelled".into()); };
                tokio::task::spawn_blocking(move || -> Result<std::path::PathBuf, String> {
                    bmp.save(&path).map_err(|e| e.to_string())?;
                    Ok(path)
                }).await.map_err(|e| e.to_string())?
            }, move |r| Message::Pin(id, PinMessage::SaveResult(r)))
        }
        PinMessage::SaveResult(_) => Task::none(),
    }
}

pub fn view<'a>(state: &'a PinState, id: window::Id) -> Element<'a, Message> {
    let img = image(state.handle.clone()).width(Length::Fill).height(Length::Fill);
    let area = mouse_area(img)
        .on_press(Message::Pin(id, PinMessage::DragStart {
            p: kashot_core::annotation::Point2::new(0.0, 0.0)
        }))
        .on_release(Message::Pin(id, PinMessage::DragEnd));

    container(area)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| iced::widget::container::Style {
            border: iced::Border {
                color:  Color::from_rgb8(100, 149, 237),
                width:  2.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}
