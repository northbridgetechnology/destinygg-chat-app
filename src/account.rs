use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use eframe::egui;
use serde::{Deserialize, Serialize};
use wry::{PageLoadEvent, WebView, WebViewBuilder};

const KEYCHAIN_SERVICE: &str = "gg.destiny.desktop";
const KEYCHAIN_ACCOUNT: &str = "Destiny.gg website session";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    sid: Option<String>,
    rememberme: Option<String>,
}

impl Session {
    pub fn cookies(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("sid", self.sid.as_deref()),
            ("rememberme", self.rememberme.as_deref()),
        ]
        .into_iter()
        .filter_map(|(name, value)| {
            value
                .filter(|value| valid_cookie(value))
                .map(|value| (name, value))
        })
    }

    pub fn cookie_header(&self) -> Option<String> {
        let cookies: Vec<_> = self
            .cookies()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        (!cookies.is_empty()).then(|| cookies.join("; "))
    }

    pub fn can_remember(&self) -> bool {
        self.rememberme.is_some()
    }
}

fn valid_cookie(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && value
            .bytes()
            .all(|b| matches!(b, b'!' | b'#'..=b'+' | b'-'..=b':' | b'<'..=b'[' | b']'..=b'~'))
}

pub fn saved_session() -> Result<Option<Session>, String> {
    credentials::load()?
        .map(|bytes| {
            serde_json::from_slice::<Session>(&bytes)
                .map(|session| session.cookie_header().is_some().then_some(session))
                .map_err(|_| "The saved Destiny.gg session could not be read.".to_owned())
        })
        .transpose()
        .map(Option::flatten)
}

pub fn save_session(session: &Session) -> Result<(), String> {
    let bytes =
        serde_json::to_vec(session).map_err(|_| "Could not store the session.".to_owned())?;
    credentials::save(&bytes)
}

pub fn delete_session() -> Result<(), String> {
    credentials::delete()
}

#[cfg(target_os = "macos")]
mod credentials {
    use super::{KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE};
    use security_framework::passwords;

    pub(super) fn load() -> Result<Option<Vec<u8>>, String> {
        match passwords::get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.code() == -25300 => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(super) fn save(bytes: &[u8]) -> Result<(), String> {
        passwords::set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, bytes)
            .map_err(|error| error.to_string())
    }

    pub(super) fn delete() -> Result<(), String> {
        match passwords::delete_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == -25300 => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
mod credentials {
    use super::{KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE};
    use keyring::{Entry, Error};

    fn entry() -> Result<Entry, String> {
        Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT).map_err(|error| error.to_string())
    }

    pub(super) fn load() -> Result<Option<Vec<u8>>, String> {
        match entry()?.get_secret() {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    pub(super) fn save(bytes: &[u8]) -> Result<(), String> {
        entry()?
            .set_secret(bytes)
            .map_err(|error| error.to_string())
    }

    pub(super) fn delete() -> Result<(), String> {
        match entry()?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

#[derive(Default)]
struct Page {
    host: String,
    loaded: bool,
}

#[derive(Default)]
pub struct SignIn {
    pub open: bool,
    view: Option<WebView>,
    bounds: Option<wry::Rect>,
    page: Arc<Mutex<Page>>,
    last_check: Option<Instant>,
    tried: Option<Session>,
}

impl SignIn {
    pub fn begin(&mut self) {
        if !self.open {
            *self = Self {
                open: true,
                ..Default::default()
            };
        }
    }

    pub fn close(&mut self) {
        *self = Self::default();
    }

    pub fn host(&self) -> String {
        self.page.lock().unwrap().host.clone()
    }

    pub fn sync(
        &mut self,
        frame: &eframe::Frame,
        rect: egui::Rect,
        ctx: &egui::Context,
    ) -> Result<Option<Session>, String> {
        let bounds = crate::player::bounds(rect, ctx.pixels_per_point());
        if self.view.is_none() {
            let page = Arc::clone(&self.page);
            let ctx = ctx.clone();
            let view = WebViewBuilder::new()
                .with_incognito(true)
                .with_bounds(bounds)
                .with_url(format!("{}/login", crate::chat::WEBSITE))
                .with_navigation_handler(|url| {
                    crate::player::web_url(&url).is_some_and(|url| url.scheme() == "https")
                })
                .with_on_page_load_handler(move |event, url| {
                    let host = url::Url::parse(&url)
                        .ok()
                        .and_then(|url| url.host_str().map(str::to_owned))
                        .unwrap_or_default();
                    *page.lock().unwrap() = Page {
                        host,
                        loaded: matches!(event, PageLoadEvent::Finished),
                    };
                    ctx.request_repaint();
                })
                .build_as_child(frame)
                .map_err(|e| e.to_string())?;
            self.view = Some(view);
            self.bounds = Some(bounds);
        }
        let view = self.view.as_ref().expect("view created above");
        if self.bounds != Some(bounds) {
            view.set_bounds(bounds).map_err(|e| e.to_string())?;
            self.bounds = Some(bounds);
        }
        let ready = {
            let page = self.page.lock().unwrap();
            page.loaded && matches!(page.host.as_str(), "www.destiny.gg" | "destiny.gg")
        };
        if !ready {
            return Ok(None);
        }
        let interval = Duration::from_secs(1);
        ctx.request_repaint_after(interval);
        if self.last_check.is_some_and(|at| at.elapsed() < interval) {
            return Ok(None);
        }
        self.last_check = Some(Instant::now());
        let mut session = Session {
            sid: None,
            rememberme: None,
        };
        for cookie in view.cookies().map_err(|e| e.to_string())? {
            let domain = cookie.domain().unwrap_or_default().trim_start_matches('.');
            if matches!(domain, "www.destiny.gg" | "destiny.gg" | "chat.destiny.gg")
                && cookie.path() == Some("/")
                && valid_cookie(cookie.value())
            {
                match cookie.name() {
                    "sid" => session.sid = Some(cookie.value().to_owned()),
                    "rememberme" => session.rememberme = Some(cookie.value().to_owned()),
                    _ => {}
                }
            }
        }
        if session.cookie_header().is_some() && self.tried.as_ref() != Some(&session) {
            self.tried = Some(session.clone());
            return Ok(Some(session));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_valid_dgg_session_values_enter_the_cookie_header() {
        let session = Session {
            sid: Some("test=value".into()),
            rememberme: Some("line\r\nbreak".into()),
        };
        assert_eq!(session.cookie_header().as_deref(), Some("sid=test=value"));
        assert!(!valid_cookie("value; other=cookie"));
    }
}
