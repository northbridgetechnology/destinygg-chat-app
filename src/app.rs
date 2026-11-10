use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::LazyLock,
    time::{Duration, Instant},
};

use chrono::{DateTime, Local};
use eframe::egui::{self, Color32, RichText};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    account::{self, Session, SignIn},
    assets::{Catalog, Images},
    channels::RecentChannels,
    chat::{self, Command, Connection, Event, Poll, Post, User},
    drawer::{Drawer, SiteView},
    player::{self, Live, Player, Stream},
};

const BG: Color32 = Color32::from_rgb(14, 16, 22);
const PANEL: Color32 = Color32::from_rgb(21, 24, 33);
const CARD: Color32 = Color32::from_rgb(28, 32, 44);
const EDGE: Color32 = Color32::from_rgb(43, 49, 66);
const TEXT: Color32 = Color32::from_rgb(227, 231, 244);
const MUTED: Color32 = Color32::from_rgb(154, 163, 188);
const ACCENT: Color32 = Color32::from_rgb(175, 160, 255);
const GREEN: Color32 = Color32::from_rgb(110, 230, 175);
const RED: Color32 = Color32::from_rgb(255, 145, 151);
const MAX_LINES: usize = 280;
const MIN_CHAT_WIDTH: f32 = 330.0;
const MIN_PLAYER_WIDTH: f32 = 450.0;

#[derive(Deserialize, Serialize)]
#[serde(default)]
struct Preferences {
    favorites: HashSet<String>,
    ignored: HashSet<String>,
    timestamps: bool,
    animate: bool,
    detached: bool,
    video_hidden: bool,
    chat_width: f32,
    emote_drawer_height: f32,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            favorites: HashSet::new(),
            ignored: HashSet::new(),
            timestamps: true,
            animate: true,
            detached: false,
            video_hidden: false,
            chat_width: 410.0,
            emote_drawer_height: 220.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Chat,
    Event,
    Whisper,
}

#[derive(Clone)]
struct Line {
    post: Post,
    kind: LineKind,
}

#[derive(Default)]
struct Transcript {
    paused: Option<PausedTranscript>,
    jump_to_bottom: bool,
}

struct PausedTranscript {
    lines: Vec<Line>,
    offset: f32,
}

enum Update {
    Catalog(String, Result<Catalog, String>),
    Streams(Result<Vec<Live>, String>),
}

pub struct Desktop {
    runtime: tokio::runtime::Runtime,
    http: reqwest::Client,
    updates: mpsc::Receiver<Update>,
    publish: mpsc::Sender<Update>,
    connection: Connection,
    has_session: bool,
    session_saved: bool,
    session: Option<Session>,
    sign_in: SignIn,
    session_pending: Option<Session>,
    online: bool,
    status: String,
    me: User,
    users: HashMap<String, User>,
    connections: u64,
    lines: VecDeque<Line>,
    seen: HashSet<(i64, String, String)>,
    whispers: VecDeque<Line>,
    pending_whisper: Option<(Instant, String, String)>,
    last_whisper: Option<String>,
    unread: usize,
    show_whispers: bool,
    chat_scroll: Transcript,
    whisper_scroll: Transcript,
    pin: Option<Post>,
    poll: Option<Poll>,
    draft: String,
    focus_input: bool,
    alert: Option<String>,
    catalog: Catalog,
    catalog_version: Option<String>,
    catalog_loading: Option<String>,
    catalog_error: Option<String>,
    catalog_retry: Instant,
    images: Images,
    show_picker: bool,
    picker_search: String,
    only_favorites: bool,
    prefs: Preferences,
    drawer: Option<Drawer>,
    drawer_site: SiteView,
    live: Option<Vec<Live>>,
    stream_error: Option<String>,
    stream_pending: bool,
    stream_refresh: Instant,
    tuned: Option<Stream>,
    recent_channels: RecentChannels,
    show_channel_overflow: bool,
    channel_input: String,
    player: Player,
    player_expanded: bool,
    divider_offset: Option<f32>,
    emote_resize_offset: Option<f32>,
}

impl Desktop {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        theme(&cc.egui_ctx);
        let prefs = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, "preferences"))
            .unwrap_or_default();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .enable_all()
            .build()?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .user_agent(concat!("DGGDesktop/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let (publish, updates) = mpsc::channel(16);
        let (session, alert) = match account::saved_session() {
            Ok(session) => (session, None),
            Err(error) => (None, Some(format!("Credential store: {error}"))),
        };
        let has_session = session.is_some();
        let connection = Connection::spawn(&runtime, &cc.egui_ctx, session.clone());
        Ok(Self {
            runtime,
            http: http.clone(),
            updates,
            publish,
            connection,
            has_session,
            session_saved: has_session,
            session,
            sign_in: SignIn::default(),
            session_pending: None,
            online: false,
            status: "Connecting…".into(),
            me: User::default(),
            users: HashMap::new(),
            connections: 0,
            lines: VecDeque::new(),
            seen: HashSet::new(),
            whispers: VecDeque::new(),
            pending_whisper: None,
            last_whisper: None,
            unread: 0,
            show_whispers: false,
            chat_scroll: Transcript::default(),
            whisper_scroll: Transcript::default(),
            pin: None,
            poll: None,
            draft: String::new(),
            focus_input: false,
            alert,
            catalog: Catalog::default(),
            catalog_version: None,
            catalog_loading: None,
            catalog_error: None,
            catalog_retry: Instant::now(),
            images: Images::new(http),
            show_picker: false,
            picker_search: String::new(),
            only_favorites: false,
            prefs,
            drawer: None,
            drawer_site: SiteView::default(),
            live: None,
            stream_error: None,
            stream_pending: false,
            stream_refresh: Instant::now(),
            tuned: None,
            recent_channels: RecentChannels::default(),
            show_channel_overflow: false,
            channel_input: String::new(),
            player: Player::default(),
            player_expanded: false,
            divider_offset: None,
            emote_resize_offset: None,
        })
    }

    fn reconnect(&mut self, ctx: &egui::Context, session: Option<Session>) {
        self.has_session = session.is_some();
        self.session = session.clone();
        self.drawer_site.close();
        self.online = false;
        self.status = "Connecting…".into();
        self.me = User::default();
        self.poll = None;
        self.pin = None;
        self.whispers.clear();
        self.whisper_scroll = Transcript::default();
        self.last_whisper = None;
        self.unread = 0;
        self.pending_whisper = None;
        self.connection = Connection::spawn(&self.runtime, ctx, session);
    }

    fn queue_catalog(&mut self, ctx: &egui::Context, version: &str) {
        if self.catalog_version.as_deref() == Some(version)
            || self.catalog_loading.as_deref() == Some(version)
        {
            return;
        }
        self.catalog_loading = Some(version.to_owned());
        let (version, ctx, http, publish) = (
            version.to_owned(),
            ctx.clone(),
            self.http.clone(),
            self.publish.clone(),
        );
        self.runtime.spawn(async move {
            let result = Catalog::fetch(&http, &version).await;
            if publish.send(Update::Catalog(version, result)).await.is_ok() {
                ctx.request_repaint();
            }
        });
    }

    fn queue_streams(&mut self, ctx: &egui::Context) {
        self.stream_pending = true;
        let (ctx, http, publish) = (ctx.clone(), self.http.clone(), self.publish.clone());
        self.runtime.spawn(async move {
            let result = player::live_streams(&http)
                .await
                .map_err(|error| error.to_string());
            if publish.send(Update::Streams(result)).await.is_ok() {
                ctx.request_repaint();
            }
        });
    }

    fn collect(&mut self, ctx: &egui::Context) {
        for _ in 0..180 {
            let Ok(event) = self.connection.events.try_recv() else {
                break;
            };
            match event {
                Event::Connecting => self.status = "Connecting…".into(),
                Event::Disconnected(reason) => {
                    self.online = false;
                    self.connections = 0;
                    self.pin = None;
                    self.poll = None;
                    self.pending_whisper = None;
                    self.status = reason;
                }
                Event::Frame(kind, data) => self.dispatch(ctx, &kind, data, false),
            }
        }
        while let Ok(event) = self.updates.try_recv() {
            match event {
                Update::Catalog(version, result)
                    if self.catalog_loading.as_deref() == Some(&version) =>
                {
                    self.catalog_loading = None;
                    match result {
                        Ok(catalog) => {
                            self.catalog = catalog;
                            self.catalog_version = Some(version);
                            self.catalog_error = None;
                        }
                        Err(error) => {
                            self.catalog_error = Some(error);
                            self.catalog_retry = Instant::now() + Duration::from_secs(25);
                        }
                    }
                }
                Update::Catalog(..) => {}
                Update::Streams(result) => {
                    self.stream_pending = false;
                    match result {
                        Ok(streams) => {
                            self.live = Some(streams);
                            self.stream_error = None;
                            self.stream_refresh = Instant::now() + Duration::from_secs(45);
                        }
                        Err(error) => {
                            self.stream_error = Some(error);
                            self.stream_refresh = Instant::now() + Duration::from_secs(15);
                        }
                    }
                }
            }
        }
        if self.catalog_version.is_none()
            && self.catalog_loading.is_none()
            && Instant::now() >= self.catalog_retry
        {
            self.queue_catalog(ctx, "");
        }
        if !self.prefs.video_hidden && !self.stream_pending {
            let remaining = self
                .stream_refresh
                .saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.queue_streams(ctx);
            } else {
                ctx.request_repaint_after(remaining);
            }
        }
        if self.catalog_version.is_none() && self.catalog_loading.is_none() {
            ctx.request_repaint_after(self.catalog_retry.saturating_duration_since(Instant::now()));
        }
        if self
            .pending_whisper
            .as_ref()
            .is_some_and(|(at, _, _)| at.elapsed() > Duration::from_secs(8))
        {
            self.pending_whisper = None;
            self.alert = Some("Whisper delivery was not confirmed.".into());
        }
        if let Some(remaining) = self
            .recent_channels
            .expire(chrono::Utc::now().timestamp_millis())
        {
            ctx.request_repaint_after(remaining);
        }
        self.images.collect(ctx);
    }

    fn dispatch(&mut self, ctx: &egui::Context, kind: &str, data: Value, backlog: bool) {
        chat::update_users(&mut self.users, &data);
        match kind {
            "ME" => {
                self.me = serde_json::from_value(data).unwrap_or_default();
                if let Some(session) = self.session_pending.take() {
                    if self.me.nick.is_empty() {
                        self.alert = Some(
                            "DGG has not confirmed sign-in. Finish signing in on the page below."
                                .into(),
                        );
                    } else {
                        let storage = if session.can_remember() {
                            account::save_session(&session)
                        } else if self.session_saved {
                            account::delete_session()
                        } else {
                            Ok(())
                        };
                        self.alert = storage
                            .as_ref()
                            .err()
                            .map(|error| format!("Credential store: {error}"));
                        if storage.is_ok() {
                            self.session_saved = session.can_remember();
                        }
                        self.sign_in.close();
                        self.set_drawer(None);
                    }
                } else if self.has_session && self.me.nick.is_empty() && !self.sign_in.open {
                    self.alert = Some(
                        "Your Destiny.gg session has expired. Connect your account again.".into(),
                    );
                }
            }
            "NAMES" => {
                self.online = true;
                self.status = "Chat connected".into();
                self.connections = data["connectioncount"].as_u64().unwrap_or_default();
                self.users = chat::users(&data["users"])
                    .map(|user| (user.nick.to_ascii_lowercase(), user))
                    .collect();
            }
            "USERSDELTA" => {
                for user in chat::users(&data["removed"]) {
                    self.users.remove(&user.nick.to_ascii_lowercase());
                }
            }
            "QUIT" => {
                if let Some(nick) = data["nick"].as_str() {
                    self.users.remove(&nick.to_ascii_lowercase());
                }
            }
            "HISTORY" if !backlog => {
                if let Some(entries) = data.as_array() {
                    for entry in entries.iter().filter_map(Value::as_str) {
                        if let Some((kind, value)) = chat::parse_frame(entry)
                            && matches!(
                                kind,
                                "MSG"
                                    | "BROADCAST"
                                    | "SUBSCRIPTION"
                                    | "GIFTSUB"
                                    | "MASSGIFT"
                                    | "DONATION"
                            )
                        {
                            self.dispatch(ctx, kind, value, true);
                        }
                    }
                }
            }
            "MSG" => {
                if let Ok(mut post) = serde_json::from_value::<Post>(data) {
                    if let Some(user) = self.users.get(&post.user.nick.to_ascii_lowercase())
                        && post.user.features.is_empty()
                        && post.user.subscription.is_none()
                    {
                        post.user = user.clone();
                    }
                    if post.text.contains('#')
                        && !self
                            .prefs
                            .ignored
                            .contains(&post.user.nick.to_ascii_lowercase())
                    {
                        let now = chrono::Utc::now().timestamp_millis();
                        let posted_at = if post.timestamp == 0 && !backlog {
                            now
                        } else {
                            post.timestamp
                        };
                        self.recent_channels.observe(&post.text, posted_at, now);
                    }
                    self.append(post, LineKind::Chat);
                }
            }
            "PIN" if !backlog => {
                self.pin = serde_json::from_value::<Post>(data)
                    .ok()
                    .filter(|post| !post.text.is_empty());
            }
            "POLLSTART" if !backlog => {
                self.poll = serde_json::from_value::<Poll>(data)
                    .ok()
                    .filter(|poll| !poll.options.is_empty())
                    .map(Poll::start);
            }
            "POLLSTOP" if !backlog => {
                if let Some(poll) = &mut self.poll {
                    poll.finish(&data);
                }
            }
            "VOTECAST" if !backlog => {
                if let Some(poll) = &mut self.poll {
                    poll.cast(
                        number(&data["vote"]),
                        data["quantity"].as_u64().unwrap_or(1),
                    );
                }
            }
            "VOTECOUNTED" if !backlog => {
                if let Some(poll) = &mut self.poll {
                    poll.myvote = number(&data["vote"]);
                    poll.pending = None;
                    poll.canvote = false;
                }
            }
            "PRIVMSG" if !backlog => {
                if let Ok(post) = serde_json::from_value::<Post>(data) {
                    self.last_whisper = Some(post.user.nick.clone());
                    self.unread += usize::from(!self.show_whispers);
                    self.append(post, LineKind::Whisper);
                }
            }
            "PRIVMSGSENT" if !backlog => {
                if let Some((_, nick, text)) = self.pending_whisper.take() {
                    self.append(
                        Post {
                            user: self.me.clone(),
                            text: format!("To {nick} · {text}"),
                            timestamp: chrono::Utc::now().timestamp_millis(),
                        },
                        LineKind::Whisper,
                    );
                }
            }
            "REFRESHCHATASSETS" if !backlog => {
                if let Some(version) = data["cacheKey"].as_str() {
                    self.queue_catalog(ctx, version);
                }
            }
            "ERR" if !backlog => {
                let reason = data["description"]
                    .as_str()
                    .or_else(|| data.as_str())
                    .unwrap_or("DGG rejected the request");
                self.alert = Some(match reason {
                    "needlogin" => "Connect your Destiny.gg account to participate.".into(),
                    "throttled" => {
                        "DGG is rate-limiting messages. Wait a moment and try again.".into()
                    }
                    _ => format!("DGG · {reason}"),
                });
                if let Some(poll) = &mut self.poll {
                    poll.pending = None;
                }
                self.pending_whisper = None;
            }
            "PAIDEVENTS" if !backlog => {
                for event in data
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if let Some((kind, value)) = chat::parse_frame(event)
                        && let Some(post) = event_post(kind, &value)
                    {
                        self.append(post, LineKind::Event);
                    }
                }
            }
            "BROADCAST" | "SUBSCRIPTION" | "GIFTSUB" | "MASSGIFT" | "DONATION" | "MUTE"
            | "UNMUTE" | "BAN" | "UNBAN" | "SUBONLY" => {
                if let Some(post) = event_post(kind, &data) {
                    self.append(post, LineKind::Event);
                }
            }
            _ => {}
        }
    }

    fn append(&mut self, post: Post, kind: LineKind) {
        if kind == LineKind::Whisper {
            self.whispers.push_back(Line { post, kind });
            if self.whispers.len() > MAX_LINES {
                self.whispers.pop_front();
            }
            return;
        }
        let key = (post.timestamp, post.user.nick.clone(), post.text.clone());
        if !self.seen.insert(key) {
            return;
        }
        self.lines.push_back(Line { post, kind });
        if self.lines.len() > MAX_LINES
            && let Some(line) = self.lines.pop_front()
        {
            self.seen
                .remove(&(line.post.timestamp, line.post.user.nick, line.post.text));
        }
    }

    fn send(&mut self, command: Command) -> bool {
        if self.me.nick.is_empty() || !self.online {
            self.alert = Some(
                if self.online {
                    "Sign in to participate."
                } else {
                    "Chat is reconnecting."
                }
                .into(),
            );
            return false;
        }
        if self.connection.commands.try_send(command).is_err() {
            self.alert = Some("Chat is busy. Try again in a moment.".into());
            return false;
        }
        self.alert = None;
        true
    }

    fn vote(&mut self, option: usize) -> bool {
        if !self.poll.as_ref().is_some_and(|poll| {
            poll.canvote
                && !poll.ended
                && !poll.remaining().is_zero()
                && poll.pending.is_none()
                && (1..=poll.options.len()).contains(&option)
        }) {
            self.alert = Some("This vote is unavailable or has already been cast.".into());
            return false;
        }
        if self.send(Command::Vote(option))
            && let Some(poll) = &mut self.poll
        {
            poll.pending = Some(Instant::now());
            return true;
        }
        false
    }

    fn submit(&mut self) {
        let text = self.draft.trim().to_owned();
        if text.is_empty() {
            return;
        }
        if text.chars().count() > 512 {
            self.alert = Some("Messages are limited to 512 characters.".into());
            return;
        }
        let (command, args) = text.split_once(' ').unwrap_or((&text, ""));
        let sent = match command.to_ascii_lowercase().as_str() {
            "/w" | "/msg" | "/whisper" => {
                let Some((nick, body)) = args.trim().split_once(' ') else {
                    self.alert = Some("Use /w nickname message.".into());
                    return;
                };
                self.whisper(nick, body.trim())
            }
            "/r" | "/reply" => {
                let Some(nick) = self.last_whisper.clone() else {
                    self.alert = Some("There is no recent whisper to reply to.".into());
                    return;
                };
                self.whisper(&nick, args.trim())
            }
            "/embed" | "/e" => {
                if let Some(stream) = Stream::parse(args.trim()) {
                    self.tune(stream);
                    true
                } else {
                    self.alert = Some("Paste a supported stream link after /embed.".into());
                    false
                }
            }
            _ => {
                let is_option = text.parse::<usize>().ok().filter(|i| {
                    self.poll.as_ref().is_some_and(|p| {
                        !p.ended && !p.remaining().is_zero() && (1..=p.options.len()).contains(i)
                    })
                });
                if let Some(option) = is_option {
                    self.vote(option)
                } else {
                    self.send(Command::Say(text))
                }
            }
        };
        if sent {
            self.draft.clear();
            self.focus_input = true;
        }
    }

    fn whisper(&mut self, nick: &str, text: &str) -> bool {
        if nick.is_empty() || text.is_empty() || self.pending_whisper.is_some() {
            self.alert = Some(
                "Enter a nickname and message, or wait for the previous whisper to be confirmed."
                    .into(),
            );
            return false;
        }
        if self.send(Command::Whisper {
            nick: nick.into(),
            text: text.into(),
        }) {
            self.pending_whisper = Some((Instant::now(), nick.into(), text.into()));
            self.last_whisper = Some(nick.into());
            return true;
        }
        false
    }

    fn tune(&mut self, stream: Stream) {
        self.tuned = Some(stream);
        self.alert = None;
    }

    fn current_stream(&self) -> Option<&Stream> {
        self.tuned
            .as_ref()
            .or_else(|| self.live.as_ref()?.first().map(|live| &live.stream))
    }
}

impl eframe::App for Desktop {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let handle = self.runtime.handle().clone();
        let _runtime = handle.enter();
        #[cfg(target_os = "linux")]
        {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            if self.sign_in.open || self.drawer.is_some() || !self.prefs.video_hidden {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
        self.collect(ctx);
        self.header(ctx);
        let video_hidden = self.prefs.video_hidden;
        if video_hidden {
            self.player.reset();
        }
        self.left_drawer(ctx, frame);
        if self.prefs.detached {
            self.popout(ctx);
        } else if !video_hidden && !self.player_expanded {
            self.docked_chat(ctx);
        }
        let background = if video_hidden && !self.prefs.detached {
            PANEL
        } else {
            BG
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(background).inner_margin(16))
            .show(ctx, |ui| {
                if !video_hidden {
                    self.watch(ui, frame);
                } else if self.prefs.detached {
                    ui.label(quiet("Chat is open in a separate window."));
                } else {
                    self.chat_panel(ui, false);
                }
            });
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "preferences", &self.prefs);
    }
}

fn visible_tabs(widths: &[f32], gap: f32, overflow_width: f32, available: f32) -> usize {
    let total = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
    if total <= available {
        return widths.len();
    }
    let mut used = overflow_width;
    widths
        .iter()
        .take_while(|width| {
            used += **width + gap;
            used <= available
        })
        .count()
}

fn number(value: &Value) -> usize {
    value
        .as_u64()
        .map(|n| n as usize)
        .or_else(|| value.as_str()?.parse().ok())
        .unwrap_or(0)
}

fn event_post(kind: &str, data: &Value) -> Option<Post> {
    let user: User =
        serde_json::from_value(data.get("user").unwrap_or(data).clone()).unwrap_or_default();
    let nick = data["nick"]
        .as_str()
        .or(data["username"].as_str())
        .or(data["name"].as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| (!user.nick.is_empty()).then_some(user.nick.as_str()))
        .unwrap_or("Someone");
    let message = data["data"]
        .as_str()
        .or(data["message"].as_str())
        .unwrap_or("");
    let text = match kind {
        "BROADCAST" => message.to_owned(),
        "SUBSCRIPTION" => format!("{nick} subscribed. {message}"),
        "GIFTSUB" => format!("{nick} gifted a subscription. {message}"),
        "MASSGIFT" => format!("{nick} gifted subscriptions. {message}"),
        "DONATION" => format!("{nick} donated. {message}"),
        "MUTE" => format!("{message} was muted."),
        "UNMUTE" => format!("{message} was unmuted."),
        "BAN" => format!("{message} was banned."),
        "UNBAN" => format!("{message} was unbanned."),
        "SUBONLY" => format!("Subscriber-only mode · {message}"),
        _ => return None,
    };
    Some(Post {
        user,
        text,
        timestamp: data["timestamp"].as_i64().unwrap_or(0),
    })
}

fn theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.window_fill = PANEL;
    style.visuals.panel_fill = BG;
    style.visuals.faint_bg_color = CARD;
    style.visuals.extreme_bg_color = BG;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.hyperlink_color = ACCENT;
    style.visuals.selection.bg_fill = Color32::from_rgb(72, 58, 128);
    style.visuals.widgets.inactive.bg_fill = CARD;
    style.visuals.widgets.inactive.weak_bg_fill = CARD;
    style.visuals.widgets.hovered.bg_fill = EDGE;
    style.visuals.widgets.hovered.weak_bg_fill = EDGE;
    style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, EDGE);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.item_spacing = egui::vec2(7.0, 7.0);
    ctx.set_style(style);
}

fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(8)
        .inner_margin(10)
        .stroke(egui::Stroke::new(1.0_f32, EDGE))
}

fn quiet(text: impl Into<String>) -> RichText {
    RichText::new(text).small().color(MUTED)
}

fn badge(text: impl Into<String>, color: Color32) -> RichText {
    RichText::new(text).small().strong().color(color)
}

fn open(url: &str) {
    let _ = webbrowser::open(url);
}

impl Desktop {
    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("app_header")
            .frame(
                egui::Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(17, 11)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(66, 52, 112))
                        .corner_radius(7)
                        .inner_margin(egui::Margin::symmetric(9, 8))
                        .show(ui, |ui| {
                            ui.label(badge("DGG", Color32::WHITE));
                        });
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.label(RichText::new("DGG Desktop").size(18.0).strong());
                        ui.label(quiet(if self.prefs.video_hidden {
                            "CHAT ONLY"
                        } else {
                            "BIGSCREEN"
                        }));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.me.nick.is_empty()
                            && !self.sign_in.open
                            && ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Connect DGG account").strong(),
                                    )
                                    .fill(Color32::from_rgb(66, 52, 112)),
                                )
                                .clicked()
                        {
                            self.alert = None;
                            self.set_drawer(Some(Drawer::Profile));
                            self.sign_in.begin();
                        }
                        let profile_label = if self.me.nick.is_empty() {
                            "Profile"
                        } else {
                            &self.me.nick
                        };
                        if ui
                            .selectable_label(self.drawer == Some(Drawer::Profile), profile_label)
                            .clicked()
                        {
                            self.toggle_drawer(Drawer::Profile, ui.ctx());
                        }
                        for (drawer, label) in
                            [(Drawer::Vestaboard, "Vestaboard"), (Drawer::Tts, "TTS")]
                        {
                            if ui
                                .selectable_label(self.drawer == Some(drawer), label)
                                .clicked()
                            {
                                self.toggle_drawer(drawer, ui.ctx());
                            }
                        }
                        if ui
                            .button(if self.prefs.detached {
                                "Dock chat"
                            } else {
                                "Pop out chat"
                            })
                            .clicked()
                        {
                            self.prefs.detached = !self.prefs.detached;
                            self.player_expanded = false;
                        }
                        let video_label = if self.prefs.video_hidden {
                            "Show video"
                        } else {
                            "Hide video"
                        };
                        if ui.button(video_label).clicked() {
                            self.prefs.video_hidden = !self.prefs.video_hidden;
                            self.player_expanded = false;
                            if !self.prefs.video_hidden {
                                self.stream_refresh = Instant::now();
                                ui.ctx().request_repaint();
                            }
                        }
                        ui.label(badge(
                            if self.online {
                                "●  CHAT LIVE"
                            } else {
                                "●  CONNECTING"
                            },
                            if self.online { GREEN } else { MUTED },
                        ))
                        .on_hover_text(&self.status);
                    });
                });
            });
    }

    fn watch(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Now watching").size(19.0).strong());
            if let Some(stream) = self.current_stream() {
                ui.label(badge(stream.name().to_ascii_uppercase(), ACCENT));
            }
            if ui
                .button(if self.player_expanded {
                    "Show chat"
                } else {
                    "Theater"
                })
                .clicked()
            {
                self.player_expanded = !self.player_expanded;
            }
            if ui
                .add_enabled(self.tuned.is_some(), egui::Button::new("Back to Destiny"))
                .clicked()
            {
                self.tuned = None;
            }
        });
        self.channel_tabs(ui);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 28.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if let Some(stream) = self.current_stream() {
                    let url = stream.browser_url();
                    if ui.small_button("Browser ↗").on_hover_text(&url).clicked() {
                        open(&url);
                    }
                }
                let tune = ui.button("Tune").clicked();
                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.channel_input)
                        .id(ui.id().with("channel"))
                        .desired_width(f32::INFINITY)
                        .hint_text("Paste a stream URL or #kick/channel"),
                );
                let enter =
                    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if tune || enter {
                    if let Some(stream) = Stream::parse(self.channel_input.trim()) {
                        self.channel_input.clear();
                        self.tune(stream);
                    } else {
                        self.alert = Some(
                            "Enter a supported Kick, YouTube, Twitch, or Rumble stream link."
                                .into(),
                        );
                    }
                }
            },
        );
        if let Some(error) = self
            .alert
            .as_ref()
            .filter(|_| self.prefs.detached || self.player_expanded)
        {
            ui.label(RichText::new(error).small().color(RED));
        }

        let target = self.current_stream().cloned();
        let available = ui.available_size();
        let footer = 37.0;
        let size = egui::vec2(available.x.max(1.0), (available.y - footer).max(1.0));
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 8, Color32::from_rgb(8, 10, 16));
        self.player
            .sync(frame, rect, ui.ctx().pixels_per_point(), target.as_ref());
        if target.is_none() || self.player.error().is_some() {
            let message = if let Some(error) = self.player.error() {
                format!("Could not open the player\n{error}")
            } else if self.stream_error.is_some() {
                "Destiny’s stream status is temporarily unavailable.\nPaste a link to tune in manually.".into()
            } else if self.live.is_none() {
                "Checking Destiny’s live streams…".into()
            } else {
                "Destiny is offline.\nChoose a stream link from chat or paste one above.".into()
            };
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                message,
                egui::FontId::proportional(15.0),
                MUTED,
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if target.is_some() && ui.small_button("Reload player").clicked() {
                self.player.reset();
                ui.ctx().request_repaint();
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let title = target.as_ref().map_or("DESTINY.GG", |stream| {
                    self.live
                        .as_ref()
                        .and_then(|live| {
                            live.iter().find(|entry| entry.stream.same_channel(stream))
                        })
                        .map(|entry| entry.title.as_str())
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| stream.id())
                });
                ui.add(egui::Label::new(RichText::new(title).size(17.0).strong()).truncate());
            });
        });
    }

    fn channel_tabs(&mut self, ui: &mut egui::Ui) {
        let mut tabs: Vec<_> = self
            .live
            .iter()
            .flatten()
            .map(|live| {
                (
                    live.stream.clone(),
                    format!("{} · {} watching", live.stream.name(), live.viewers),
                    live.title.clone(),
                )
            })
            .collect();
        let live_count = tabs.len();
        let active = self.current_stream().cloned();
        for stream in self.recent_channels.iter() {
            if tabs[..live_count]
                .iter()
                .any(|(live, _, _)| live.same_channel(stream))
            {
                continue;
            }
            let (id, suffix) = if stream.id().len() > 20 {
                (&stream.id()[..20], "…")
            } else {
                (stream.id(), "")
            };
            tabs.push((
                stream.clone(),
                format!("{} · {id}{suffix}", stream.name()),
                stream.browser_url(),
            ));
        }
        if let Some(active) = &active
            && let Some(index) = tabs[live_count..]
                .iter()
                .position(|(stream, _, _)| stream.same_channel(active))
        {
            tabs[live_count..=live_count + index].rotate_right(1);
        }
        if tabs.is_empty() {
            self.show_channel_overflow = false;
            return;
        }

        let font = egui::TextStyle::Button.resolve(ui.style());
        let padding = 2.0 * ui.spacing().button_padding.x;
        let gap = ui.spacing().item_spacing.x;
        let measure = |text: &str| {
            ui.fonts(|fonts| {
                (fonts
                    .layout_no_wrap(text.to_owned(), font.clone(), TEXT)
                    .size()
                    .x
                    + padding)
                    .ceil()
            })
        };
        let widths: Vec<_> = tabs.iter().map(|(_, label, _)| measure(label)).collect();
        let visible = visible_tabs(&widths, gap, measure("+"), ui.available_width());
        if visible == tabs.len() {
            self.show_channel_overflow = false;
        }
        let mut selected = None;
        let is_active = |stream: &Stream| {
            active
                .as_ref()
                .is_some_and(|active| active.same_channel(stream))
        };
        ui.horizontal(|ui| {
            for (stream, label, tooltip) in &tabs[..visible] {
                if ui
                    .selectable_label(is_active(stream), label)
                    .on_hover_text(tooltip)
                    .clicked()
                {
                    selected = Some(stream.clone());
                }
            }
            if visible < tabs.len()
                && ui
                    .selectable_label(self.show_channel_overflow, "+")
                    .on_hover_text(format!("{} more channels", tabs.len() - visible))
                    .clicked()
            {
                self.show_channel_overflow = !self.show_channel_overflow;
            }
        });
        if self.show_channel_overflow {
            // Keep the overflow above the native player surface so every item can receive input.
            card().show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("extra_channels")
                    .max_height(105.0)
                    .show(ui, |ui| {
                        for (stream, label, tooltip) in &tabs[visible..] {
                            if ui
                                .selectable_label(is_active(stream), label)
                                .on_hover_text(tooltip)
                                .clicked()
                            {
                                selected = Some(stream.clone());
                            }
                        }
                    });
            });
        }
        if let Some(stream) = selected {
            self.show_channel_overflow = false;
            self.tune(stream);
        }
    }

    fn set_drawer(&mut self, drawer: Option<Drawer>) {
        if self.drawer != drawer {
            self.drawer_site.close();
            self.drawer = drawer;
        }
    }

    fn toggle_drawer(&mut self, drawer: Drawer, ctx: &egui::Context) {
        let next = (self.drawer != Some(drawer)).then_some(drawer);
        if self.sign_in.open {
            self.cancel_sign_in(ctx);
        }
        self.set_drawer(next);
    }

    fn cancel_sign_in(&mut self, ctx: &egui::Context) {
        self.sign_in.close();
        self.session_pending = None;
        if self.has_session && self.me.nick.is_empty() {
            self.reconnect(ctx, None);
        }
        self.alert = None;
    }

    fn left_drawer(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let progress = ctx.animate_bool_with_time(
            egui::Id::new("left_drawer_slide"),
            self.drawer.is_some(),
            0.18,
        );
        if progress <= 0.0 {
            return;
        }
        let reserve_chat =
            if !self.prefs.detached && !self.player_expanded && !self.prefs.video_hidden {
                MIN_CHAT_WIDTH
            } else {
                0.0
            };
        let width = (ctx.available_rect().width() - reserve_chat - 260.0).clamp(240.0, 420.0);
        egui::SidePanel::left("left_drawer")
            .exact_width(width * progress)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL))
            .show(ctx, |ui| {
                ui.set_min_width(ui.available_width());
                let Some(drawer) = self.drawer.filter(|_| progress >= 1.0) else {
                    return;
                };
                egui::Frame::NONE.inner_margin(12).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.heading(drawer.title());
                        if ui.small_button("Close").clicked() {
                            self.toggle_drawer(drawer, ui.ctx());
                        }
                        if let Some(url) = drawer.url() {
                            if ui.small_button("Browser ↗").clicked() {
                                open(&url);
                            }
                            if self.me.nick.is_empty() && ui.small_button("Sign in").clicked() {
                                self.set_drawer(Some(Drawer::Profile));
                                self.sign_in.begin();
                            }
                        }
                    });
                    if self.drawer != Some(drawer) {
                        return;
                    }
                    ui.separator();
                    if let Some(url) = drawer.url() {
                        self.drawer_site
                            .show(ui, frame, &url, self.session.as_ref());
                    } else if self.sign_in.open {
                        self.sign_in_panel(ui, frame);
                    } else {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.account(ui));
                    }
                });
            });
    }

    fn sign_in_panel(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("Connect your DGG account");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    self.cancel_sign_in(ui.ctx());
                }
            });
        });
        if !self.sign_in.open {
            return;
        }
        let host = self.sign_in.host();
        ui.label(quiet(if host.is_empty() {
            "www.destiny.gg"
        } else {
            &host
        }));
        if self.session_pending.is_some() {
            ui.label(badge("Confirming your Destiny.gg session…", GREEN));
        } else if let Some(alert) = &self.alert {
            ui.label(RichText::new(alert).color(RED));
        } else {
            ui.label(quiet("Choose the provider already linked to your account. Select Remember Me on Destiny.gg to stay signed in."));
        }
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        match self.sign_in.sync(frame, rect, ui.ctx()) {
            Ok(Some(session)) => {
                self.alert = None;
                self.reconnect(ui.ctx(), Some(session.clone()));
                self.session_pending = Some(session);
            }
            Ok(None) => {}
            Err(error) => self.alert = Some(format!("Could not open Destiny.gg sign-in: {error}")),
        }
    }

    fn account(&mut self, ui: &mut egui::Ui) {
        ui.heading("Account & chat");
        ui.label(quiet(
            "Sign in to post messages, send whispers, and cast votes.",
        ));
        ui.add_space(8.0);
        card().show(ui, |ui| {
            ui.set_max_width(600.0);
            ui.label(RichText::new("Destiny.gg account").strong());
            if self.me.nick.is_empty() {
                ui.label(quiet("Watching as a guest."));
                if ui.button("Connect DGG account").clicked() {
                    self.alert = None;
                    self.sign_in.begin();
                }
            } else {
                ui.label(RichText::new(format!("Signed in as {}", self.me.nick)).color(GREEN));
            }
            if (self.has_session || self.session_saved) && ui.button("Sign out").clicked() {
                match account::delete_session() {
                    Ok(()) => {
                        self.session_saved = false;
                        self.session_pending = None;
                        self.alert = None;
                        self.reconnect(ui.ctx(), None);
                    }
                    Err(error) => self.alert = Some(format!("Credential store: {error}")),
                }
            }
            if let Some(alert) = &self.alert {
                ui.label(RichText::new(alert).color(RED));
            }
        });
        ui.add_space(8.0);
        card().show(ui, |ui| {
            ui.set_max_width(600.0);
            ui.label(RichText::new("Chat preferences").strong());
            ui.checkbox(&mut self.prefs.timestamps, "Show message timestamps");
            ui.checkbox(&mut self.prefs.animate, "Animate chat emotes");
            ui.horizontal(|ui| {
                ui.label(quiet(format!(
                    "{} emotes · {} flairs",
                    self.catalog.emotes.len(),
                    self.catalog.flairs.len()
                )));
                if ui.small_button("Refresh assets").clicked() {
                    self.catalog_version = None;
                    self.catalog_retry = Instant::now();
                    ui.ctx().request_repaint();
                }
            });
            if let Some(error) = &self.catalog_error {
                ui.label(quiet(format!("Catalog refresh failed · {error}")));
            }
            if !self.prefs.ignored.is_empty() && ui.small_button("Clear ignored users").clicked() {
                self.prefs.ignored.clear();
            }
        });
    }

    fn docked_chat(&mut self, ctx: &egui::Context) {
        let available = ctx.available_rect();
        let max = (available.width() - MIN_PLAYER_WIDTH).max(MIN_CHAT_WIDTH);
        let width = self.prefs.chat_width.clamp(MIN_CHAT_WIDTH, max);
        egui::SidePanel::right("chat_panel")
            .exact_width(width)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin {
                left: 0,
                right: 14,
                top: 14,
                bottom: 14,
            }))
            .show(ctx, |ui| {
                let edge = available.right() - width;
                let grab = egui::Rect::from_min_max(
                    egui::pos2(edge, available.top()),
                    egui::pos2(edge + 12.0, available.bottom()),
                );
                let (pointer_x, hovered) =
                    resize_edge(ui, grab, edge, |pos| pos.x, &mut self.divider_offset);
                if let Some(x) = pointer_x {
                    self.prefs.chat_width = (available.right() - x).clamp(MIN_CHAT_WIDTH, max);
                    ctx.request_repaint();
                }
                let color = if hovered || self.divider_offset.is_some() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    ACCENT
                } else {
                    EDGE
                };
                ui.painter()
                    .vline(edge, available.y_range(), egui::Stroke::new(1.0_f32, color));
                ui.painter().rect_filled(
                    egui::Rect::from_center_size(grab.center(), egui::vec2(3.0, 28.0)),
                    2,
                    color,
                );
                egui::Frame::NONE
                    .inner_margin(egui::Margin {
                        left: 15,
                        ..Default::default()
                    })
                    .show(ui, |ui| self.chat_panel(ui, false));
            });
    }

    fn popout(&mut self, ctx: &egui::Context) {
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("dgg_chat_popout"),
            egui::ViewportBuilder::default()
                .with_title("DGG Chat")
                .with_inner_size([430.0, 720.0])
                .with_min_inner_size([330.0, 420.0]),
            |ctx, class| {
                if class == egui::ViewportClass::Embedded
                    || ctx.input(|input| input.viewport().close_requested())
                {
                    self.prefs.detached = false;
                    return;
                }
                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(PANEL).inner_margin(14))
                    .show(ctx, |ui| self.chat_panel(ui, true));
            },
        );
    }

    fn chat_panel(&mut self, ui: &mut egui::Ui, detached: bool) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("DGG Chat").size(18.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if detached && ui.small_button("Dock").clicked() {
                    self.prefs.detached = false;
                }
                ui.label(quiet(if self.online {
                    format!("{} connected", self.connections)
                } else {
                    "Reconnecting".into()
                }))
                .on_hover_text(&self.status);
            });
        });
        ui.horizontal(|ui| {
            if ui
                .selectable_label(!self.show_whispers, "Live chat")
                .clicked()
            {
                self.show_whispers = false;
            }
            let whisper = if self.unread > 0 {
                format!("Whispers ({})", self.unread)
            } else {
                "Whispers".into()
            };
            if ui.selectable_label(self.show_whispers, whisper).clicked() {
                self.show_whispers = true;
                self.unread = 0;
            }
            let scroll = if self.show_whispers {
                &mut self.whisper_scroll
            } else {
                &mut self.chat_scroll
            };
            if scroll.paused.is_some()
                && ui
                    .small_button("Jump to latest")
                    .on_hover_text("Chat is paused while you read")
                    .clicked()
            {
                scroll.paused = None;
                scroll.jump_to_bottom = true;
            }
        });
        if !self.show_whispers
            && let Some(pin) = &self.pin
        {
            let pin = pin.clone();
            card().show(ui, |ui| {
                ui.label(badge("PINNED MESSAGE", ACCENT));
                self.post(ui, &pin, LineKind::Chat, false);
            });
        }
        self.poll_panel(ui);
        ui.separator();
        egui::TopBottomPanel::bottom("chat_composer")
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                top: 6,
                ..Default::default()
            }))
            .show_inside(ui, |ui| self.composer(ui));
        if self.show_picker {
            self.emote_drawer(ui);
        } else {
            self.emote_resize_offset = None;
        }
        let (lines, scroll, scroll_id) = if self.show_whispers {
            (&self.whispers, &mut self.whisper_scroll, "whispers")
        } else {
            (&self.lines, &mut self.chat_scroll, "transcript")
        };
        let paused_offset = scroll.paused.as_ref().map(|paused| paused.offset);
        let jump = std::mem::take(&mut scroll.jump_to_bottom);
        let messages = if let Some(paused) = &scroll.paused {
            paused.lines.clone()
        } else {
            lines
                .iter()
                .filter(|line| {
                    !self
                        .prefs
                        .ignored
                        .contains(&line.post.user.nick.to_ascii_lowercase())
                })
                .cloned()
                .collect()
        };
        let mut area = egui::ScrollArea::vertical()
            .id_salt(scroll_id)
            .stick_to_bottom(paused_offset.is_none())
            .auto_shrink([false; 2]);
        if let Some(offset) = paused_offset {
            area = area.vertical_scroll_offset(offset);
        }
        let output = area.animated(!jump).show(ui, |ui| {
            if messages.is_empty() {
                let label = if self.show_whispers {
                    "New whispers will appear here. Use /w nickname message to send one."
                } else if self.online {
                    "Waiting for new messages…"
                } else {
                    "Connecting to Destiny.gg chat…"
                };
                ui.label(quiet(label));
            }
            ui.spacing_mut().item_spacing.y = 3.0;
            for line in &messages {
                self.post(ui, &line.post, line.kind, true);
            }
            if jump {
                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
            }
        });
        let scroll = if self.show_whispers {
            &mut self.whisper_scroll
        } else {
            &mut self.chat_scroll
        };
        let remaining = output.content_size.y - output.inner_rect.height() - output.state.offset.y;
        if remaining <= 1.0 {
            if scroll.paused.take().is_some() {
                scroll.jump_to_bottom = true;
                ui.ctx().request_repaint();
            }
        } else if !jump {
            if let Some(paused) = &mut scroll.paused {
                paused.offset = output.state.offset.y;
            } else {
                scroll.paused = Some(PausedTranscript {
                    lines: messages,
                    offset: output.state.offset.y,
                });
                ui.ctx().request_repaint();
            }
        }
    }

    fn poll_panel(&mut self, ui: &mut egui::Ui) {
        let Some(poll) = &mut self.poll else { return };
        if poll
            .pending
            .is_some_and(|at| at.elapsed() > Duration::from_secs(8))
        {
            poll.pending = None;
            self.alert = Some("DGG did not confirm that vote.".into());
        }
        let remaining = poll.remaining();
        let active = !poll.ended && !remaining.is_zero();
        if active || poll.pending.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        let mut vote = None;
        let mut dismiss = false;
        card().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(badge(
                    if poll.weighted {
                        "SUB-WEIGHTED POLL"
                    } else {
                        "DGG POLL"
                    },
                    ACCENT,
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if poll.ended && ui.small_button("Dismiss").clicked() {
                        dismiss = true;
                    }
                    ui.label(quiet(if active {
                        format!("{}s left", remaining.as_secs_f32().ceil() as u32)
                    } else if poll.ended {
                        "Closed".into()
                    } else {
                        "Awaiting results".into()
                    }));
                });
            });
            ui.label(RichText::new(&poll.question).strong())
                .on_hover_text(format!("Started by {}", poll.nick));
            let height = (ui.available_height() * 0.30).clamp(64.0, 160.0);
            egui::ScrollArea::vertical()
                .id_salt("poll_options")
                .max_height(height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (index, option) in poll.options.iter().enumerate() {
                        let count = poll.totals.get(index).copied().unwrap_or(0);
                        let percent = if poll.totalvotes == 0 {
                            0.0
                        } else {
                            count as f64 / poll.totalvotes as f64 * 100.0
                        };
                        let selected = poll.myvote == index + 1;
                        let summary = if poll.hidden {
                            String::new()
                        } else {
                            format!("  ·  {percent:.0}% ({count})")
                        };
                        let label = format!(
                            "{}  {option}{summary}",
                            if selected {
                                "✓".to_owned()
                            } else {
                                (index + 1).to_string()
                            }
                        );
                        let can_vote = active
                            && self.online
                            && !self.me.nick.is_empty()
                            && poll.canvote
                            && poll.pending.is_none();
                        let button = egui::Button::new(label)
                            .selected(selected)
                            .min_size(egui::vec2(ui.available_width(), 24.0));
                        if ui.add_enabled(can_vote, button).clicked() {
                            vote = Some(index + 1);
                        }
                    }
                });
            let status = if poll.pending.is_some() {
                "Waiting for vote confirmation".into()
            } else if poll.hidden {
                format!("{} votes · Results hidden", poll.totalvotes)
            } else {
                format!("{} votes", poll.totalvotes)
            };
            ui.label(quiet(status));
        });
        if let Some(vote) = vote {
            self.vote(vote);
        }
        if dismiss {
            self.poll = None;
        }
    }

    fn composer(&mut self, ui: &mut egui::Ui) {
        if let Some(alert) = self.alert.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(alert).small().color(RED));
                if ui.small_button("Dismiss").clicked() {
                    self.alert = None;
                }
            });
        }
        let id = ui.id().with("message_input");
        if ui.memory(|memory| memory.has_focus(id)) {
            let state = egui::TextEdit::load_state(ui.ctx(), id);
            let cursor = state.as_ref().and_then(|state| state.cursor.char_range());
            if let Some((range, suggestion)) = self.completion(cursor) {
                let tab = ui.input_mut(|input| {
                    let mut pressed = false;
                    input.events.retain(|event| {
                        let complete = matches!(event, egui::Event::Key {
                            key: egui::Key::Tab, pressed: true, modifiers, ..
                        } if modifiers.is_none());
                        pressed |= complete;
                        !complete
                    });
                    pressed
                });
                if tab || ui.small_button(format!("Tab   {suggestion}")).clicked() {
                    let position =
                        self.draft[..range.start].chars().count() + suggestion.chars().count() + 1;
                    self.draft.replace_range(range, &format!("{suggestion} "));
                    let mut state = state.unwrap_or_default();
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(
                            egui::text::CCursor::new(position),
                        )));
                    state.store(ui.ctx(), id);
                    ui.memory_mut(|memory| memory.request_focus(id));
                }
            }
        }
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 30.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let send = ui.button("Send").clicked();
                if ui.selectable_label(self.show_picker, "Emotes").clicked() {
                    self.show_picker = !self.show_picker;
                }
                let hint = if self.show_whispers {
                    "/w nickname message"
                } else if self.me.nick.is_empty() {
                    "Sign in to chat"
                } else {
                    "Message DGG Chat"
                };
                let mut edit = egui::TextEdit::singleline(&mut self.draft)
                    .id(id)
                    .desired_width(f32::INFINITY)
                    .min_size(egui::vec2(0.0, 30.0))
                    .vertical_align(egui::Align::Center)
                    .hint_text(hint)
                    .show(ui);
                let field = edit.response;
                let mut cursor = edit.state.cursor.char_range();
                if self.focus_input {
                    field.request_focus();
                    cursor = Some(egui::text::CCursorRange::one(egui::text::CCursor::new(
                        self.draft.chars().count(),
                    )));
                    edit.state.cursor.set_char_range(cursor);
                    edit.state.store(ui.ctx(), id);
                    self.focus_input = false;
                }
                if field.has_focus() {
                    // TextEdit sets its own filter while drawing. Install ours afterwards
                    // so egui sees it before it dispatches Tab on the next frame.
                    let complete = self.completion(cursor).is_some();
                    ui.memory_mut(|memory| {
                        memory.set_focus_lock_filter(
                            id,
                            egui::EventFilter {
                                tab: complete,
                                horizontal_arrows: true,
                                vertical_arrows: true,
                                ..Default::default()
                            },
                        )
                    });
                }
                let enter =
                    field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if send || enter {
                    if self.show_whispers
                        && !matches!(
                            self.draft.split_whitespace().next(),
                            Some("/w" | "/msg" | "/whisper" | "/r" | "/reply")
                        )
                    {
                        self.alert =
                            Some("Use /w nickname message or /r message in Whispers.".into());
                    } else {
                        self.submit();
                    }
                }
            },
        );
    }

    fn completion(
        &self,
        cursor: Option<egui::text::CCursorRange>,
    ) -> Option<(std::ops::Range<usize>, String)> {
        let caret = match cursor {
            Some(cursor) if cursor.primary.index != cursor.secondary.index => return None,
            Some(cursor) => self
                .draft
                .char_indices()
                .nth(cursor.primary.index)
                .map_or(self.draft.len(), |(byte, _)| byte),
            None => self.draft.len(),
        };
        let before = &self.draft[..caret];
        let start = before.trim_end_matches(|c: char| !c.is_whitespace()).len();
        let token = &before[start..];
        let suggestion = self.suggestion(token)?;
        let end = self.draft[caret..]
            .find(char::is_whitespace)
            .map_or(self.draft.len(), |offset| caret + offset);
        let end = end + usize::from(self.draft[end..].starts_with(' '));
        Some((start..end, suggestion))
    }

    fn suggestion(&self, token: &str) -> Option<String> {
        if let Some(prefix) = token.strip_prefix('@').filter(|prefix| !prefix.is_empty()) {
            let lower = prefix.to_ascii_lowercase();
            return self
                .users
                .iter()
                .filter(|(nick, _)| nick.starts_with(&lower) && *nick != &lower)
                .min_by_key(|(nick, _)| (nick.len(), *nick))
                .map(|(_, user)| format!("@{}", user.nick));
        }
        let lower = token.to_ascii_lowercase();
        if lower.len() < 2 || self.catalog.emotes.contains_key(token) {
            return None;
        }
        self.catalog
            .emotes
            .values()
            .filter(|emote| {
                emote.allowed(&self.me) && emote.prefix.to_ascii_lowercase().starts_with(&lower)
            })
            .min_by_key(|emote| {
                (
                    !self.prefs.favorites.contains(&emote.prefix),
                    emote.prefix.len(),
                    &emote.prefix,
                )
            })
            .map(|emote| emote.prefix.clone())
    }

    fn emote_drawer(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_rect_before_wrap();
        let max = (available.height() - 75.0).max(0.0);
        if max < 100.0 {
            return;
        }
        let min = 134.0_f32.min(max);
        let height = self.prefs.emote_drawer_height.clamp(min, max);
        egui::TopBottomPanel::bottom("emote_drawer")
            .exact_height(height)
            .show_separator_line(false)
            .frame(card().inner_margin(egui::Margin {
                top: 0,
                ..egui::Margin::same(10)
            }))
            .show_inside(ui, |ui| {
                let (grip, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 12.0),
                    egui::Sense::hover(),
                );
                let edge = available.bottom() - height;
                let (y, hovered) =
                    resize_edge(ui, grip, edge, |pos| pos.y, &mut self.emote_resize_offset);
                if let Some(y) = y {
                    self.prefs.emote_drawer_height = (available.bottom() - y).clamp(min, max);
                    ui.ctx().request_repaint();
                }
                let color = if hovered || self.emote_resize_offset.is_some() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                    ACCENT
                } else {
                    MUTED
                };
                ui.painter().rect_filled(
                    egui::Rect::from_center_size(grip.center(), egui::vec2(32.0, 3.0)),
                    2,
                    color,
                );
                self.emote_picker(ui);
            });
    }

    fn emote_picker(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Emotes").strong());
            ui.label(quiet(format!("{}", self.catalog.emotes.len())));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Close").clicked() {
                    self.show_picker = false;
                }
                if ui
                    .selectable_label(self.only_favorites, "Favorites")
                    .clicked()
                {
                    self.only_favorites = !self.only_favorites;
                }
            });
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.picker_search)
                .hint_text("Search DGG emotes")
                .desired_width(f32::INFINITY),
        );
        let search = self.picker_search.to_ascii_lowercase();
        let emotes = self
            .catalog
            .emotes
            .values()
            .filter(|emote| {
                emote.prefix.to_ascii_lowercase().contains(&search)
                    && (!self.only_favorites || self.prefs.favorites.contains(&emote.prefix))
            })
            .collect::<Vec<_>>();
        if emotes.is_empty() {
            let message = if self.catalog.emotes.is_empty() {
                "Waiting for the emote catalog…"
            } else {
                "No matching emotes."
            };
            ui.label(quiet(message));
        }
        let columns = (ui.available_width() / 70.0).floor().max(3.0) as usize;
        let row_height = 43.0;
        let cell_width =
            (ui.available_width() - (columns as f32 - 1.0) * ui.spacing().item_spacing.x - 10.0)
                / columns as f32;
        let height = ui.available_height().max(0.0);
        egui::ScrollArea::vertical()
            .id_salt("emote_grid")
            .auto_shrink([false, false])
            .max_height(height)
            .show_rows(
                ui,
                row_height,
                emotes.len().div_ceil(columns),
                |ui, rows| {
                    for row in rows {
                        ui.horizontal(|ui| {
                            for emote in emotes.iter().skip(row * columns).take(columns) {
                                let allowed = emote.allowed(&self.me);
                                let cell = ui.allocate_ui_with_layout(
                                    egui::vec2(cell_width, row_height),
                                    egui::Layout::top_down(egui::Align::Center),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.y = 1.0;
                                        if let Some(image) = self.images.image(
                                            ui,
                                            emote.image.first(),
                                            emote.sprite,
                                            24.0,
                                            self.prefs.animate,
                                        ) {
                                            ui.add(
                                                image.max_size(egui::vec2(cell_width - 4.0, 24.0)),
                                            );
                                        } else {
                                            ui.label(quiet("…"));
                                        }
                                        let color = if self.prefs.favorites.contains(&emote.prefix)
                                        {
                                            ACCENT
                                        } else if allowed {
                                            TEXT
                                        } else {
                                            MUTED
                                        };
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&emote.prefix)
                                                    .size(10.0)
                                                    .color(color),
                                            )
                                            .truncate(),
                                        );
                                    },
                                );
                                let hit = ui.interact(
                                    cell.response.rect,
                                    ui.id().with(("emote", &emote.prefix)),
                                    egui::Sense::click(),
                                );
                                if hit.hovered() {
                                    ui.painter().rect_stroke(
                                        hit.rect,
                                        4,
                                        egui::Stroke::new(1.0_f32, ACCENT),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                let access = if allowed {
                                    "Click to insert"
                                } else if emote.patreon_only {
                                    "Patreon emote"
                                } else {
                                    "Subscriber emote"
                                };
                                let hit = hit.on_hover_text(format!(
                                    "{} · {}\n{} · Right-click to favorite",
                                    emote.prefix,
                                    emote
                                        .creator
                                        .as_deref()
                                        .filter(|name| !name.is_empty())
                                        .unwrap_or("DGG"),
                                    access
                                ));
                                if hit.clicked() && allowed {
                                    insert_word(&mut self.draft, &emote.prefix);
                                    self.focus_input = true;
                                    ui.ctx().request_repaint();
                                    ui.ctx().request_repaint();
                                }
                                if hit.secondary_clicked()
                                    && !self.prefs.favorites.remove(&emote.prefix)
                                {
                                    self.prefs.favorites.insert(emote.prefix.clone());
                                }
                            }
                        });
                    }
                },
            );
    }

    fn post(&mut self, ui: &mut egui::Ui, post: &Post, kind: LineKind, menu: bool) {
        static TOKENS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\S+|[^\S\n]+|\n").unwrap());
        let mentioned = !self.me.nick.is_empty()
            && post
                .text
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| word.eq_ignore_ascii_case(&self.me.nick));
        let green_text = post.text.starts_with('>');
        let color = match kind {
            LineKind::Event => GREEN,
            LineKind::Whisper => ACCENT,
            LineKind::Chat if green_text => GREEN,
            _ => TEXT,
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if self.prefs.timestamps
                && let Some(time) =
                    DateTime::from_timestamp_millis(post.timestamp).filter(|_| post.timestamp != 0)
            {
                ui.label(quiet(
                    time.with_timezone(&Local).format("%H:%M").to_string(),
                ));
                ui.add_space(6.0);
            }
            if kind != LineKind::Event && !post.user.nick.is_empty() {
                let mut flairs = post
                    .user
                    .features
                    .iter()
                    .filter_map(|name| self.catalog.flairs.get(name))
                    .collect::<Vec<_>>();
                flairs.sort_by_key(|flair| flair.priority);
                let nick_color = flairs
                    .iter()
                    .find_map(|flair| flair.color.as_deref().and_then(hex_color))
                    .unwrap_or(ACCENT);
                for flair in flairs {
                    if let Some(image) =
                        self.images
                            .image(ui, flair.image.first(), None, 15.0, self.prefs.animate)
                    {
                        ui.add(image).on_hover_text(&flair.label);
                        ui.add_space(3.0);
                    }
                }
                let nick = ui.add(
                    egui::Label::new(RichText::new(&post.user.nick).strong().color(nick_color))
                        .sense(egui::Sense::click()),
                );
                ui.label(
                    RichText::new(if kind == LineKind::Whisper {
                        " › "
                    } else {
                        ": "
                    })
                    .color(MUTED),
                );
                if menu {
                    if nick.clicked() {
                        insert_word(&mut self.draft, &format!("@{}", post.user.nick));
                        self.focus_input = true;
                    }
                    nick.context_menu(|ui| {
                        if ui.button("Mention").clicked() {
                            insert_word(&mut self.draft, &format!("@{}", post.user.nick));
                            self.focus_input = true;
                            ui.close_menu();
                        }
                        if ui.button("Whisper").clicked() {
                            self.draft = format!("/w {} ", post.user.nick);
                            self.focus_input = true;
                            ui.close_menu();
                        }
                        if ui.button("Ignore user").clicked() {
                            self.prefs
                                .ignored
                                .insert(post.user.nick.to_ascii_lowercase());
                            ui.close_menu();
                        }
                    });
                }
            }
            for token in TOKENS.find_iter(&post.text).map(|matched| matched.as_str()) {
                let link = token.trim_end_matches(['.', ',', ')', ']', '!', '>', '"']);
                if let Some(stream) = Stream::parse(link) {
                    let label = format!("{} · {} ↗", stream.name(), stream.id());
                    if ui
                        .link(label)
                        .on_hover_text(if self.prefs.video_hidden {
                            format!("Select {link} for when video is shown")
                        } else {
                            format!("Tune the player to {link}")
                        })
                        .clicked()
                    {
                        self.tune(stream);
                    }
                    ui.label(&token[link.len()..]);
                } else if let Some(url) = player::web_url(link) {
                    ui.add(egui::Hyperlink::from_label_and_url(link, url.as_str()));
                    ui.label(&token[link.len()..]);
                } else if let Some(emote) = self
                    .catalog
                    .emotes
                    .get(token)
                    .filter(|emote| kind == LineKind::Event || emote.allowed(&post.user))
                    && let Some(image) = self.images.image(
                        ui,
                        emote.image.first(),
                        emote.sprite,
                        29.0,
                        self.prefs.animate,
                    )
                {
                    let response = ui
                        .add(image.sense(egui::Sense::click()))
                        .on_hover_text(&emote.prefix);
                    if response.clicked() {
                        insert_word(&mut self.draft, token);
                        self.focus_input = true;
                    }
                } else {
                    let mut text = RichText::new(token).color(color);
                    if mentioned {
                        text = text.background_color(Color32::from_rgb(57, 45, 83));
                    }
                    ui.label(text);
                }
            }
        });
    }
}

fn resize_edge(
    ui: &egui::Ui,
    grip: egui::Rect,
    edge: f32,
    coordinate: impl Fn(egui::Pos2) -> f32,
    offset: &mut Option<f32>,
) -> (Option<f32>, bool) {
    ui.input(|input| {
        let mut position = None;
        // Anchor to the press position even if movement and release arrive in the same frame.
        for event in &input.events {
            match event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                } if grip.contains(*pos) => *offset = Some(coordinate(*pos) - edge),
                egui::Event::PointerMoved(pos) => {
                    if let Some(offset) = *offset {
                        position = Some(coordinate(*pos) - offset);
                    }
                }
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    ..
                } => {
                    if let Some(offset) = offset.take() {
                        position = Some(coordinate(*pos) - offset);
                    }
                }
                egui::Event::WindowFocused(false) => *offset = None,
                _ => {}
            }
        }
        (
            position,
            input
                .pointer
                .hover_pos()
                .is_some_and(|pos| grip.contains(pos)),
        )
    })
}

fn insert_word(draft: &mut String, word: &str) {
    if !draft.is_empty() && !draft.ends_with(char::is_whitespace) {
        draft.push(' ');
    }
    draft.push_str(word);
    draft.push(' ');
}

fn hex_color(hex: &str) -> Option<Color32> {
    let value = hex.strip_prefix('#').filter(|hex| hex.len() == 6)?;
    let rgb = u32::from_str_radix(value, 16).ok()?;
    Some(Color32::from_rgb(
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::visible_tabs;

    #[test]
    fn channel_strip_reserves_overflow_space_only_when_needed() {
        assert_eq!(visible_tabs(&[70.0, 80.0], 7.0, 25.0, 157.0), 2);
        assert_eq!(visible_tabs(&[70.0, 80.0, 90.0], 7.0, 25.0, 189.0), 2);
        assert_eq!(visible_tabs(&[70.0, 80.0, 90.0], 7.0, 25.0, 188.0), 1);
        assert_eq!(visible_tabs(&[70.0], 7.0, 25.0, 45.0), 0);
    }
}
