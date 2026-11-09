use std::{
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener},
    time::Duration,
};

use eframe::egui;
use serde::Deserialize;
use url::Url;
use wry::{
    WebView, WebViewBuilder,
    dpi::{PhysicalPosition, PhysicalSize},
    http::HeaderMap,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stream {
    Kick(String),
    KickVod(String, String),
    YouTube(String, u32),
    Twitch(String),
    TwitchVod(String),
    TwitchClip(String),
    Rumble(String),
}

impl Stream {
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(hash) = text.strip_prefix('#') {
            return Self::from_hash(hash);
        }
        let url = web_url(text)?;
        let host = url.host_str()?.trim_start_matches("www.");
        let path = url.path().trim_matches('/');
        let mut parts = path.split('/');
        let first = parts.next()?;
        let second = parts.next();
        let third = parts.next();
        let param = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        match host {
            "destiny.gg" | "d.gg" if path == "bigscreen" => Self::from_hash(url.fragment()?),
            "kick.com" if param("clip").is_none() => match (second, third) {
                (None, _)
                    if channel(first)
                        && !matches!(
                            first,
                            "categories" | "search" | "browse" | "following" | "settings" | "clips"
                        ) =>
                {
                    Some(Self::Kick(first.into()))
                }
                (Some("videos"), Some(id))
                    if channel(first) && uuid(id) && parts.next().is_none() =>
                {
                    Some(Self::KickVod(first.into(), id.into()))
                }
                _ => None,
            },
            "youtube.com" | "m.youtube.com" => {
                let id = match (first, second) {
                    ("watch", None) => param("v")?,
                    ("live" | "shorts" | "embed", Some(id)) if third.is_none() => id.into(),
                    _ => return None,
                };
                youtube(&id, param("t").or_else(|| param("start")).as_deref())
            }
            "youtu.be" if second.is_none() => youtube(first, param("t").as_deref()),
            "twitch.tv" => match (second, third) {
                (Some(id), None) if first == "videos" && digits(id) => {
                    Some(Self::TwitchVod(id.into()))
                }
                (Some("clip"), Some(id)) if identifier(id, 100) && parts.next().is_none() => {
                    Some(Self::TwitchClip(id.into()))
                }
                (None, _)
                    if channel(first)
                        && !matches!(
                            first,
                            "directory"
                                | "downloads"
                                | "settings"
                                | "subscriptions"
                                | "videos"
                                | "search"
                        ) =>
                {
                    Some(Self::Twitch(first.into()))
                }
                _ => None,
            },
            "clips.twitch.tv" if second.is_none() && identifier(first, 100) => {
                Some(Self::TwitchClip(first.into()))
            }
            "rumble.com" if first == "embed" && third.is_none() && identifier(second?, 32) => {
                Some(Self::Rumble(second?.into()))
            }
            _ => None,
        }
    }

    fn from_hash(hash: &str) -> Option<Self> {
        let (kind, rest) = hash.split_once('/')?;
        let (id, query) = rest.split_once('?').unwrap_or((rest, ""));
        match kind {
            "kick" if channel(id) => Some(Self::Kick(id.into())),
            "kick-vod" => {
                let (name, vod) = id.split_once('/')?;
                (channel(name) && uuid(vod)).then(|| Self::KickVod(name.into(), vod.into()))
            }
            "youtube" => {
                let time =
                    url::form_urlencoded::parse(query.as_bytes()).find(|(key, _)| key == "t");
                youtube(id, time.as_ref().map(|(_, value)| value.as_ref()))
            }
            "twitch" if channel(id) => Some(Self::Twitch(id.into())),
            "twitch-vod" if digits(id) => Some(Self::TwitchVod(id.into())),
            "twitch-clip" if identifier(id, 100) => Some(Self::TwitchClip(id.into())),
            "rumble" if identifier(id, 32) => Some(Self::Rumble(id.into())),
            _ => None,
        }
    }

    pub fn channel_tab(&self) -> Option<Self> {
        match self {
            Self::Kick(id) => Some(Self::Kick(id.to_ascii_lowercase())),
            Self::Twitch(id) => Some(Self::Twitch(id.to_ascii_lowercase())),
            Self::YouTube(id, _) => Some(Self::YouTube(id.clone(), 0)),
            Self::Rumble(_) => Some(self.clone()),
            _ => None,
        }
    }

    pub fn same_channel(&self, other: &Self) -> bool {
        self == other
            || match (self, other) {
                (Self::Kick(a), Self::Kick(b)) | (Self::Twitch(a), Self::Twitch(b)) => {
                    a.eq_ignore_ascii_case(b)
                }
                (Self::YouTube(a, _), Self::YouTube(b, _)) => a == b,
                _ => false,
            }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Kick(_) | Self::KickVod(..) => "Kick",
            Self::YouTube(..) => "YouTube",
            Self::Twitch(_) | Self::TwitchVod(_) | Self::TwitchClip(_) => "Twitch",
            Self::Rumble(_) => "Rumble",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Kick(id)
            | Self::KickVod(_, id)
            | Self::YouTube(id, _)
            | Self::Twitch(id)
            | Self::TwitchVod(id)
            | Self::TwitchClip(id)
            | Self::Rumble(id) => id,
        }
    }

    pub fn browser_url(&self) -> String {
        match self {
            Self::Kick(id) => format!("https://kick.com/{id}"),
            Self::KickVod(name, id) => format!("https://kick.com/{name}/videos/{id}"),
            Self::YouTube(id, time) => format!("https://www.youtube.com/watch?v={id}&t={time}s"),
            Self::Twitch(id) => format!("https://www.twitch.tv/{id}"),
            Self::TwitchVod(id) => format!("https://www.twitch.tv/videos/{id}"),
            Self::TwitchClip(id) => format!("https://clips.twitch.tv/{id}"),
            Self::Rumble(id) => format!("https://rumble.com/embed/{id}/"),
        }
    }

    fn embed(&self) -> String {
        match self {
            Self::Kick(id) => format!("https://player.kick.com/{id}?autoplay=true"),
            Self::KickVod(name, id) => format!("{}/api/kick/vod/{name}/{id}", crate::chat::WEBSITE),
            Self::YouTube(id, time) => {
                format!("https://www.youtube.com/embed/{id}?autoplay=1&playsinline=1&start={time}")
            }
            Self::Twitch(id) => {
                format!("https://player.twitch.tv/?channel={id}&parent=localhost&muted=true")
            }
            Self::TwitchVod(id) => {
                format!("https://player.twitch.tv/?video=v{id}&parent=localhost&muted=true")
            }
            Self::TwitchClip(id) => format!(
                "https://clips.twitch.tv/embed?clip={id}&parent=localhost&autoplay=true&muted=true"
            ),
            Self::Rumble(id) => format!("https://rumble.com/embed/{id}/?pub=4"),
        }
    }
}

fn identifier(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn channel(s: &str) -> bool {
    identifier(s, 64)
}

fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

fn youtube(id: &str, time: Option<&str>) -> Option<Stream> {
    (id.len() == 11 && identifier(id, 11))
        .then(|| Stream::YouTube(id.into(), time.and_then(seconds).unwrap_or(0)))
}

fn seconds(input: &str) -> Option<u32> {
    if let Ok(n) = input.parse() {
        return Some(n);
    }
    let mut value = 0u32;
    let mut total = 0u32;
    for c in input.chars() {
        if let Some(digit) = c.to_digit(10) {
            value = value.checked_mul(10)?.checked_add(digit)?;
            continue;
        }
        let unit = match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
        total = total.checked_add(value.checked_mul(unit)?)?;
        value = 0;
    }
    total.checked_add(value)
}

pub fn web_url(text: &str) -> Option<Url> {
    let url = Url::parse(text).ok().or_else(|| {
        let host = text.split('/').next()?;
        (host.contains('.')
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')))
        .then(|| Url::parse(&format!("https://{text}")).ok())
        .flatten()
    })?;
    (matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

#[derive(Clone, Debug)]
pub struct Live {
    pub stream: Stream,
    pub title: String,
    pub viewers: u64,
}

#[derive(Deserialize)]
struct StatusResponse {
    data: Status,
}

#[derive(Deserialize)]
struct Status {
    streams: std::collections::HashMap<String, Option<StatusStream>>,
}

#[derive(Deserialize)]
struct StatusStream {
    live: bool,
    id: String,
    status_text: Option<String>,
    viewers: Option<u64>,
}

pub async fn live_streams(client: &reqwest::Client) -> Result<Vec<Live>, reqwest::Error> {
    let data: StatusResponse = client
        .get(format!("{}/api/info/stream", crate::chat::WEBSITE))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(["kick", "youtube", "twitch", "rumble"]
        .iter()
        .filter_map(|platform| {
            let entry = data
                .data
                .streams
                .get(*platform)?
                .as_ref()
                .filter(|entry| entry.live)?;
            Some(Live {
                stream: Stream::from_hash(&format!("{platform}/{}", entry.id))?,
                title: entry.status_text.clone().unwrap_or_default(),
                viewers: entry.viewers.unwrap_or(0),
            })
        })
        .collect())
}

#[derive(Default)]
pub struct Player {
    view: Option<WebView>,
    current: Option<Stream>,
    bounds: Option<wry::Rect>,
    visible: bool,
    error: Option<String>,
}

impl Player {
    pub fn sync(
        &mut self,
        frame: &eframe::Frame,
        ui_rect: egui::Rect,
        scale: f32,
        target: Option<&Stream>,
    ) {
        let Some(target) = target else {
            self.reset();
            return;
        };
        let bounds = bounds(ui_rect, scale);
        if self.current.as_ref() != Some(target) {
            self.reset();
            self.current = Some(target.clone());
            let result = (|| {
                let mut headers = HeaderMap::new();
                if matches!(target, Stream::YouTube(..)) {
                    headers.insert(
                        "Referer",
                        "https://gg.destiny.desktop/"
                            .parse()
                            .expect("valid app identifier"),
                    );
                }
                let embed = target.embed();
                let url = if target.name() == "Twitch" {
                    twitch_document(&embed)?
                } else {
                    embed
                };
                let view = WebViewBuilder::new()
                    .with_bounds(bounds)
                    .with_autoplay(true)
                    .with_url_and_headers(url, headers)
                    .with_new_window_req_handler(|url, _| {
                        if let Some(url) = web_url(&url) {
                            let _ = webbrowser::open(url.as_str());
                        }
                        wry::NewWindowResponse::Deny
                    })
                    .build_as_child(frame)
                    .map_err(|e| e.to_string())?;
                self.bounds = Some(bounds);
                self.visible = true;
                Ok::<_, String>(view)
            })();
            match result {
                Ok(view) => self.view = Some(view),
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(view) = &self.view {
            let show = ui_rect.width() >= 1.0 && ui_rect.height() >= 1.0;
            if self.visible != show && view.set_visible(show).is_ok() {
                self.visible = show;
            }
            if self.bounds != Some(bounds) && show && view.set_bounds(bounds).is_ok() {
                self.bounds = Some(bounds);
            }
        }
    }

    pub fn reset(&mut self) {
        if let Some(view) = &self.view {
            // Wry retains the underlying WKWebView after detaching it from the window.
            let _ = view.load_url("about:blank");
        }
        *self = Self::default();
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

pub fn bounds(rect: egui::Rect, scale: f32) -> wry::Rect {
    wry::Rect {
        position: PhysicalPosition::new(
            (rect.min.x * scale).round() as i32,
            (rect.min.y * scale).round() as i32,
        )
        .into(),
        size: PhysicalSize::new(
            (rect.width() * scale).round() as u32,
            (rect.height() * scale).round() as u32,
        )
        .into(),
    }
}

fn twitch_document(embed: &str) -> Result<String, String> {
    // Twitch requires a parent origin. Its localhost development exception lets
    // a loopback page supply one without publishing an external player website.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let body = format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="referrer" content="strict-origin-when-cross-origin"><style>html,body,iframe{{margin:0;width:100%;height:100%;border:0;background:#090b10;overflow:hidden}}</style><iframe src="{}" allow="autoplay; fullscreen" allowfullscreen></iframe>"#,
        embed.replace('&', "&amp;")
    );
    std::thread::spawn(move || {
        let until = std::time::Instant::now() + Duration::from_secs(15);
        while std::time::Instant::now() < until {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    let _ = conn.set_read_timeout(Some(Duration::from_millis(500)));
                    let _ = conn.set_write_timeout(Some(Duration::from_millis(500)));
                    let mut request = [0; 2048];
                    if let Ok(size) = conn.read(&mut request)
                        && request[..size].starts_with(b"GET / HTTP/")
                    {
                        let _ = write!(
                            conn,
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25))
                }
                Err(_) => break,
            }
        }
    });
    Ok(format!("http://localhost:{port}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_links_resolve_to_typed_channels() {
        assert_eq!(
            Stream::parse("#kick/destiny"),
            Some(Stream::Kick("destiny".into()))
        );
        assert_eq!(
            Stream::parse("https://www.destiny.gg/bigscreen#youtube/mHLKCNmOmm8?t=1m5s"),
            Some(Stream::YouTube("mHLKCNmOmm8".into(), 65))
        );
        assert_eq!(
            Stream::parse("twitch.tv/videos/123"),
            Some(Stream::TwitchVod("123".into()))
        );
        assert_eq!(
            Stream::parse("https://rumble.com/embed/v1234/"),
            Some(Stream::Rumble("v1234".into()))
        );
        assert!(Stream::parse("https://kick.com.evil.example/destiny").is_none());
        assert!(Stream::parse("https://kick.com/destiny?clip=abc").is_none());
        assert!(Stream::parse("javascript:alert(1)").is_none());
    }
}
