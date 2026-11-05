//! Telegram as a [`HumanChannel`]: each message is one `sendMessage` to one chat, so the owner's
//! phone buzzes once per episode and level.
//!
//! The bot token is a secret and is passed by reference only: the caller reads it from an
//! environment variable it names. It never appears in an error, a log line or a message (request
//! errors are reported without their URL, which carries the token).
//!
//! Delivery is once per key within a process. Across a crash between Telegram accepting a message
//! and the ledger recording it, the message can arrive twice: Telegram's API has no idempotency key.

use std::collections::BTreeSet;
use std::sync::Mutex;

use async_trait::async_trait;

use super::human::HumanChannel;
use crate::CallError;
use crate::episodes::Notice;

/// A Telegram bot writing to one chat.
pub struct Telegram {
    client: reqwest::Client,
    endpoint: String,
    chat: String,
    sent: Mutex<BTreeSet<String>>,
}

impl Telegram {
    /// A channel to `chat` through the bot whose token is `token`.
    ///
    /// # Errors
    /// The HTTP client cannot be built.
    pub fn new(token: &str, chat: &str) -> Result<Self, CallError> {
        Self::at("https://api.telegram.org", token, chat)
    }

    /// The same against another API base, for tests.
    ///
    /// # Errors
    /// The HTTP client cannot be built.
    pub fn at(base: &str, token: &str, chat: &str) -> Result<Self, CallError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| CallError::Channel(format!("telegram client: {}", e.without_url())))?;
        Ok(Self {
            client,
            endpoint: format!("{}/bot{token}/sendMessage", base.trim_end_matches('/')),
            chat: chat.to_owned(),
            sent: Mutex::new(BTreeSet::new()),
        })
    }
}

#[async_trait]
impl HumanChannel for Telegram {
    async fn deliver(&self, notice: &Notice, text: &str) -> Result<(), CallError> {
        let key = notice.key();
        if self
            .sent
            .lock()
            .map_err(|_| CallError::Channel("telegram state poisoned".into()))?
            .contains(&key)
        {
            return Ok(());
        }
        let body = serde_json::json!({
            "chat_id": self.chat,
            "text": text,
            "disable_web_page_preview": true,
        });
        let reply = self
            .client
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| CallError::Channel(format!("telegram: {}", e.without_url())))?;
        let status = reply.status();
        if !status.is_success() {
            // The body names the problem (wrong chat, bot blocked); it never echoes the token.
            let why = reply.text().await.unwrap_or_default();
            let why: String = why.chars().take(200).collect();
            return Err(CallError::Channel(format!("telegram: {status}: {why}")));
        }
        self.sent
            .lock()
            .map_err(|_| CallError::Channel("telegram state poisoned".into()))?
            .insert(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use runo_clock::Level;
    use runo_core::{Clock, Hash, SingerId, TaskId, VerseId};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::Telegram;
    use crate::channel::human::HumanChannel;
    use crate::episodes::{EpisodeId, Lane, Notice};

    /// A one-request-at-a-time fake of the Bot API: answers with `status` and returns each
    /// request's text.
    async fn fake(status: &'static str) -> (String, tokio::sync::mpsc::Receiver<String>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let mut buf = vec![0; 8192];
                let mut got = Vec::new();
                loop {
                    let n = s.read(&mut buf).await.unwrap();
                    got.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&got);
                    if let Some(head_end) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length: ")
                                    .map(str::to_owned)
                            })
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if got.len() >= head_end + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let reply = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{{\"ok\":true}}"
                );
                s.write_all(reply.as_bytes()).await.unwrap();
                tx.send(String::from_utf8_lossy(&got).into_owned())
                    .await
                    .unwrap();
            }
        });
        (base, rx)
    }

    fn notice(level: Level) -> Notice {
        Notice {
            id: EpisodeId {
                lane: Lane {
                    singer: SingerId("desk".into()),
                    task: TaskId("boot-1".into()),
                },
                clock: Clock::Reachability,
                opening: VerseId(Hash([3; 32])),
            },
            level,
            resolved: None,
            reason: "no word for 1 h".into(),
            cleared: false,
        }
    }

    #[tokio::test]
    async fn sends_each_key_once_to_the_chat() {
        let (base, mut rx) = fake("200 OK").await;
        let t = Telegram::at(&base, "123:SECRET", "4242").unwrap();
        t.deliver(&notice(Level::Called), "desk is quiet")
            .await
            .unwrap();
        t.deliver(&notice(Level::Called), "desk is quiet")
            .await
            .unwrap();
        t.deliver(&notice(Level::CalledAgain), "desk is still quiet")
            .await
            .unwrap();
        let first = rx.recv().await.unwrap();
        assert!(
            first.starts_with("POST /bot123:SECRET/sendMessage "),
            "{first}"
        );
        assert!(first.contains(r#""chat_id":"4242""#), "{first}");
        assert!(first.contains(r#""text":"desk is quiet""#), "{first}");
        let second = rx.recv().await.unwrap();
        assert!(
            second.contains("still quiet"),
            "the repeat was dropped: {second}"
        );
    }

    #[tokio::test]
    async fn a_refusal_is_an_error_that_never_shows_the_token() {
        let (base, _rx) = fake("403 Forbidden").await;
        let t = Telegram::at(&base, "123:SECRET", "4242").unwrap();
        let err = t
            .deliver(&notice(Level::Called), "x")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("403"), "{err}");
        assert!(!err.contains("SECRET"), "the token must not leak: {err}");
        let dead = Telegram::at("http://127.0.0.1:9", "123:SECRET", "4242").unwrap();
        let err = dead
            .deliver(&notice(Level::Called), "x")
            .await
            .unwrap_err()
            .to_string();
        assert!(!err.contains("SECRET"), "the token must not leak: {err}");
    }
}
