use std::{collections::HashMap, time::Duration};

use chrono::{DateTime, Utc};
use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::JoinHandle, time};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::header};

pub const WEBSITE: &str = "https://www.destiny.gg";
pub const SOCKET: &str = "wss://chat.destiny.gg/ws";
use crate::account::Session;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct User {
    pub nick: String,
    pub features: Vec<String>,
    pub roles: Vec<String>,
    pub subscription: Option<Subscription>,
    pub patreon_member: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Subscription {
    pub tier: u8,
}

impl User {
    pub fn tier(&self) -> u8 {
        self.subscription.as_ref().map_or(0, |sub| sub.tier)
    }

    pub fn privileged(&self) -> bool {
        self.roles.iter().any(|role| {
            matches!(
                role.to_ascii_lowercase().as_str(),
                "admin" | "moderator" | "protected"
            )
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Post {
    #[serde(flatten)]
    pub user: User,
    #[serde(rename = "data")]
    pub text: String,
    #[serde(default)]
    pub timestamp: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Poll {
    pub question: String,
    pub nick: String,
    pub options: Vec<String>,
    #[serde(deserialize_with = "nullable_totals")]
    pub totals: Vec<u64>,
    pub totalvotes: u64,
    pub canvote: bool,
    #[serde(deserialize_with = "poll_vote")]
    pub myvote: usize,
    pub weighted: bool,
    pub hidden: bool,
    pub start: Option<DateTime<Utc>>,
    pub now: Option<DateTime<Utc>>,
    pub time: u64,
    #[serde(skip)]
    pub deadline: Option<std::time::Instant>,
    #[serde(skip)]
    pub ended: bool,
    #[serde(skip)]
    pub pending: Option<std::time::Instant>,
}

fn nullable_totals<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u64>, D::Error> {
    Ok(Option::<Vec<u64>>::deserialize(deserializer)?.unwrap_or_default())
}

fn poll_vote<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
        .or_else(|| value.as_str()?.parse().ok())
        .unwrap_or(0))
}

impl Poll {
    pub fn start(mut self) -> Self {
        let elapsed = self.start.zip(self.now).map_or(0, |(start, now)| {
            (now - start).num_milliseconds().max(0) as u64
        });
        self.deadline = Some(
            std::time::Instant::now()
                + Duration::from_millis(self.time.saturating_sub(elapsed).min(3_600_000)),
        );
        self.totals.resize(self.options.len(), 0);
        self
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.map_or(Duration::ZERO, |deadline| {
            deadline.saturating_duration_since(std::time::Instant::now())
        })
    }

    pub fn cast(&mut self, option: usize, quantity: u64) {
        if !self.ended {
            self.totalvotes = self.totalvotes.saturating_add(quantity);
            if !self.hidden
                && let Some(total) = option.checked_sub(1).and_then(|i| self.totals.get_mut(i))
            {
                *total = total.saturating_add(quantity);
            }
        }
    }

    pub fn finish(&mut self, data: &Value) {
        if let Ok(totals) = serde_json::from_value::<Vec<u64>>(data["totals"].clone())
            && totals.len() == self.options.len()
        {
            self.totalvotes = data["totalvotes"]
                .as_u64()
                .unwrap_or_else(|| totals.iter().sum());
            self.totals = totals;
            self.hidden = false;
        }
        self.ended = true;
        self.pending = None;
    }
}

#[derive(Debug)]
pub enum Event {
    Connecting,
    Disconnected(String),
    Frame(String, Value),
}

#[derive(Debug)]
pub enum Command {
    Say(String),
    Whisper { nick: String, text: String },
    Vote(usize),
}

impl Command {
    fn wire(self) -> String {
        match self {
            Self::Say(text) => format!("MSG {}", json!({ "data": text })),
            Self::Whisper { nick, text } => {
                format!("PRIVMSG {}", json!({ "nick": nick, "data": text }))
            }
            Self::Vote(vote) => format!("CASTVOTE {}", json!({ "vote": vote.to_string() })),
        }
    }
}

pub fn parse_frame(frame: &str) -> Option<(&str, Value)> {
    let (kind, payload) = frame.split_once(' ')?;
    Some((
        kind,
        serde_json::from_str(payload).unwrap_or_else(|_| Value::String(payload.to_owned())),
    ))
}

pub fn users(value: &Value) -> impl Iterator<Item = User> + '_ {
    value.as_array().into_iter().flatten().filter_map(|value| {
        serde_json::from_value::<User>(value.clone())
            .ok()
            .filter(|user| !user.nick.is_empty())
    })
}

pub fn update_users(users_by_nick: &mut HashMap<String, User>, data: &Value) {
    for user in users(&data["users"]) {
        users_by_nick.insert(user.nick.to_ascii_lowercase(), user);
    }
}

pub struct Connection {
    pub commands: mpsc::Sender<Command>,
    pub events: mpsc::Receiver<Event>,
    task: JoinHandle<()>,
}

impl Connection {
    pub fn spawn(
        runtime: &tokio::runtime::Runtime,
        ctx: &egui::Context,
        session: Option<Session>,
    ) -> Self {
        let (commands, rx) = mpsc::channel(32);
        let (tx, events) = mpsc::channel(256);
        let task = runtime.spawn(run(ctx.clone(), session, tx, rx));
        Self {
            commands,
            events,
            task,
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn publish(tx: &mpsc::Sender<Event>, ctx: &egui::Context, event: Event) {
    if tx.send(event).await.is_ok() {
        ctx.request_repaint();
    }
}

async fn run(
    ctx: egui::Context,
    session: Option<Session>,
    tx: mpsc::Sender<Event>,
    mut commands: mpsc::Receiver<Command>,
) {
    let mut attempt = 0u32;
    loop {
        while commands.try_recv().is_ok() {}
        publish(&tx, &ctx, Event::Connecting).await;
        let connected_at = time::Instant::now();
        let result = connect(session.as_ref(), &tx, &ctx, &mut commands).await;
        if connected_at.elapsed() >= Duration::from_secs(10) {
            attempt = 0;
        }
        let jitter = u64::from(Utc::now().timestamp_subsec_millis());
        let delay = 2u64.pow(attempt.min(5)) + 1;
        let reason = result.err().unwrap_or_else(|| "Chat disconnected".into());
        publish(
            &tx,
            &ctx,
            Event::Disconnected(format!("{reason}. Retrying in {delay}s.")),
        )
        .await;
        time::sleep(Duration::from_millis(delay * 1_000 + jitter)).await;
        attempt = attempt.saturating_add(1);
    }
}

async fn connect(
    session: Option<&Session>,
    tx: &mpsc::Sender<Event>,
    ctx: &egui::Context,
    commands: &mut mpsc::Receiver<Command>,
) -> Result<(), String> {
    let mut request = SOCKET.into_client_request().map_err(|e| e.to_string())?;
    request
        .headers_mut()
        .insert(header::ORIGIN, header::HeaderValue::from_static(WEBSITE));
    if let Some(value) = session.and_then(Session::cookie_header) {
        let mut cookie = header::HeaderValue::from_str(&value)
            .map_err(|_| "Invalid Destiny.gg session".to_string())?;
        cookie.set_sensitive(true);
        request.headers_mut().insert(header::COOKIE, cookie);
    }
    let (mut socket, _) = time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| "Connection timed out".to_string())?
    .map_err(|error| format!("Chat connection failed ({error})"))?;
    let idle = time::sleep(Duration::from_secs(90));
    tokio::pin!(idle);
    loop {
        tokio::select! {
            _ = &mut idle => return Err("No response from chat".into()),
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()) };
                time::timeout(Duration::from_secs(8), socket.send(Message::text(command.wire())))
                    .await.map_err(|_| "Send timed out".to_string())?
                    .map_err(|_| "Chat send failed".to_string())?;
            }
            frame = socket.next() => {
                idle.as_mut().reset(time::Instant::now() + Duration::from_secs(90));
                let Some(frame) = frame else { return Ok(()) };
                match frame.map_err(|_| "Chat connection lost".to_string())? {
                    Message::Text(text) if text.starts_with("PING ") => {
                        let pong = format!("PONG {}", &text[5..]);
                        socket.send(Message::text(pong)).await.map_err(|_| "Heartbeat failed".to_string())?;
                    }
                    Message::Text(text) => {
                        if let Some((kind, data)) = parse_frame(&text) {
                            publish(tx, ctx, Event::Frame(kind.to_owned(), data)).await;
                        }
                    }
                    Message::Ping(_) => socket.flush().await.map_err(|_| "Heartbeat failed".to_string())?,
                    Message::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_and_vote_frames_match_dgg() {
        let (kind, data) = parse_frame(r#"MSG {"nick":"viewer","features":["subscriber"],"subscription":{"tier":2},"timestamp":1794251440002,"data":"Nice stream"}"#).unwrap();
        assert_eq!(kind, "MSG");
        let post = serde_json::from_value::<Post>(data).unwrap();
        assert_eq!(post.user.nick, "viewer");
        assert_eq!(post.user.tier(), 2);
        assert_eq!(post.text, "Nice stream");
        assert_eq!(Command::Vote(2).wire(), r#"CASTVOTE {"vote":"2"}"#);
    }

    #[test]
    fn hidden_weighted_poll_reveals_only_server_totals() {
        let mut poll = serde_json::from_value::<Poll>(json!({
            "question": "Which stream?", "options": ["A", "B"],
            "canvote": true, "hidden": true, "weighted": true, "time": 30_000,
            "myvote": -1, "totals": null,
            "start": "2026-11-09T19:00:00Z", "now": "2026-11-09T19:00:20Z"
        }))
        .unwrap()
        .start();
        assert!(poll.remaining() <= Duration::from_secs(10));
        assert!(poll.remaining() > Duration::from_secs(9));
        poll.cast(2, 3);
        assert!(poll.hidden);
        assert_eq!(poll.myvote, 0);
        assert_eq!(poll.totals, [0, 0]);
        assert_eq!(poll.totalvotes, 3);
        poll.finish(&json!({ "totals": [1, 3], "totalvotes": 4 }));
        assert!(poll.ended);
        assert!(!poll.hidden);
        assert_eq!(poll.totals, [1, 3]);
    }

    #[tokio::test]
    #[ignore = "connects read-only to live Destiny.gg chat"]
    async fn live_guest_receives_history_and_presence() {
        let (tx, mut events) = mpsc::channel(256);
        let (_commands, mut rx) = mpsc::channel(8);
        let task =
            tokio::spawn(
                async move { connect(None, &tx, &egui::Context::default(), &mut rx).await },
            );
        time::timeout(Duration::from_secs(20), async {
            let (mut history, mut presence) = (false, false);
            while let Some(event) = events.recv().await {
                if let Event::Frame(kind, data) = event {
                    match kind.as_str() {
                        "HISTORY" => {
                            history = data.as_array().is_some_and(|entries| !entries.is_empty())
                        }
                        "NAMES" => presence = data["connectioncount"].as_u64().is_some(),
                        _ => {}
                    }
                    if history && presence {
                        return;
                    }
                }
            }
            panic!("Guest connection ended before receiving history and presence");
        })
        .await
        .unwrap();
        task.abort();
    }
}
