use eframe::egui;
use wry::{WebView, WebViewBuilder};

use crate::{account::Session, chat::WEBSITE, player};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Drawer {
    Profile,
    Tts,
    Vestaboard,
}

impl Drawer {
    pub fn title(self) -> &'static str {
        match self {
            Self::Profile => "Profile",
            Self::Tts => "TTS Queue",
            Self::Vestaboard => "Vestaboard",
        }
    }

    pub fn url(self) -> Option<String> {
        let path = match self {
            Self::Profile => return None,
            Self::Tts => "tts",
            Self::Vestaboard => "vestaboard",
        };
        Some(format!("{WEBSITE}/{path}"))
    }
}

#[derive(Default)]
pub struct SiteView {
    view: Option<WebView>,
    bounds: Option<wry::Rect>,
    error: Option<String>,
}

impl SiteView {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        url: &str,
        session: Option<&Session>,
    ) {
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        let bounds = player::bounds(rect, ui.ctx().pixels_per_point());
        if self.view.is_none() && self.error.is_none() {
            match build(frame, url, bounds, session) {
                Ok(view) => {
                    self.bounds = Some(bounds);
                    self.view = Some(view);
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        if let Some(view) = &self.view
            && self.bounds != Some(bounds)
            && view.set_bounds(bounds).is_ok()
        {
            self.bounds = Some(bounds);
        }
        if let Some(error) = &self.error {
            ui.painter().text(
                rect.left_top(),
                egui::Align2::LEFT_TOP,
                format!("Could not open this page.\n{error}"),
                egui::FontId::proportional(13.0),
                ui.visuals().error_fg_color,
            );
        }
    }

    pub fn close(&mut self) {
        if let Some(view) = self.view.take() {
            let _ = view.load_url("about:blank");
        }
        *self = Self::default();
    }
}

fn build(
    frame: &eframe::Frame,
    url: &str,
    bounds: wry::Rect,
    session: Option<&Session>,
) -> wry::Result<WebView> {
    let view = WebViewBuilder::new()
        .with_incognito(true)
        .with_bounds(bounds)
        .with_navigation_handler(|url| {
            url == "about:blank"
                || player::web_url(&url).is_some_and(|url| {
                    url.scheme() == "https"
                        && matches!(url.host_str(), Some("www.destiny.gg" | "destiny.gg"))
                })
        })
        .with_new_window_req_handler(|url, _| {
            if let Some(url) = player::web_url(&url) {
                let _ = webbrowser::open(url.as_str());
            }
            wry::NewWindowResponse::Deny
        })
        .build_as_child(frame)?;

    if let Some(session) = session {
        for (name, value) in session.cookies() {
            let cookie = wry::cookie::Cookie::build((name, value))
                .domain("www.destiny.gg")
                .path("/")
                .secure(true)
                .http_only(true)
                .build();
            view.set_cookie(&cookie)?;
        }
    }
    view.load_url(url)?;
    Ok(view)
}
