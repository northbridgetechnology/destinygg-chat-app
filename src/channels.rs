use std::time::Duration;

use crate::player::Stream;

const CHANNEL_TTL_MS: i64 = 3 * 60 * 60 * 1_000;
const MAX_RECENT_CHANNELS: usize = 128;

#[derive(Default)]
pub struct RecentChannels {
    entries: Vec<(Stream, i64)>,
}

impl RecentChannels {
    pub fn observe(&mut self, message: &str, posted_at: i64, now: i64) {
        let posted_at = posted_at.min(now);
        if posted_at <= now - CHANNEL_TTL_MS {
            return;
        }
        let links = message.split_whitespace().filter_map(|token| {
            let link =
                token.trim_matches(['(', ')', '[', ']', '<', '>', '"', '\'', ',', '.', '!', ';']);
            link.starts_with('#')
                .then_some(link)
                .and_then(Stream::parse)
                .and_then(|stream| stream.channel_tab())
        });
        for stream in links {
            if let Some((_, last_seen)) =
                self.entries.iter_mut().find(|(entry, _)| *entry == stream)
            {
                // History replay and late messages must not undo a more recent mention.
                *last_seen = (*last_seen).max(posted_at);
            } else {
                self.entries.push((stream, posted_at));
                if self.entries.len() > MAX_RECENT_CHANNELS
                    && let Some((oldest, _)) = self
                        .entries
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, (_, at))| *at)
                {
                    self.entries.remove(oldest);
                }
            }
        }
    }

    pub fn expire(&mut self, now: i64) -> Option<Duration> {
        self.entries.retain(|(_, at)| *at > now - CHANNEL_TTL_MS);
        self.entries
            .iter()
            .map(|(_, at)| Duration::from_millis((at + CHANNEL_TTL_MS - now) as u64))
            .min()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Stream> {
        // New discoveries appear first. Repeat mentions extend the expiry in place.
        self.entries.iter().rev().map(|(stream, _)| stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_hashes_are_normalized_and_expire_from_last_post() {
        let now = CHANNEL_TTL_MS * 2;
        let mut channels = RecentChannels::default();
        channels.observe(
            "try (#kick/Example), #youtube/mHLKCNmOmm8?t=15 #bad/test",
            now - 20,
            now,
        );
        channels.observe("#kick/example #youtube/mHLKCNmOmm8?t=19", now, now);
        channels.observe(
            "#kick/example #kick/expired https://kick.com/unadvertised",
            now - CHANNEL_TTL_MS,
            now,
        );
        channels.observe("#kick/Example", now - 10, now);
        assert_eq!(
            channels.iter().cloned().collect::<Vec<_>>(),
            [
                Stream::YouTube("mHLKCNmOmm8".into(), 0),
                Stream::Kick("example".into())
            ]
        );
        assert_eq!(
            channels.expire(now + CHANNEL_TTL_MS - 1),
            Some(Duration::from_millis(1))
        );
        assert_eq!(channels.expire(now + CHANNEL_TTL_MS), None);
        assert_eq!(channels.iter().count(), 0);
    }
}
