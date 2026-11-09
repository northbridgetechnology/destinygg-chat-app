use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};

use eframe::egui;
use regex::Regex;
use serde::Deserialize;
use tokio::sync::{Semaphore, mpsc};

use crate::chat::User;

const CDN: &str = "https://cdn.destiny.gg";
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub url: Option<String>,
    pub width: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Emote {
    pub prefix: String,
    pub creator: Option<String>,
    pub minimum_sub_tier: u8,
    pub patreon_only: bool,
    pub image: Vec<Asset>,
    #[serde(skip)]
    pub sprite: Option<Sprite>,
}

impl Emote {
    pub fn allowed(&self, user: &User) -> bool {
        user.privileged()
            || if self.patreon_only {
                user.patreon_member
            } else {
                self.minimum_sub_tier <= user.tier()
            }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Flair {
    pub name: String,
    pub label: String,
    pub color: Option<String>,
    pub priority: i32,
    pub image: Vec<Asset>,
}

#[derive(Clone, Copy, Debug)]
pub struct Sprite {
    width: u32,
    steps: Option<u32>,
    period: Duration,
}

#[derive(Default)]
pub struct Catalog {
    pub emotes: BTreeMap<String, Emote>,
    pub flairs: HashMap<String, Flair>,
}

impl Catalog {
    pub async fn fetch(client: &reqwest::Client, version: &str) -> Result<Self, String> {
        let emotes = client
            .get(format!("{CDN}/emotes/emotes.json"))
            .query(&[("_", version)]);
        let flairs = client
            .get(format!("{CDN}/flairs/flairs.json"))
            .query(&[("_", version)]);
        let styles = client
            .get(format!("{CDN}/emotes/emotes.css"))
            .query(&[("_", version)]);
        let (emotes, flairs, styles) = tokio::join!(emotes.send(), flairs.send(), styles.send());
        let emotes: Vec<Emote> = emotes
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let flairs: Vec<Flair> = flairs
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        let styles = styles
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self::from_parts(emotes, flairs, &styles))
    }

    fn from_parts(emotes: Vec<Emote>, flairs: Vec<Flair>, css: &str) -> Self {
        let mut catalog = Self {
            emotes: emotes.into_iter().map(|e| (e.prefix.clone(), e)).collect(),
            flairs: flairs.into_iter().map(|f| (f.name.clone(), f)).collect(),
        };
        let blocks = Regex::new(r"(?m)^\.emote\.([\w-]+)\s*\{([^{}]*)\}").unwrap();
        let width = Regex::new(r"(?:^|;)\s*width:\s*(\d+)px").unwrap();
        let steps = Regex::new(r"steps\(\s*(\d+)").unwrap();
        let period = Regex::new(r"(?:^|[^\w.-])(\d*\.?\d+)(ms|s)\b").unwrap();
        for block in blocks.captures_iter(css) {
            let Some(emote) = catalog.emotes.get_mut(&block[1]) else {
                continue;
            };
            let Some(sheet_width) = emote.image.first().and_then(|image| image.width) else {
                continue;
            };
            let body = &block[2];
            let frame_width = width
                .captures_iter(body)
                .last()
                .and_then(|m| m[1].parse::<u32>().ok());
            let Some(frame_width) = frame_width.filter(|w| *w > 0 && *w * 2 <= sheet_width) else {
                continue;
            };
            let stepped = body.split(';').find(|part| {
                part.trim_start().starts_with("animation:") && part.contains("steps(")
            });
            let count = stepped
                .and_then(|value| steps.captures(value))
                .and_then(|m| m[1].parse().ok());
            let seconds = stepped
                .and_then(|value| period.captures(value))
                .and_then(|m| {
                    m[1].parse::<f64>()
                        .ok()
                        .map(|value| if &m[2] == "ms" { value / 1000.0 } else { value })
                });
            emote.sprite = Some(Sprite {
                width: frame_width,
                steps: count.or(emote.sprite.and_then(|sprite| sprite.steps)),
                period: seconds
                    .filter(|s| (0.01..=60.0).contains(s))
                    .map(Duration::from_secs_f64)
                    .or(emote.sprite.map(|sprite| sprite.period))
                    .unwrap_or(Duration::from_millis(700)),
            });
        }
        catalog
    }
}

struct Frame {
    image: egui::ColorImage,
    delay: Duration,
}

struct Decoded {
    frames: Vec<Frame>,
    duration: Duration,
}

struct Texture {
    decoded: Decoded,
    texture: egui::TextureHandle,
    showing: usize,
    created: Instant,
    used: Instant,
}

impl Texture {
    fn source(
        &mut self,
        ctx: &egui::Context,
        sprite: Option<Sprite>,
        animate: bool,
    ) -> (egui::TextureId, egui::Vec2, egui::Rect) {
        self.used = Instant::now();
        let elapsed = self.created.elapsed();
        if animate && self.decoded.frames.len() > 1 {
            let mut at = elapsed.as_millis() % self.decoded.duration.as_millis().max(1);
            for (index, frame) in self.decoded.frames.iter().enumerate() {
                let duration = frame.delay.as_millis();
                if at < duration {
                    if index != self.showing {
                        self.showing = index;
                        self.texture
                            .set(frame.image.clone(), egui::TextureOptions::LINEAR);
                    }
                    ctx.request_repaint_after(Duration::from_millis((duration - at) as u64));
                    break;
                }
                at = at.saturating_sub(duration);
            }
        } else if self.showing != 0 {
            self.showing = 0;
            self.texture.set(
                self.decoded.frames[0].image.clone(),
                egui::TextureOptions::LINEAR,
            );
        }
        let size = self.texture.size_vec2();
        let (size, uv) = if let Some(sprite) = sprite.filter(|sprite| sprite.width as f32 <= size.x)
        {
            let frame_width = sprite.width as f32;
            let available = (size.x / frame_width).floor() as u32;
            let steps = sprite.steps.unwrap_or(available).min(available).max(1);
            let period = sprite.period.as_secs_f64();
            let at = if animate {
                elapsed.as_secs_f64() % period
            } else {
                0.0
            };
            let frame = (at / period * f64::from(steps)).floor() as u32;
            if animate && steps > 1 {
                ctx.request_repaint_after(Duration::from_secs_f64(
                    (period / f64::from(steps)).max(1.0 / 60.0),
                ));
            }
            let start = frame_width * frame as f32 / size.x;
            (
                egui::vec2(frame_width, size.y),
                egui::Rect::from_min_max(
                    egui::pos2(start, 0.0),
                    egui::pos2(start + frame_width / size.x, 1.0),
                ),
            )
        } else {
            (
                size,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            )
        };
        (self.texture.id(), size, uv)
    }
}

enum Entry {
    Loading,
    Failed(Instant),
    Ready(Texture),
}

pub struct Images {
    entries: HashMap<String, Entry>,
    send: mpsc::Sender<(String, Result<Decoded, String>)>,
    receive: mpsc::Receiver<(String, Result<Decoded, String>)>,
    permits: Arc<Semaphore>,
    client: reqwest::Client,
}

impl Images {
    pub fn new(client: reqwest::Client) -> Self {
        let (send, receive) = mpsc::channel(64);
        Self {
            entries: HashMap::new(),
            send,
            receive,
            permits: Arc::new(Semaphore::new(10)),
            client,
        }
    }

    pub fn collect(&mut self, ctx: &egui::Context) {
        while let Ok((url, result)) = self.receive.try_recv() {
            let entry = match result {
                Ok(decoded) => Entry::Ready(Texture {
                    texture: ctx.load_texture(
                        &url,
                        decoded.frames[0].image.clone(),
                        egui::TextureOptions::LINEAR,
                    ),
                    decoded,
                    showing: 0,
                    created: Instant::now(),
                    used: Instant::now(),
                }),
                Err(_) => Entry::Failed(Instant::now()),
            };
            self.entries.insert(url, entry);
        }
        let mut total = 0;
        let mut ready = Vec::new();
        for (url, entry) in &self.entries {
            if let Entry::Ready(texture) = entry {
                let size: usize = texture
                    .decoded
                    .frames
                    .iter()
                    .map(|frame| frame.image.pixels.len() * 4)
                    .sum();
                total += size;
                ready.push((url.clone(), texture.used, size));
            }
        }
        if total > 100 * 1024 * 1024 {
            ready.sort_by_key(|(_, time, _)| *time);
            for (url, _, size) in ready {
                if total <= 80 * 1024 * 1024 {
                    break;
                }
                self.entries.remove(&url);
                total -= size;
            }
        }
    }

    pub fn image(
        &mut self,
        ui: &egui::Ui,
        asset: Option<&Asset>,
        sprite: Option<Sprite>,
        height: f32,
        animate: bool,
    ) -> Option<egui::Image<'static>> {
        let url = asset?.url.as_ref()?;
        if let Some(Entry::Ready(texture)) = self.entries.get_mut(url) {
            let (id, size, uv) = texture.source(ui.ctx(), sprite, animate);
            let size = size * (height.min(size.y) / size.y);
            return Some(egui::Image::new((id, size)).uv(uv));
        }
        let should_load = match self.entries.get(url) {
            None => true,
            Some(Entry::Failed(at)) => {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs(60).saturating_sub(at.elapsed()));
                at.elapsed() >= Duration::from_secs(60)
            }
            _ => false,
        };
        if should_load && url.starts_with("https://cdn.destiny.gg/") {
            self.entries.insert(url.clone(), Entry::Loading);
            let (url, client, tx, ctx, permits) = (
                url.clone(),
                self.client.clone(),
                self.send.clone(),
                ui.ctx().clone(),
                Arc::clone(&self.permits),
            );
            tokio::spawn(async move {
                let Ok(_permit) = permits.acquire().await else {
                    return;
                };
                let result = download(&client, &url).await;
                if tx.send((url, result)).await.is_ok() {
                    ctx.request_repaint();
                }
            });
        }
        None
    }
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Decoded, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if data.len() + chunk.len() > MAX_BYTES {
            return Err("Image exceeded download limit".into());
        }
        data.extend_from_slice(&chunk);
    }
    tokio::task::spawn_blocking(move || decode(&data))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos::decode;

#[cfg(any(target_os = "windows", target_os = "linux"))]
mod portable;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use portable::decode;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_accepts_null_creator_and_css_sprites() {
        let emotes = serde_json::from_str(
            r#"[{"prefix":"Example","creator":null,"image":[{"width":192}]}]"#,
        )
        .unwrap();
        let catalog = Catalog::from_parts(
            emotes,
            vec![],
            ".emote.Example { width: 32px; animation: Example 1.2s steps(6) 2; }",
        );
        let sprite = catalog.emotes["Example"].sprite.unwrap();
        assert_eq!(sprite.width, 32);
        assert_eq!(sprite.steps, Some(6));
        assert_eq!(sprite.period, Duration::from_millis(1200));
    }

    #[tokio::test]
    #[ignore = "downloads the live Destiny.gg asset catalog"]
    async fn live_catalog_decodes_every_image() {
        use futures_util::{StreamExt, stream};
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let catalog = Catalog::fetch(&client, "").await.unwrap();
        assert!(!catalog.emotes.is_empty());
        assert!(!catalog.flairs.is_empty());
        let urls = catalog
            .emotes
            .values()
            .flat_map(|emote| &emote.image)
            .chain(catalog.flairs.values().flat_map(|flair| &flair.image))
            .filter_map(|image| image.url.as_deref())
            .filter(|url| cfg!(target_os = "macos") || !url.ends_with(".avif"))
            .collect::<Vec<_>>();
        let results = stream::iter(urls.iter().copied())
            .map(|url| {
                let client = &client;
                async move {
                    (
                        url,
                        download(client, url).await.map(|asset| asset.frames.len()),
                    )
                }
            })
            .buffer_unordered(8)
            .collect::<Vec<_>>()
            .await;
        let failures = results
            .iter()
            .filter_map(|(url, result)| result.as_ref().err().map(|error| (*url, error)))
            .collect::<Vec<_>>();
        assert!(failures.is_empty(), "{failures:?}");
        #[cfg(target_os = "macos")]
        assert!(results.iter().any(|(url, result)| url.ends_with(".avif")
            && result.as_ref().is_ok_and(|frames| *frames > 1)));
        println!(
            "Decoded {} emotes and {} flairs ({} images)",
            catalog.emotes.len(),
            catalog.flairs.len(),
            urls.len()
        );
    }
}
