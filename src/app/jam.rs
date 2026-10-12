//! Spotify Jam: starting, joining and leaving a shared listening session,
//! and sending a guest's controls to it.
//!
//! The Jam plays on this computer either way. A host's controls stay on
//! the local player, which is the Jam's player. A guest's skips and queued
//! songs go to the Jam, and their pause is theirs alone: resuming catches
//! back up with the Jam, which kept going (the librespot fork's Jam hold).

use super::*;
use crate::backend::{JamEvent, JamRequest};
use crate::jam::{JamError, JamSession};

/// How long a Jam request may be shown as under way. Spotify answers in
/// well under a second; past this the controls come back regardless.
const JAM_BUSY_FOR: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JamBusyKind {
    Starting,
    Joining,
    /// Leaving or ending: the Jam is already shown as gone.
    Leaving,
    Updating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JamBusy {
    pub kind: JamBusyKind,
    pub at: Instant,
}

impl App {
    /// Whether this device can be in a Jam: Spotify binds one to a Connect
    /// device, so it needs local playback.
    pub fn jam_available(&self) -> bool {
        self.local_ready
    }

    pub fn jam_busy(&self) -> Option<JamBusyKind> {
        self.jam_busy
            .filter(|busy| busy.at.elapsed() < JAM_BUSY_FOR)
            .map(|busy| busy.kind)
    }

    /// The Jam this device is a guest in, whose controls go to the Jam.
    pub fn jam_guest_session(&self) -> Option<&str> {
        self.jam
            .as_ref()
            .filter(|jam| !jam.is_host && self.local_ready)
            .map(|jam| jam.session_id.as_str())
    }

    fn send_jam(&mut self, kind: JamBusyKind, request: JamRequest) {
        self.jam_busy = Some(JamBusy {
            kind,
            at: Instant::now(),
        });
        self.backend.send(Command::Jam(request));
    }

    pub(super) fn apply_jam_action(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::ToggleJamPopup => {
                self.show_jam = !self.show_jam;
                if self.show_jam {
                    self.show_devices = false;
                    if self.jam_available() && self.jam_busy().is_none() {
                        self.backend.send(Command::Jam(JamRequest::Refresh));
                    }
                }
            }
            Action::StartJam if self.jam_available() => {
                self.send_jam(JamBusyKind::Starting, JamRequest::Start);
            }
            Action::JoinJam(link) if self.jam_available() => {
                let link = link.trim().to_owned();
                if link.is_empty() {
                    return;
                }
                self.send_jam(JamBusyKind::Joining, JamRequest::Join(link));
            }
            Action::LeaveJam => {
                if let Some(jam) = self.jam.take() {
                    self.jam_hold = false;
                    self.send_jam(
                        JamBusyKind::Leaving,
                        JamRequest::Leave {
                            session_id: jam.session_id,
                        },
                    );
                }
            }
            Action::EndJam => {
                if let Some(jam) = self.jam.take() {
                    self.jam_hold = false;
                    self.send_jam(
                        JamBusyKind::Leaving,
                        JamRequest::End {
                            session_id: jam.session_id,
                        },
                    );
                }
            }
            Action::RemoveJamMember(member_id) => {
                let Some(jam) = self.jam.as_mut().filter(|jam| jam.is_host) else {
                    return;
                };
                let session_id = jam.session_id.clone();
                jam.members.retain(|member| member.id != member_id);
                self.send_jam(
                    JamBusyKind::Updating,
                    JamRequest::RemoveMember {
                        session_id,
                        member_id,
                    },
                );
            }
            Action::SetJamQueueOnly(queue_only) => {
                let Some(jam) = self.jam.as_mut().filter(|jam| jam.is_host) else {
                    return;
                };
                jam.queue_only_mode = queue_only;
                self.send_jam(JamBusyKind::Updating, JamRequest::QueueOnly(queue_only));
            }
            Action::CopyJamLink => {
                if let Some(url) = self.jam.as_ref().and_then(JamSession::share_url) {
                    ctx.copy_text(url);
                    self.toast(gettext(self.locale, "Jam link copied"));
                }
            }
            _ => {}
        }
    }

    pub(super) fn handle_jam(&mut self, event: JamEvent) {
        let leaving = self.jam_busy() == Some(JamBusyKind::Leaving);
        match event {
            // A push from before leaving still names the Jam just left.
            JamEvent::Pushed(Some(_)) if leaving => {}
            JamEvent::Pushed(session) => self.show_jam_session(session),
            JamEvent::Replied(Some(_)) if leaving => {}
            JamEvent::Replied(session) => {
                let was = self.jam_busy();
                self.jam_busy = None;
                if was == Some(JamBusyKind::Joining)
                    && let Some(jam) = &session
                {
                    self.jam_link.clear();
                    let host = jam.host().map(|host| host.name.clone()).unwrap_or_default();
                    self.toast(if host.is_empty() {
                        gettext(self.locale, "Joined the Jam").into_owned()
                    } else {
                        // Translators: {name} is the name of the person hosting the Jam.
                        gettext(self.locale, "Joined {name}'s Jam").replace("{name}", &host)
                    });
                }
                self.show_jam_session(session);
            }
            JamEvent::Failed(error) => {
                let was = self.jam_busy();
                self.jam_busy = None;
                if was == Some(JamBusyKind::Leaving) || was == Some(JamBusyKind::Updating) {
                    // what is shown was a guess; ask what is true
                    self.backend.send(Command::Jam(JamRequest::Refresh));
                }
                self.toast_error(self.jam_error_text(&error));
            }
        }
    }

    fn show_jam_session(&mut self, session: Option<JamSession>) {
        if session.is_none() {
            self.jam_hold = false;
        }
        self.jam = session;
    }

    fn jam_error_text(&self, error: &JamError) -> String {
        match error {
            JamError::NotFound => gettext(
                self.locale,
                "That Jam has ended, or the link is not a Jam invite",
            )
            .into_owned(),
            JamError::BadLink => {
                gettext(self.locale, "That is not a Spotify Jam link").into_owned()
            }
            JamError::Refused => {
                gettext(self.locale, "Spotify refused the Jam request").into_owned()
            }
            JamError::Network(message) => {
                // Translators: {error} is an error message from Spotify or the network.
                gettext(self.locale, "Jam: {error}").replace("{error}", message)
            }
        }
    }

    /// Local playback went away, and with it this device's place in a Jam.
    pub(super) fn forget_jam(&mut self) {
        self.jam = None;
        self.jam_busy = None;
        self.jam_hold = false;
        self.show_jam = false;
    }

    /// Sends a guest's control to the Jam. `true` when this device is a
    /// guest and the command went there.
    pub(super) fn jam_guest_command(&mut self, command: serde_json::Value) -> bool {
        let Some(session_id) = self.jam_guest_session().map(str::to_owned) else {
            return false;
        };
        self.backend.send(Command::Jam(JamRequest::Command {
            session_id,
            command,
        }));
        true
    }

    /// A guest's play and pause: theirs alone. Resuming catches back up with
    /// the Jam where it is now.
    pub(super) fn jam_guest_toggle(&mut self) -> bool {
        if self.jam_guest_session().is_none() {
            return false;
        }
        let hold = self.local.playback == Playback::Playing;
        self.jam_hold = hold;
        self.backend.player(PlayerCommand::JamHold(hold));
        true
    }
}
