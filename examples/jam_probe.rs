//! Diagnostic: Spotify Jam (social-connect) calls with the stored playback
//! credential. `current` only reads; the others change the account's Jam.
//!
//!   cargo run --example jam_probe -- current
//!   cargo run --example jam_probe -- start
//!   cargo run --example jam_probe -- end <session id>
//!   cargo run --example jam_probe -- join <link or token>
//!   cargo run --example jam_probe -- leave
//!
//! This session registers no Connect device, so a Jam started or joined
//! here has nowhere to play; use it to check the requests, not to listen.

use librespot_core::{Session, SessionConfig, cache::Cache};
use spotifast::jam;

fn main() -> anyhow::Result<()> {
    fastframe_log::Logging::new("spotifast", env!("CARGO_PKG_VERSION"))
        .filter("warn")
        .init()?;
    let mut args = std::env::args().skip(1);
    let action = args.next().unwrap_or_else(|| "current".into());
    let argument = args.next();

    let dirs = spotifast::paths::AppDirs::discover();
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
        if let Some(warning) = loaded.warning {
            eprintln!("{warning}");
        }
        let Some(spotifast::credentials::Grant::Playback(credentials)) = loaded.grant else {
            anyhow::bail!("Enable playback in Spotifast first");
        };
        let session = Session::new(SessionConfig::default(), Some(cache));
        session.connect(credentials, false).await?;
        println!("connected as {}", session.username());

        let required = || {
            argument
                .clone()
                .ok_or_else(|| anyhow::anyhow!("{action} needs an argument"))
        };
        match action.as_str() {
            "current" => println!("{:#?}", jam::current(&session).await),
            "start" => println!("{:#?}", jam::start(&session).await),
            "end" => println!("{:?}", jam::end(&session, &required()?).await),
            "join" => {
                let client = reqwest::Client::new();
                let token = jam::resolve_join_token(&client, &required()?).await?;
                println!("{:#?}", jam::join(&session, &token).await);
            }
            "leave" => println!("{:?}", jam::leave(&session).await),
            other => anyhow::bail!("unknown action {other}"),
        }
        anyhow::Ok(())
    })?;
    Ok(())
}
