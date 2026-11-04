//! Diagnostic: Spotify Connect commands to another device, Soloist's say,
//! with the stored playback credential, and the answer printed in full.
//!
//!   cargo run --example soloist_probe -- dj <device id>
//!   cargo run --example soloist_probe -- end-jam <session id> [METHOD path]
//!   cargo run --example soloist_probe -- raw <device id> <command json>
//!   cargo run --example soloist_probe -- state
//!   cargo run --example soloist_probe -- djctx
//!
//! `FROM=<device id>` sends a command as that device. Git Bash rewrites a
//! path argument unless `MSYS_NO_PATHCONV=1` is set.

use librespot_core::{Session, SessionConfig, cache::Cache};

fn main() -> anyhow::Result<()> {
    fastframe_log::Logging::new("spotifast", env!("CARGO_PKG_VERSION"))
        .filter("warn")
        .init()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dirs = spotifast::paths::AppDirs::discover().dirs;
    let cache = Cache::new::<&std::path::Path>(None, None, None, None)?.with_memory_credentials();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let store = spotifast::credentials::Store::new(dirs);
        let loaded = store
            .lease(spotifast::credentials::Slot::Playback)
            .load()
            .await?;
        let Some(spotifast::credentials::Grant::Playback(credentials)) = loaded.grant else {
            anyhow::bail!("Enable playback in Spotifast first");
        };
        let session = Session::new(SessionConfig::default(), Some(cache));
        session.connect(credentials, false).await?;
        println!("connected as {}", session.username());
        let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
        if arg(0) == "local" {
            // Soloist's own channel, as Spotifast reads it: local <port>
            use futures_util::{SinkExt, StreamExt};
            use tokio_tungstenite::tungstenite::Message;
            for (command, wanted) in [
                ("get_state", "playback_state"),
                ("get_queue", "queue_changed"),
            ] {
                let (mut socket, _) =
                    tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}", arg(1))).await?;
                let body = serde_json::json!({"type": "command", "command": command, "limit": 0});
                socket.send(Message::Text(body.to_string().into())).await?;
                let answer = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while let Some(message) = socket.next().await {
                        if let Ok(Message::Text(text)) = message {
                            let value: serde_json::Value =
                                serde_json::from_str(&text).unwrap_or_default();
                            let kind = value["type"].as_str().unwrap_or_default().to_owned();
                            if kind == wanted || kind == "error" {
                                return Some(value);
                            }
                        }
                    }
                    None
                })
                .await;
                let text = format!("{answer:?}");
                println!("{command}: {}", text.chars().take(1500).collect::<String>());
            }
            return anyhow::Ok(());
        }
        if arg(0) == "djctx" {
            // What Spotify's context service says DJ's playlist is.
            let context = session
                .spclient()
                .get_context("spotify:playlist:37i9dQZF1EYkqdzj48dyYq")
                .await?;
            println!(
                "uri={:?} url={:?} pages={}",
                context.uri,
                context.url,
                context.pages.len()
            );
            for (key, value) in &context.metadata {
                println!("  {key} = {}", value.chars().take(160).collect::<String>());
            }
            return anyhow::Ok(());
        }
        if arg(0) == "state" {
            // What Spotify says plays where, through the Web API sign-in.
            let mut token = None;
            for slot in [
                spotifast::credentials::Slot::Personal,
                spotifast::credentials::Slot::Shared,
            ] {
                if let Ok(loaded) = store.lease(slot).load().await
                    && let Some(spotifast::credentials::Grant::Web(stored)) = loaded.grant
                {
                    token = Some(stored.access_token);
                    break;
                }
            }
            let token = token.ok_or_else(|| anyhow::anyhow!("no Web API sign-in"))?;
            let http = reqwest::Client::new();
            for url in [
                "https://api.spotify.com/v1/me/player",
                "https://api.spotify.com/v1/me/player/queue",
            ] {
                let response = http.get(url).bearer_auth(&token).send().await?;
                let status = response.status();
                let body: serde_json::Value = response.json().await.unwrap_or_default();
                println!("{url}: {status}");
                println!(
                    "  device: {} active={}",
                    body["device"]["name"], body["device"]["is_active"]
                );
                println!("  context: {}", body["context"]["uri"]);
                println!(
                    "  shuffle={} smart_shuffle={} playing={}",
                    body["shuffle_state"], body["smart_shuffle"], body["is_playing"]
                );
                println!("  item: {} ({})", body["item"]["name"], body["item"]["uri"]);
                println!("  currently_playing: {}", body["currently_playing"]["name"]);
                if let Some(queue) = body["queue"].as_array() {
                    for item in queue.iter().take(12) {
                        println!(
                            "    queued: {} - {}",
                            item["name"], item["artists"][0]["name"]
                        );
                    }
                }
            }
            return anyhow::Ok(());
        }
        let (to, body) = match arg(0).as_str() {
            "dj" => (
                arg(1),
                spotifast::sonos::dj_play_body("spotify:playlist:37i9dQZF1EYkqdzj48dyYq"),
            ),
            // any command: raw <to> <json>
            "raw" => (arg(1), serde_json::from_str(&arg(2))?),
            "end-jam" => {
                // end-jam <session id> [METHOD path], {id} in the path
                let method = if args.len() > 2 {
                    arg(2)
                } else {
                    "DELETE".into()
                };
                let path = if args.len() > 3 {
                    arg(3).replace("{id}", &arg(1))
                } else {
                    format!("/social-connect/v3/sessions/{}", arg(1))
                };
                let method = reqwest::Method::from_bytes(method.as_bytes())?;
                let body = (method != reqwest::Method::DELETE && method != reqwest::Method::GET)
                    .then_some(b"{}".as_slice());
                let answer = session.spclient().request(&method, &path, None, body).await;
                println!(
                    "{:?}",
                    answer.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                );
                return anyhow::Ok(());
            }
            other => anyhow::bail!("unknown action {other}"),
        };
        // FROM=<device id> sends as another device, the engine's say.
        let from = std::env::var("FROM").unwrap_or_else(|_| session.device_id().to_owned());
        let path = format!("/connect-state/v1/player/command/from/{from}/to/{to}");
        let answer = session
            .spclient()
            .request(
                &reqwest::Method::POST,
                &path,
                None,
                Some(body.to_string().as_bytes()),
            )
            .await;
        println!(
            "{:?}",
            answer.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        );
        anyhow::Ok(())
    })?;
    Ok(())
}
