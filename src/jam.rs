//! Spotify Jam: shared listening sessions through `social-connect`, the
//! private service Spotify's own apps use for them (formerly Group
//! Sessions). There is no Web API for it, so every call goes over the
//! librespot session, like the restricted Connect commands in `sonos.rs`.
//!
//! The service binds a Jam to a Connect device, so each call names this
//! computer's device and the dealer connection it registered on. It then
//! drives that device like any Connect remote, from a virtual
//! `social-connect-<session id>` controller: the playback side lives in the
//! librespot fork's Spirc (a Jam's edits continue the playing stream, and a
//! guest can pause just for themselves).
//!
//! A host plays the Jam on their own device, so their controls stay local. A
//! guest's skips and queued songs go to the Jam through [`command`], as the
//! official client redirects them.

use librespot_core::{Error as CoreError, error::ErrorKind, session::Session};
use reqwest::Method;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};

/// Dealer topic for a Jam's live changes: members joining and leaving, the
/// host's device, and the Jam ending.
pub const SESSION_UPDATE_URI: &str = "social-connect/v2/session_update";

const SHARE_URL: &str = "https://open.spotify.com/socialsession/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JamMember {
    pub id: String,
    pub name: String,
    pub image_url: Option<String>,
    pub is_host: bool,
    pub is_listening: bool,
    pub is_current_user: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JamSession {
    pub session_id: String,
    /// The secret half of the invite link; never logged.
    pub join_token: Option<String>,
    pub is_host: bool,
    pub members: Vec<JamMember>,
    /// Guests may only add songs; playing and skipping is the host's.
    pub queue_only_mode: bool,
    /// The Connect device the Jam plays on, on the host's side.
    pub host_device_id: Option<String>,
    pub host_device_name: Option<String>,
}

impl JamSession {
    /// The link to share. Spotify also returns an internal `hm://` address,
    /// which nothing outside its apps can open; this is the public one built
    /// from the same token.
    pub fn share_url(&self) -> Option<String> {
        self.join_token
            .as_ref()
            .map(|token| format!("{SHARE_URL}{token}"))
    }

    pub fn host(&self) -> Option<&JamMember> {
        self.members.iter().find(|member| member.is_host)
    }
}

/// What a dealer push says about the Jam this device is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JamUpdate {
    Session(JamSession),
    /// The host ended it, or this account left or was removed.
    Ended,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum JamError {
    #[error("That Jam has ended, or the link is not a Jam invite.")]
    NotFound,
    #[error("Spotify refused the Jam request.")]
    Refused,
    #[error("That is not a Spotify Jam link.")]
    BadLink,
    #[error("Jam request failed: {0}")]
    Network(String),
}

impl From<CoreError> for JamError {
    fn from(error: CoreError) -> Self {
        match error.kind {
            ErrorKind::NotFound => Self::NotFound,
            ErrorKind::PermissionDenied | ErrorKind::Unauthenticated => Self::Refused,
            _ => Self::Network(error.to_string()),
        }
    }
}

/// The first of several spellings: REST replies are snake_case and dealer
/// pushes are protobuf JSON in camelCase.
fn pick(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(|found| match found {
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .filter(|text| !text.is_empty())
}

fn pick_bool(value: &Value, keys: &[&str]) -> bool {
    keys.iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Reads a Jam from a REST reply, either bare or wrapped in `session`.
pub fn session_from_value(value: &Value) -> Option<JamSession> {
    let value = value.get("session").unwrap_or(value);
    let session_id = pick(value, &["session_id", "sessionId"])?;
    let owner = pick(value, &["session_owner_id", "sessionOwnerId"]);
    let join_token = pick(value, &["join_session_token", "joinSessionToken"]).or_else(|| {
        pick(value, &["join_session_url", "joinSessionUrl"])
            .and_then(|url| join_token_in(&url))
            .or_else(|| {
                pick(value, &["join_session_uri", "joinSessionUri"])
                    .and_then(|uri| join_token_in(&uri))
            })
    });
    let members = ["session_members", "sessionMembers"]
        .iter()
        .find_map(|key| value.get(*key))
        .and_then(Value::as_array)
        .map(|members| {
            members
                .iter()
                .filter_map(|member| {
                    let id = pick(member, &["id", "username"])?;
                    Some(JamMember {
                        name: pick(member, &["display_name", "displayName", "username"])
                            .unwrap_or_else(|| id.clone()),
                        image_url: pick(
                            member,
                            &["image_url", "imageUrl", "large_image_url", "largeImageUrl"],
                        ),
                        is_host: owner.as_deref() == Some(id.as_str()),
                        is_listening: pick_bool(member, &["is_listening", "isListening"]),
                        is_current_user: pick_bool(member, &["is_current_user", "isCurrentUser"]),
                        id,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let host_device = value
        .get("host_device_info")
        .or_else(|| value.get("hostDeviceInfo"));
    Some(JamSession {
        session_id,
        join_token,
        is_host: pick_bool(value, &["is_session_owner", "isSessionOwner"]),
        members,
        queue_only_mode: pick_bool(value, &["queue_only_mode", "queueOnlyMode"]),
        host_device_id: pick(
            value,
            &[
                "host_active_device_id",
                "hostActiveDeviceId",
                "host_device_id",
            ],
        ),
        host_device_name: host_device.and_then(|info| pick(info, &["device_name", "deviceName"])),
    })
}

/// Reads a `social-connect/v2/session_update` push.
pub fn update_from_value(value: &Value) -> Option<JamUpdate> {
    let reason = pick(value, &["reason"]).unwrap_or_default();
    if matches!(
        reason.as_str(),
        "SESSION_DELETED" | "YOU_LEFT" | "YOU_WERE_KICKED"
    ) {
        return Some(JamUpdate::Ended);
    }
    session_from_value(value.get("session")?).map(JamUpdate::Session)
}

/// The join token in a share link (`open.spotify.com/socialsession/…` or
/// `open.spotify.com/jam/…`), a `spotify:socialsession:` URI, or a bare
/// token. A `spotify.link` short link is not one; [`resolve_join_token`]
/// follows it first.
pub fn join_token_in(text: &str) -> Option<String> {
    let text = text.trim();
    let marked = candidates(text).find_map(|rest| {
        rest.split(['/', '?', '#', '&', '%', '"', '\'', '<', ' ', ','])
            .next()
            .filter(|token| is_token(token))
    });
    if let Some(token) = marked {
        return Some(token.to_owned());
    }
    let token = if text.contains("/sessions/join/") {
        // the join path of the internal address
        text.rsplit('/').next()?
    } else if text.contains(['/', ':', '.', ' ', '"']) {
        return None;
    } else {
        text
    };
    is_token(token).then(|| token.to_owned())
}

fn is_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// What follows each part of the text that names a Jam invite. A redirect
/// page names `socialsession` in other places too (`"app_destination":
/// "socialsession"`), so every mention is a candidate, in order.
fn candidates(text: &str) -> impl Iterator<Item = &str> {
    let social = text
        .match_indices("socialsession")
        .filter_map(|(at, marker)| {
            let rest = &text[at + marker.len()..];
            rest.strip_prefix("%3A")
                .or_else(|| rest.strip_prefix("%3a"))
                .or_else(|| rest.strip_prefix("%2F"))
                .or_else(|| rest.strip_prefix("%2f"))
                .or_else(|| rest.strip_prefix([':', '/']))
        });
    let jam = [
        "/jam/",
        ":jam:",
        "%3Ajam%3A",
        "%3ajam%3a",
        "%2Fjam%2F",
        "%2fjam%2f",
    ]
    .into_iter()
    .flat_map(move |marker| {
        text.match_indices(marker)
            .map(move |(at, _)| &text[at + marker.len()..])
    });
    social.chain(jam)
}

/// A Spotify web link that may lead to a Jam once followed: a `spotify.link`
/// short link, or another Spotify address that redirects to one.
fn is_followable(text: &str) -> bool {
    let text = text.trim();
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    host == "spotify.link"
        || host == "spotify.app.link"
        || host == "spotify.com"
        || host.ends_with(".spotify.com")
}

/// The outline of a link for the log: its host and path, with anything long
/// enough to be a token hidden.
fn link_shape(text: &str) -> String {
    let text = text.trim();
    let (scheme, rest) = text.split_once("://").unwrap_or(("", text));
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let mut parts = rest.split('/');
    let host = parts.next().unwrap_or_default();
    let path: Vec<&str> = parts
        .map(|part| if part.len() > 8 { "<…>" } else { part })
        .collect();
    format!("{scheme}://{host}/{}", path.join("/"))
}

/// The join token for anything a person might paste. A `spotify.link` short
/// link (what Spotify's share sheet hands out) is followed, without
/// credentials, to the Jam address it redirects to.
pub async fn resolve_join_token(client: &reqwest::Client, link: &str) -> Result<String, JamError> {
    if let Some(token) = join_token_in(link) {
        return Ok(token);
    }
    if !is_followable(link) {
        log::warn!("not a Jam link: {}", link_shape(link));
        return Err(JamError::BadLink);
    }
    let url = if link.trim().starts_with("http") {
        link.trim().to_owned()
    } else {
        format!("https://{}", link.trim())
    };
    // Short links answer a desktop browser with a plain redirect to the
    // web address; other clients may get a page that only scripts redirect.
    let response = client
        .get(&url)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/134.0 Safari/537.36",
        )
        .send()
        .await
        .map_err(|error| JamError::Network(error.without_url().to_string()))?;
    let landed = response.url().to_string();
    if let Some(token) = join_token_in(&landed) {
        return Ok(token);
    }
    // some links land on a page that names the Jam instead, often inside
    // escaped JSON or a percent-encoded fallback address
    let body = response.text().await.unwrap_or_default();
    let page = unescape_page(&body);
    join_token_in(&page).ok_or_else(|| {
        log::warn!(
            "no Jam behind {} (landed on {}; page of {} bytes, mentions socialsession: {}, jam: {})",
            link_shape(link),
            link_shape(&landed),
            body.len(),
            page.contains("socialsession"),
            page.contains("/jam/") || page.contains(":jam:"),
        );
        JamError::BadLink
    })
}

/// A page's text with the escapes a redirect page hides an address behind
/// undone: JSON's `\/`, HTML's slash entities, and percent-encoding.
fn unescape_page(body: &str) -> String {
    let body = body
        .replace("\\/", "/")
        .replace("&#x2F;", "/")
        .replace("&#x2f;", "/")
        .replace("&#47;", "/")
        .replace("&amp;", "&");
    percent_encoding::percent_decode_str(&body)
        .decode_utf8_lossy()
        .into_owned()
}

/// One social-connect call, made the way the official client makes it: as
/// this Connect device, on the dealer connection it registered on. librespot
/// adds the first-party bearer and client token; a token for a registered
/// Web API app is refused here whatever its scopes.
async fn call(session: &Session, method: Method, endpoint: &str) -> Result<Value, JamError> {
    call_with_body(session, method, endpoint, None).await
}

async fn call_with_body(
    session: &Session,
    method: Method,
    endpoint: &str,
    body: Option<&Value>,
) -> Result<Value, JamError> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(connection) = HeaderValue::from_str(&session.connection_id())
        && !connection.is_empty()
    {
        headers.insert("Spotify-Connection-Id", connection);
    }
    // librespot always sets Content-Length, so a bodiless POST is not
    // refused with 411 by the frontend in front of spclient.
    let body = body.map(Value::to_string);
    let bytes = session
        .spclient()
        .request(
            &method,
            endpoint,
            Some(headers),
            body.as_deref().map(str::as_bytes),
        )
        .await
        .map_err(|error| {
            let error = JamError::from(error);
            // not being in a Jam is answered with 404, and is no failure
            if error == JamError::NotFound {
                log::debug!("social-connect {method} {}: not found", redact(endpoint));
            } else {
                log::warn!(
                    "social-connect {method} {} failed: {error}",
                    redact(endpoint)
                );
            }
            error
        })?;
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|error| JamError::Network(error.to_string()))
}

/// An endpoint for the log, without the join token it may carry.
fn redact(endpoint: &str) -> String {
    const JOIN: &str = "/sessions/join/";
    match endpoint.find(JOIN) {
        Some(at) => {
            let start = at + JOIN.len();
            let end = endpoint[start..]
                .find('?')
                .map_or(endpoint.len(), |offset| start + offset);
            format!("{}<token>{}", &endpoint[..start], &endpoint[end..])
        }
        None => endpoint.to_owned(),
    }
}

fn parsed(value: &Value) -> Result<JamSession, JamError> {
    session_from_value(value).ok_or_else(|| JamError::Network("no Jam in Spotify's reply".into()))
}

fn path_safe(id: &str) -> Result<&str, JamError> {
    let safe = !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if safe { Ok(id) } else { Err(JamError::BadLink) }
}

/// The Jam this device is in, if any.
pub async fn current(session: &Session) -> Result<Option<JamSession>, JamError> {
    let endpoint = format!(
        "/social-connect/v2/sessions/current?local_device_id={}",
        session.device_id()
    );
    match call(session, Method::GET, &endpoint).await {
        Ok(value) => Ok(session_from_value(&value)),
        Err(JamError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Starts a Jam on this device, or returns the one it already hosts. The Jam
/// takes over whatever this device is playing.
pub async fn start(session: &Session) -> Result<JamSession, JamError> {
    let endpoint = format!(
        "/social-connect/v2/sessions/current_or_new?activate=true&local_device_id={}&type=REMOTE",
        session.device_id()
    );
    parsed(&call(session, Method::GET, &endpoint).await?)
}

/// Joins someone's Jam by its invite token. social-connect then transfers the
/// Jam to this device, so it plays here.
pub async fn join(session: &Session, token: &str) -> Result<JamSession, JamError> {
    let endpoint = format!(
        "/social-connect/v2/sessions/join/{}?playback_control=listen_and_control&join_type=deeplinking&local_device_id={}",
        path_safe(token)?,
        session.device_id()
    );
    parsed(&call(session, Method::POST, &endpoint).await?)
}

/// Leaves the Jam this device is in, as a guest.
///
/// Spotify refuses the old device-wide `v2/sessions/leave` with 405, so
/// the session's own leave path goes first; the others are tried in turn.
pub async fn leave(session: &Session, session_id: &str) -> Result<(), JamError> {
    let id = path_safe(session_id)?;
    let attempts = [
        format!("/social-connect/v3/sessions/{id}/leave"),
        format!("/social-connect/v2/sessions/{id}/leave"),
        format!(
            "/social-connect/v2/sessions/leave?local_device_id={}",
            session.device_id()
        ),
    ];
    let mut last = JamError::NotFound;
    for endpoint in &attempts {
        match call(session, Method::POST, endpoint).await {
            Ok(_) => return Ok(()),
            Err(error) => last = error,
        }
    }
    match last {
        // nothing left to leave
        JamError::NotFound => Ok(()),
        error => Err(error),
    }
}

/// Ends a Jam this account hosts, for everyone in it.
pub async fn end(session: &Session, session_id: &str) -> Result<(), JamError> {
    let endpoint = format!("/social-connect/v3/sessions/{}", path_safe(session_id)?);
    match call(session, Method::DELETE, &endpoint).await {
        Ok(_) | Err(JamError::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Removes a member from a Jam this account hosts.
pub async fn remove_member(
    session: &Session,
    session_id: &str,
    member_id: &str,
) -> Result<Option<JamSession>, JamError> {
    let endpoint = format!(
        "/social-connect/v3/sessions/{}/member/{}/kick",
        path_safe(session_id)?,
        path_safe(member_id)?
    );
    Ok(session_from_value(
        &call(session, Method::POST, &endpoint).await?,
    ))
}

/// Lets guests control playback (`false`), or only add songs (`true`).
pub async fn set_queue_only(session: &Session, queue_only: bool) -> Result<(), JamError> {
    let mode = if queue_only { "enabled" } else { "disabled" };
    let endpoint = format!("/social-connect/v2/sessions/current/queue_only_mode/{mode}");
    call(session, Method::PUT, &endpoint).await.map(drop)
}

/// A guest's player command, sent to the Jam instead of their own player,
/// as the official client redirects it. `command` is the Connect command
/// JSON, such as `{"endpoint": "skip_next"}`.
pub async fn command(
    session: &Session,
    session_id: &str,
    mut command: Value,
) -> Result<(), JamError> {
    let device = session.device_id().to_owned();
    if let Some(fields) = command.as_object_mut() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default();
        fields.entry("logging_params").or_insert(json!({
            "command_initiated_time": now,
            "device_identifier": device,
        }));
    }
    let endpoint = format!(
        "/social-connect/v2/sessions/{}/commands/from/{device}",
        path_safe(session_id)?
    );
    call_with_body(
        session,
        Method::POST,
        &endpoint,
        Some(&json!({ "command": command })),
    )
    .await
    .map(drop)
}

/// Connect command bodies for a guest's controls.
pub mod commands {
    use serde_json::{Value, json};

    pub fn skip_next() -> Value {
        json!({ "endpoint": "skip_next" })
    }

    pub fn skip_prev() -> Value {
        json!({ "endpoint": "skip_prev" })
    }

    pub fn add_to_queue(uri: &str) -> Value {
        json!({ "endpoint": "add_to_queue", "track": { "uri": uri } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_rest_reply() {
        let value = json!({
            "session_id": "abc",
            "join_session_token": "tok123",
            "join_session_url": "hm://social-connect/v2/sessions/join/tok123",
            "session_owner_id": "me",
            "is_session_owner": true,
            "queue_only_mode": true,
            "host_device_info": {"device_name": "Spotifast"},
            "session_members": [
                {"id": "me", "display_name": "Me", "is_current_user": true},
                {"id": "pal", "username": "pal", "is_listening": true, "image_url": "https://i/p"}
            ]
        });
        let jam = session_from_value(&value).unwrap();
        assert_eq!(jam.session_id, "abc");
        assert!(jam.is_host);
        assert!(jam.queue_only_mode);
        assert_eq!(
            jam.share_url().as_deref(),
            Some("https://open.spotify.com/socialsession/tok123")
        );
        assert_eq!(jam.host_device_name.as_deref(), Some("Spotifast"));
        assert!(jam.members[0].is_host && jam.members[0].is_current_user);
        assert_eq!(jam.members[1].name, "pal");
        assert!(jam.members[1].is_listening);
        assert_eq!(jam.members[1].image_url.as_deref(), Some("https://i/p"));
        assert_eq!(jam.host().map(|m| m.id.as_str()), Some("me"));
    }

    #[test]
    fn a_share_link_is_built_from_the_internal_address_when_no_token_is_given() {
        let value = json!({
            "session_id": "abc",
            "join_session_url": "hm://social-connect/v2/sessions/join/4Oxc"
        });
        let jam = session_from_value(&value).unwrap();
        assert!(!jam.is_host);
        assert_eq!(
            jam.share_url().as_deref(),
            Some("https://open.spotify.com/socialsession/4Oxc")
        );
    }

    #[test]
    fn reads_a_dealer_push() {
        let push = json!({
            "reason": "USER_JOINED",
            "session": {
                "sessionId": "s1",
                "joinSessionToken": "t",
                "sessionOwnerId": "h",
                "isSessionOwner": false,
                "sessionMembers": [{"id": "h", "displayName": "Host"}],
                "hostActiveDeviceId": "dev"
            }
        });
        let Some(JamUpdate::Session(jam)) = update_from_value(&push) else {
            panic!("not a session");
        };
        assert_eq!(jam.session_id, "s1");
        assert_eq!(jam.members[0].name, "Host");
        assert!(jam.members[0].is_host);
        assert_eq!(jam.host_device_id.as_deref(), Some("dev"));
    }

    #[test]
    fn an_ended_jam_is_reported_whatever_the_session_says() {
        for reason in ["SESSION_DELETED", "YOU_LEFT", "YOU_WERE_KICKED"] {
            let push = json!({"reason": reason, "session": {"sessionId": "s1"}});
            assert_eq!(update_from_value(&push), Some(JamUpdate::Ended), "{reason}");
        }
        assert_eq!(update_from_value(&json!({"reason": "USER_LEFT"})), None);
    }

    #[test]
    fn finds_the_join_token_in_every_link_shape() {
        for link in [
            "https://open.spotify.com/socialsession/AbC123?si=x",
            "https://open.spotify.com/intl-de/socialsession/AbC123",
            "open.spotify.com/socialsession/AbC123/",
            "spotify:socialsession:AbC123",
            "https://x/?u=spotify%3Asocialsession%3AAbC123&y",
            "hm://social-connect/v2/sessions/join/AbC123",
            "https://open.spotify.com/jam/AbC123?si=x",
            "https://open.spotify.com/intl-fr/jam/AbC123",
            "spotify:jam:AbC123",
            "<meta content=\"spotify:socialsession:AbC123\">",
            "  AbC123 ",
        ] {
            assert_eq!(join_token_in(link).as_deref(), Some("AbC123"), "{link}");
        }
    }

    #[test]
    fn rejects_what_is_not_a_jam_link() {
        for link in [
            "",
            "not a link!",
            "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC",
            "https://spotify.link/a1b2c3",
            "spotify:socialsession:",
        ] {
            assert_eq!(join_token_in(link), None, "{link}");
        }
        assert!(is_followable("https://spotify.link/a1b2c3"));
        assert!(is_followable("spotify.link/a1b2c3"));
        assert!(is_followable("https://open.spotify.com/something/else"));
        assert!(!is_followable("https://example.com/socialsession"));
        assert!(!is_followable("https://notspotify.com/x"));
    }

    #[test]
    fn a_redirect_page_gives_up_its_jam() {
        for page in [
            r#"{"$desktop_url":"https:\/\/open.spotify.com\/socialsession\/AbC123"}"#,
            "<a href=\"https:&#x2F;&#x2F;open.spotify.com&#x2F;socialsession&#x2F;AbC123\">",
            "?$fallback_url=https%3A%2F%2Fopen.spotify.com%2Fsocialsession%2FAbC123%3Fsi%3Dx",
            r#"window.location = "spotify:\/\/socialsession\/AbC123";"#,
            r#"{"app_destination":"socialsession","$desktop_url":"https:\/\/open.spotify.com\/socialsession\/AbC123"}"#,
        ] {
            assert_eq!(
                join_token_in(&unescape_page(page)).as_deref(),
                Some("AbC123"),
                "{page}"
            );
        }
    }

    #[test]
    fn a_rejected_link_is_logged_without_its_token() {
        assert_eq!(
            link_shape("https://open.spotify.com/xyz/AbCdEfGh123?si=secret"),
            "https://open.spotify.com/xyz/<…>"
        );
    }

    #[test]
    fn logs_never_carry_the_join_token() {
        assert_eq!(
            redact("/social-connect/v2/sessions/join/secret?local_device_id=d"),
            "/social-connect/v2/sessions/join/<token>?local_device_id=d"
        );
        assert_eq!(
            redact("/social-connect/v2/sessions/current"),
            "/social-connect/v2/sessions/current"
        );
    }

    #[test]
    fn ids_cannot_escape_their_path_segment() {
        assert!(path_safe("member/../kick").is_err());
        assert!(path_safe("").is_err());
        assert_eq!(path_safe("1ce14fef83e5a794"), Ok("1ce14fef83e5a794"));
    }

    #[test]
    fn errors_say_what_happened() {
        assert_eq!(
            JamError::from(CoreError::not_found("gone")),
            JamError::NotFound
        );
        assert_eq!(
            JamError::from(CoreError::permission_denied("rbac")),
            JamError::Refused
        );
    }
}
