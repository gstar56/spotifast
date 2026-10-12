//! The Spotify Jam popup, opened from the player bar: start or join a Jam,
//! share it, see who is in it, and leave or end it.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Rect, pos2, vec2};

use crate::app::App;
use crate::app::JamBusyKind;
use crate::i18n::gettext;
use crate::jam::JamSession;
use crate::model::Action;
use crate::theme::{self, Icon};

pub const BUTTON_RECT_ID: &str = "jam-button-rect";

pub fn popup(app: &mut App, ctx: &egui::Context) {
    if !app.show_jam {
        return;
    }
    let palette = app.palette;
    let button = ctx
        .data(|data| data.get_temp::<Rect>(egui::Id::new(BUTTON_RECT_ID)))
        .unwrap_or_else(|| Rect::from_min_size(pos2(400.0, 400.0), vec2(0.0, 0.0)));
    let width = 320.0;
    let position = pos2(
        (button.right() - width).max(8.0),
        (button.top() - 12.0).max(8.0),
    );
    let area = egui::Area::new(egui::Id::new("jam-popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(position)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .show(ctx, |ui| {
            super::widgets::menu_frame(&palette).show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 8.0;
                match app.jam.clone() {
                    Some(jam) => in_jam(app, ui, &jam),
                    None => not_in_jam(app, ui),
                }
            });
        });
    let popup_rect = area.response.rect;
    let clicked_outside = ctx.input(|input| {
        input.pointer.any_pressed()
            && input
                .pointer
                .interact_pos()
                .is_some_and(|pos| !popup_rect.contains(pos) && !button.contains(pos))
    });
    if clicked_outside {
        app.show_jam = false;
    }
}

fn header(app: &App, ui: &mut egui::Ui, title: &str) {
    let palette = app.palette;
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        theme::text(ui, title, theme::bold(16.0), palette.text);
        if app.jam_busy().is_some() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                theme::spinner(ui, 16.0, palette.accent);
            });
        }
    });
}

fn note(app: &App, ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .font(theme::regular(12.0))
                    .color(app.palette.secondary),
            )
            .wrap()
            .selectable(false),
        );
    });
}

fn not_in_jam(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let locale = app.locale;
    header(app, ui, &gettext(locale, "Jam"));
    if !app.jam_available() {
        note(
            app,
            ui,
            &gettext(
                locale,
                "A Jam plays on this computer. Set up playback here to start or join one.",
            ),
        );
        return;
    }
    let busy = app.jam_busy();
    note(
        app,
        ui,
        &gettext(
            locale,
            "Listen together: friends join with a link and add songs to the queue.",
        ),
    );
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        ui.add_enabled_ui(busy.is_none(), |ui| {
            if theme::pill_button(ui, &palette, &gettext(locale, "Start a Jam"), true).clicked() {
                app.actions.push(Action::StartJam);
            }
        });
    });
    super::widgets::menu_separator(ui, &palette);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        theme::text(
            ui,
            gettext(locale, "Join a Jam").as_ref(),
            theme::semibold(14.0),
            palette.text,
        );
    });
    let mut submit = false;
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let field = Frame::new()
            .fill(palette.surface)
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(10, 7))
            .show(ui, |ui| {
                ui.set_width(190.0);
                super::widgets::text_edit(
                    ui,
                    locale,
                    egui::TextEdit::singleline(&mut app.jam_link)
                        .id(egui::Id::new("jam-link"))
                        .hint_text(
                            egui::RichText::new(gettext(locale, "Paste a Jam link"))
                                .color(palette.dim),
                        )
                        .font(theme::regular(13.0))
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY),
                )
            })
            .inner;
        if field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            submit = true;
        }
        let can_join = busy.is_none() && !app.jam_link.trim().is_empty();
        ui.add_enabled_ui(can_join, |ui| {
            if theme::pill_button(ui, &palette, &gettext(locale, "Join"), false).clicked() {
                submit = true;
            }
        });
    });
    if submit && busy.is_none() && !app.jam_link.trim().is_empty() {
        app.actions.push(Action::JoinJam(app.jam_link.clone()));
    }
    if busy == Some(JamBusyKind::Joining) {
        note(app, ui, &gettext(locale, "Joining…"));
    }
}

fn in_jam(app: &mut App, ui: &mut egui::Ui, jam: &JamSession) {
    let palette = app.palette;
    let locale = app.locale;
    let host = jam.host().map(|host| host.name.clone());
    let title = match (&host, jam.is_host) {
        (_, true) => gettext(locale, "Your Jam").into_owned(),
        // Translators: {name} is the name of the person hosting the Jam.
        (Some(host), false) => gettext(locale, "{name}'s Jam").replace("{name}", host),
        (None, false) => gettext(locale, "Jam").into_owned(),
    };
    header(app, ui, &title);
    if jam.is_host {
        note(
            app,
            ui,
            &gettext(locale, "Share the link so friends can join and add songs."),
        );
    } else {
        note(
            app,
            ui,
            &gettext(
                locale,
                "Playing here in sync. Pausing pauses only you; play catches back up.",
            ),
        );
    }

    if jam.share_url().is_some() {
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            if theme::soft_button(
                ui,
                &palette,
                Some(Icon::Copy),
                &gettext(locale, "Copy invite link"),
                false,
            )
            .clicked()
            {
                app.actions.push(Action::CopyJamLink);
            }
        });
    }

    if jam.is_host {
        let mut guests_control = !jam.queue_only_mode;
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let label = gettext(locale, "Guests can play and skip");
            theme::text(ui, label.as_ref(), theme::regular(13.0), palette.text);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                if super::widgets::switch(ui, &palette, &label, &mut guests_control).changed() {
                    app.actions.push(Action::SetJamQueueOnly(!guests_control));
                }
            });
        });
    }

    super::widgets::menu_separator(ui, &palette);
    theme::text(
        ui,
        crate::i18n::ngettext(
            locale,
            // Translators: {count} is the number of people in the Jam.
            "{count} person",
            "{count} people",
            jam.members.len() as u32,
        )
        .replace("{count}", &jam.members.len().to_string()),
        theme::semibold(13.0),
        palette.secondary,
    );
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for member in &jam.members {
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    super::widgets::cover(
                        ui,
                        &palette,
                        member.image_url.as_deref(),
                        28.0,
                        14.0,
                        Icon::User,
                    );
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let name = if member.is_current_user {
                            // Translators: {name} is your own name in a list of Jam members.
                            gettext(locale, "{name} (you)").replace("{name}", &member.name)
                        } else {
                            member.name.clone()
                        };
                        theme::text(ui, name, theme::medium(13.0), palette.text);
                        if member.is_host {
                            theme::text(
                                ui,
                                gettext(locale, "Host").as_ref(),
                                theme::regular(11.0),
                                palette.secondary,
                            );
                        }
                    });
                    if jam.is_host && !member.is_host && !member.is_current_user {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(6.0);
                            if theme::icon_button(
                                ui,
                                Icon::CircleX,
                                15.0,
                                palette.secondary,
                                palette.danger,
                                &gettext(locale, "Remove from Jam"),
                            )
                            .clicked()
                            {
                                app.actions.push(Action::RemoveJamMember(member.id.clone()));
                            }
                        });
                    }
                });
            }
        });

    super::widgets::menu_separator(ui, &palette);
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let (label, action) = if jam.is_host {
            (gettext(locale, "End Jam"), Action::EndJam)
        } else {
            (gettext(locale, "Leave Jam"), Action::LeaveJam)
        };
        if theme::pill_button(ui, &palette, &label, false).clicked() {
            app.actions.push(action);
        }
    });
}
