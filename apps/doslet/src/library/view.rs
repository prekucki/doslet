//! Library presentation: a sidebar, a grid of cover cards, game pages and global settings.
use super::{Filter, Game, GameEdit, GameTab, GlobalEdit, Library, Message, VfsViewMode};
use crate::ui::{ACCENT, EDGE, FOREGROUND, MUTED};
use doslet_config::{CpuMode, HardwarePreset, KeyboardMode, MouseMode, VideoFilter};
use iced::widget::{
    button, center, checkbox, column, container, hover, image, pick_list, responsive, row,
    scrollable, slider, space, stack, svg, text, text_input,
};
use iced::{
    Alignment, Background, Border, Color, ContentFit, Element, Fill, Font, Shadow, Theme, Vector,
    gradient,
};
use std::f32::consts::{FRAC_PI_2, PI};

const BACKGROUND: Color = Color::from_rgb8(13, 14, 17);
const SIDEBAR: Color = Color::from_rgb8(18, 19, 23);
const SURFACE: Color = Color::from_rgb8(26, 28, 33);
const INK: Color = Color::from_rgb8(17, 18, 21);
const ACTIVE: Color = Color::from_rgb8(120, 214, 150);
const WARNING: Color = Color::from_rgb8(228, 193, 133);
const LOGO: &[u8] = include_bytes!("../../../../assets/org.doslet.Doslet.svg");
const PLAY: &[u8] = include_bytes!("../ui/icons/play.svg");
const SIDEBAR_WIDTH: f32 = 236.0;
/// Below this window width the sidebar gives way to filter chips.
const COMPACT_WIDTH: f32 = 900.0;
const PAGE_PADDING: f32 = 36.0;
const CARD_SPACING: f32 = 24.0;
const CARD_MAX_WIDTH: f32 = 420.0;
const CARD_PADDING: f32 = 10.0;

fn weight(weight: iced::font::Weight) -> Font {
    Font {
        weight,
        ..Font::DEFAULT
    }
}
fn bold() -> Font {
    weight(iced::font::Weight::Bold)
}
fn semibold() -> Font {
    weight(iced::font::Weight::Semibold)
}
fn fade(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}
fn fill(color: Color) -> container::Style {
    container::Style {
        background: Some(color.into()),
        ..Default::default()
    }
}

pub fn view(library: &Library) -> Element<'_, Message> {
    responsive(move |size| {
        let compact = size.width < COMPACT_WIDTH;
        let width = if compact {
            size.width
        } else {
            size.width - SIDEBAR_WIDTH - 1.0
        };
        let page = match library
            .selected
            .and_then(|index| Some((index, library.games.get(index)?)))
        {
            Some((index, game)) => {
                game_page(library, index, game, width, launch(index, library.starting))
            }
            None if library.filter == Filter::Settings => settings_page(library, compact),
            None => library_page(library, width, compact),
        };
        let body: Element<'_, Message> = if compact {
            page
        } else {
            let divider = container(space())
                .width(1)
                .height(Fill)
                .style(|_| fill(EDGE));
            row![sidebar(library), divider, page].into()
        };
        container(body)
            .width(Fill)
            .height(Fill)
            .style(|_| fill(BACKGROUND))
            .into()
    })
    .into()
}

fn tr(library: &Library) -> &'static crate::i18n::Strings {
    crate::i18n::Locale::resolve(&library.global.language).strings()
}

fn sidebar(library: &Library) -> Element<'_, Message> {
    let s = tr(library);
    let brand = row![
        svg(svg::Handle::from_memory(LOGO)).width(36).height(36),
        column![
            text("Doslet").size(20).font(bold()),
            text("DOS game library").size(11).color(MUTED),
        ]
        .spacing(1),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let mut nav = column![section(s.library_section)].spacing(4);
    let mut genres = false;
    for (label, filter) in filters(library) {
        if matches!(filter, Filter::Genre(_)) && !genres {
            genres = true;
            nav = nav.push(space().height(14)).push(section(s.genres_section));
        }
        let count = library.games.iter().filter(|g| filter.admits(g)).count();
        nav = nav.push(nav_item(
            label,
            Some(count),
            library.selected.is_none() && library.filter == filter,
            filter,
        ));
    }
    let settings_selected = library.selected.is_none() && library.filter == Filter::Settings;
    let mut footer = column![
        nav_item(s.global_settings, None, settings_selected, Filter::Settings),
        space().height(6),
        section("PROFILES"),
    ]
    .spacing(6);
    for directory in library.directories() {
        footer = footer
            .push(container(text(display_path(directory)).size(11).color(MUTED)).padding([0, 12]));
    }
    footer = footer.push(
        button(text("Rescan profiles").size(12))
            .padding([6, 12])
            .style(nav_style(false))
            .on_press(Message::Rescan),
    );
    container(
        column![
            brand,
            space().height(30),
            scrollable(nav).height(Fill),
            footer
        ]
        .spacing(8)
        .padding([28, 18]),
    )
    .width(SIDEBAR_WIDTH)
    .height(Fill)
    .style(|_| fill(SIDEBAR))
    .into()
}

/// Every library view: all games, then each genre.
fn filters(library: &Library) -> Vec<(&str, Filter)> {
    let mut filters = vec![(tr(library).all_games, Filter::All)];
    filters.extend(
        library
            .genres()
            .into_iter()
            .map(|genre| (genre, Filter::Genre(genre.to_owned()))),
    );
    filters
}

fn section(label: &str) -> Element<'_, Message> {
    container(
        text(label)
            .size(11)
            .font(semibold())
            .color(fade(MUTED, 0.7)),
    )
    .padding([4, 12])
    .into()
}

fn nav_item(
    label: &str,
    count: Option<usize>,
    selected: bool,
    filter: Filter,
) -> Element<'_, Message> {
    let mut content = row![
        text(label)
            .size(14)
            .font(if selected { semibold() } else { Font::DEFAULT }),
        space::horizontal(),
    ]
    .align_y(Alignment::Center);
    if let Some(count) = count {
        content = content.push(text(count.to_string()).size(12).color(MUTED));
    }
    button(content)
        .width(Fill)
        .padding([9, 12])
        .style(nav_style(selected))
        .on_press(Message::Filter(filter))
        .into()
}

fn nav_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = status == button::Status::Hovered;
        button::Style {
            background: (selected || hovered)
                .then(|| fade(ACCENT, if selected { 0.12 } else { 0.06 }).into()),
            text_color: if selected { ACCENT } else { FOREGROUND },
            border: Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

fn chip_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| button::Style {
        border: Border {
            color: if selected { fade(ACCENT, 0.5) } else { EDGE },
            width: 1.0,
            radius: 18.0.into(),
        },
        ..nav_style(selected)(theme, status)
    }
}

fn secondary_button_style(_: &Theme, status: button::Status) -> button::Style {
    let hovered = status == button::Status::Hovered;
    button::Style {
        background: Some(fade(SURFACE, if hovered { 0.98 } else { 0.82 }).into()),
        text_color: if hovered { ACCENT } else { FOREGROUND },
        border: Border {
            color: if hovered { fade(ACCENT, 0.55) } else { EDGE },
            width: 1.0,
            radius: 22.0.into(),
        },
        ..Default::default()
    }
}

fn library_page(library: &Library, width: f32, compact: bool) -> Element<'_, Message> {
    let s = tr(library);
    let title = match &library.filter {
        Filter::All | Filter::Settings => s.all_games.to_owned(),
        Filter::Genre(genre) => genre.clone(),
    };
    let visible: Vec<_> = library.visible().collect();
    let heading = column![
        text(title).size(30).font(bold()),
        text(match visible.len() {
            1 => "1 game".to_owned(),
            n => format!("{n} games"),
        })
        .size(13)
        .color(MUTED),
    ]
    .spacing(2);
    let search = text_input("Search title, developer or genre", &library.search)
        .on_input(Message::Search)
        .padding([11, 16])
        .size(14)
        .style(search_style);
    let mut page = column![].spacing(22).padding([30.0, PAGE_PADDING]);
    if compact {
        let mut chip_items = filters(library);
        chip_items.push((s.global_settings, Filter::Settings));
        let chips = chip_items.into_iter().map(|(label, filter)| {
            let selected = library.filter == filter;
            button(text(label).size(13))
                .padding([6, 14])
                .style(chip_style(selected))
                .on_press(Message::Filter(filter))
                .into()
        });
        page = page
            .push(row![heading, space::horizontal()].align_y(Alignment::Center))
            .push(search.width(Fill))
            .push(row(chips).spacing(8).wrap().vertical_spacing(8));
    } else {
        page = page.push(
            row![heading, space::horizontal(), search.width(320)]
                .spacing(16)
                .align_y(Alignment::Center),
        );
    }
    if let Some(notice) = &library.notice {
        page = page.push(notice_bar(notice));
    }
    let content: Element<'_, Message> = if visible.is_empty() {
        let mut empty_col = column![
            text(s.no_games_here).size(20).font(semibold()),
            text(if library.search.trim().is_empty() {
                s.no_games_hint
            } else {
                "Try another search or choose All games."
            })
            .size(14)
            .color(MUTED),
        ]
        .spacing(12)
        .align_x(Alignment::Center);
        if library.search.trim().is_empty() {
            empty_col = empty_col.push(
                row![
                    button(text(s.launch_demo).size(14).font(semibold()))
                        .padding([10, 20])
                        .style(secondary_button_style)
                        .on_press(Message::LaunchDemo),
                    button(text(s.global_settings_title).size(14))
                        .padding([10, 20])
                        .style(secondary_button_style)
                        .on_press(Message::Filter(Filter::Settings)),
                ]
                .spacing(12),
            );
        }
        center(empty_col).height(320).into()
    } else {
        // Leave room for the scrollbar beside the last column.
        cards(
            &visible,
            width - 2.0 * PAGE_PADDING - 16.0,
            library.starting,
        )
    };
    page.push(scrollable(container(content).padding(iced::Padding {
        right: 16.0,
        bottom: 24.0,
        ..Default::default()
    })))
    .into()
}

/// Rows of equally wide cards whose art keeps a 16:9 shape at any width.
fn cards<'a>(
    visible: &[(usize, &'a Game)],
    width: f32,
    starting: Option<usize>,
) -> Element<'a, Message> {
    let columns =
        (((width + CARD_SPACING) / (CARD_MAX_WIDTH + CARD_SPACING)).ceil() as usize).max(1);
    let card_width = ((width - CARD_SPACING * (columns - 1) as f32) / columns as f32)
        .floor()
        .max(120.0);
    let rows = visible.chunks(columns).map(|chunk| {
        row(chunk
            .iter()
            .map(|&(index, game)| card(index, game, card_width, launch(index, starting))))
        .spacing(CARD_SPACING)
        .into()
    });
    column(rows).spacing(CARD_SPACING).into()
}

fn notice_bar(notice: &str) -> Element<'_, Message> {
    container(
        row![
            text(notice).size(13).color(WARNING),
            space::horizontal(),
            button(text("Dismiss").size(12))
                .padding([4, 10])
                .style(nav_style(false))
                .on_press(Message::Dismiss),
        ]
        .align_y(Alignment::Center),
    )
    .padding([10, 14])
    .style(|_| container::Style {
        background: Some(fade(WARNING, 0.08).into()),
        border: Border {
            color: fade(WARNING, 0.35),
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn search_style(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: SURFACE.into(),
        border: Border {
            color: if focused { fade(ACCENT, 0.8) } else { EDGE },
            width: 1.0,
            radius: 22.0.into(),
        },
        icon: MUTED,
        placeholder: fade(MUTED, 0.8),
        value: FOREGROUND,
        selection: fade(ACCENT, 0.35),
    }
}

/// The glow color of a game: its cover's mean color, or its placeholder hue.
fn tint(game: &Game) -> Color {
    game.art
        .as_ref()
        .map_or_else(|| hue_colors(&game.name).0, |art| art.tint)
}

/// Play is offered while no machine is being prepared for the window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Launch {
    Ready,
    Starting,
    Waiting,
}
fn launch(index: usize, starting: Option<usize>) -> Launch {
    match starting {
        None => Launch::Ready,
        Some(game) if game == index => Launch::Starting,
        Some(_) => Launch::Waiting,
    }
}

fn card(index: usize, game: &Game, width: f32, state: Launch) -> Element<'_, Message> {
    let art_width = width - 2.0 * CARD_PADDING;
    let mut base = stack![art(game, 12.0, (art_width / 6.0).clamp(28.0, 52.0))]
        .width(Fill)
        .height(Fill);
    if state == Launch::Starting {
        base = base.push(container(badge("Starting…", ACTIVE)).padding(12));
    } else if game.has_autosave {
        base = base.push(container(badge("Paused", ACCENT)).padding(12));
    }
    let overlay = container(play_button(index, state, false, game.has_autosave))
        .width(Fill)
        .height(Fill)
        .align_x(Alignment::End)
        .align_y(Alignment::End)
        .padding(14)
        .style(|_| container::Style {
            background: Some(
                gradient::Linear::new(PI)
                    .add_stop(0.45, fade(INK, 0.0))
                    .add_stop(1.0, fade(INK, 0.85))
                    .into(),
            ),
            border: Border {
                radius: 12.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    // Roughly the characters of a 15-px semibold line in the card width.
    let letters = (art_width / 8.6) as usize;
    let caption = column![
        text(shorten(&game.name, letters)).size(15).font(semibold()),
        text(shorten(&game.caption(), letters + 4))
            .size(12)
            .color(MUTED),
    ]
    .spacing(3);
    let picture = container(hover(base, overlay)).height(art_width * 9.0 / 16.0);
    button(column![picture, caption].spacing(12))
        .width(width)
        .padding(CARD_PADDING)
        .style(card_style(tint(game)))
        .on_press(Message::Open(index))
        .into()
}

fn card_style(tint: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = status == button::Status::Hovered;
        button::Style {
            background: hovered.then(|| SURFACE.into()),
            text_color: FOREGROUND,
            border: Border {
                color: if hovered {
                    fade(tint, 0.45)
                } else {
                    Color::TRANSPARENT
                },
                width: 1.0,
                radius: 16.0.into(),
            },
            shadow: if hovered {
                Shadow {
                    color: fade(tint, 0.35),
                    offset: Vector::new(0.0, 10.0),
                    blur_radius: 32.0,
                }
            } else {
                Shadow::default()
            },
            snap: true,
        }
    }
}

fn badge<'a>(label: impl text::IntoFragment<'a>, color: Color) -> Element<'a, Message> {
    let dot = container(space())
        .width(7)
        .height(7)
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    container(
        row![dot, text(label).size(12).font(semibold()).color(color)]
            .spacing(7)
            .align_y(Alignment::Center),
    )
    .padding([5, 10])
    .style(|_| container::Style {
        background: Some(fade(INK, 0.82).into()),
        border: Border {
            radius: 20.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

/// Cover art, or a generated gradient with the game's initials.
fn art(game: &Game, radius: f32, initials_size: f32) -> Element<'_, Message> {
    if let Some(art) = &game.art {
        return image(art.cover.clone())
            .width(Fill)
            .height(Fill)
            .content_fit(ContentFit::Cover)
            .border_radius(radius)
            .into();
    }
    let (top, bottom) = hue_colors(&game.name);
    container(
        text(initials(&game.name))
            .size(initials_size)
            .font(bold())
            .color(fade(Color::WHITE, 0.92)),
    )
    .center(Fill)
    .style(move |_| container::Style {
        background: Some(
            gradient::Linear::new(2.4)
                .add_stop(0.0, top)
                .add_stop(1.0, bottom)
                .into(),
        ),
        border: Border {
            radius: radius.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

fn play_button(index: usize, state: Launch, large: bool, resume: bool) -> Element<'static, Message> {
    let ready = state == Launch::Ready;
    let size = if large { 20 } else { 15 };
    let icon = svg(svg::Handle::from_memory(PLAY))
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style {
            color: Some(if ready { INK } else { MUTED }),
        });
    let label = text(if state == Launch::Starting {
        "Starting…"
    } else if resume {
        "Resume"
    } else {
        "Play"
    })
    .size(if large { 18 } else { 14 })
    .font(bold());
    button(row![icon, label].spacing(8).align_y(Alignment::Center))
        .padding(if large { [15, 38] } else { [8, 18] })
        .style(play_style)
        .on_press_maybe(ready.then_some(Message::Launch(index)))
        .into()
}

fn play_style(_: &Theme, status: button::Status) -> button::Style {
    let (top, bottom, text_color, glow) = match status {
        button::Status::Disabled => (SURFACE, SURFACE, MUTED, 0.0),
        button::Status::Hovered => (
            Color::from_rgb8(214, 247, 222),
            Color::from_rgb8(160, 222, 178),
            INK,
            0.45,
        ),
        _ => (
            Color::from_rgb8(196, 238, 206),
            Color::from_rgb8(142, 207, 162),
            INK,
            0.25,
        ),
    };
    button::Style {
        background: Some(Background::Gradient(
            gradient::Linear::new(PI)
                .add_stop(0.0, top)
                .add_stop(1.0, bottom)
                .into(),
        )),
        text_color,
        border: Border {
            radius: 26.0.into(),
            ..Default::default()
        },
        shadow: Shadow {
            color: fade(ACCENT, glow),
            offset: Vector::new(0.0, 6.0),
            blur_radius: 22.0,
        },
        snap: true,
    }
}

fn game_page<'a>(
    library: &'a Library,
    index: usize,
    game: &'a Game,
    width: f32,
    state: Launch,
) -> Element<'a, Message> {
    let backdrop: Element<'_, Message> = match &game.art {
        Some(art) => image(art.backdrop.clone())
            .width(Fill)
            .height(Fill)
            .content_fit(ContentFit::Cover)
            .filter_method(image::FilterMethod::Linear)
            .opacity(0.55_f32)
            .into(),
        None => {
            let (top, _) = hue_colors(&game.name);
            container(space())
                .width(Fill)
                .height(Fill)
                .style(move |_| container::Style {
                    background: Some(
                        gradient::Linear::new(2.4)
                            .add_stop(0.0, fade(top, 0.55))
                            .add_stop(1.0, fade(top, 0.0))
                            .into(),
                    ),
                    ..Default::default()
                })
                .into()
        }
    };
    // Darken toward the bottom and left so text stays readable on any art.
    let shade = |angle: f32, from: f32, to: f32| {
        container(space())
            .width(Fill)
            .height(Fill)
            .style(move |_| container::Style {
                background: Some(
                    gradient::Linear::new(angle)
                        .add_stop(0.0, fade(BACKGROUND, from))
                        .add_stop(1.0, fade(BACKGROUND, to))
                        .into(),
                ),
                ..Default::default()
            })
    };
    stack![
        backdrop,
        shade(PI, 0.35, 1.0),
        shade(FRAC_PI_2, 0.55, 0.0),
        game_details(library, index, game, width, state),
    ]
    .width(Fill)
    .height(Fill)
    .into()
}

fn game_details<'a>(
    library: &'a Library,
    index: usize,
    game: &'a Game,
    width: f32,
    state: Launch,
) -> Element<'a, Message> {
    let wide = width >= 900.0;
    let usable = (width - 88.0).max(240.0);
    let art_width = if wide {
        (usable * 0.52).min(720.0)
    } else {
        usable.min(720.0)
    };
    let s = tr(library);
    let back = button(
        row![text("‹").size(22), text(s.library_back).size(14)]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([6, 12])
    .style(nav_style(false))
    .on_press(Message::Back);
    let hero = container(art(game, 16.0, (art_width / 7.0).clamp(40.0, 96.0)))
        .width(art_width)
        .height(art_width * 9.0 / 16.0)
        .style(move |_| container::Style {
            shadow: Shadow {
                color: fade(tint(game), 0.35),
                offset: Vector::new(0.0, 18.0),
                blur_radius: 48.0,
            },
            border: Border {
                radius: 16.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    let mut byline = Vec::new();
    byline.extend(game.info.developers.first().cloned());
    byline.extend(game.info.released.map(|year| year.to_string()));
    let tags = row(game
        .info
        .genres
        .iter()
        .chain(&game.info.gameplay)
        .map(|label| tag(label)))
    .spacing(8)
    .wrap()
    .vertical_spacing(8);
    let status: Element<'_, Message> = if state == Launch::Starting {
        badge("Preparing the machine…", ACTIVE)
    } else if game.has_autosave {
        badge("Paused state ready", ACCENT)
    } else {
        text("Enter plays · Esc returns")
            .size(12)
            .color(fade(MUTED, 0.8))
            .into()
    };
    let ready = state == Launch::Ready;
    let mut actions_row = row![play_button(index, state, true, game.has_autosave)]
        .spacing(12)
        .align_y(Alignment::Center);
    if game.has_autosave {
        actions_row = actions_row.push(
            button(text(s.restart_game).size(14).font(semibold()))
                .padding([12, 22])
                .style(secondary_button_style)
                .on_press_maybe(ready.then_some(Message::Restart(index))),
        );
    }
    for action in &game.actions {
        actions_row = actions_row.push(
            button(text(&action.name).size(14).font(semibold()))
                .padding([12, 20])
                .style(secondary_button_style)
                .on_press_maybe(
                    ready.then(|| Message::LaunchAction(index, action.id.clone())),
                ),
        );
    }
    actions_row = actions_row.push(status);

    let tab_badge = if let Some(summary) = &game.store_summary && summary.changed_count > 0 {
        format!(" ({})", summary.changed_count)
    } else if game.has_autosave {
        " (Paused)".to_owned()
    } else {
        String::new()
    };

    let tabs = row![
        button(
            text(s.tab_overview)
                .size(13)
                .font(if library.game_tab == GameTab::Overview { bold() } else { semibold() }),
        )
        .padding([7, 16])
        .style(chip_style(library.game_tab == GameTab::Overview))
        .on_press(Message::SelectTab(GameTab::Overview)),
        button(
            text(format!("{}{tab_badge}", s.tab_vfs_state))
                .size(13)
                .font(if library.game_tab == GameTab::VfsAndState { bold() } else { semibold() }),
        )
        .padding([7, 16])
        .style(chip_style(library.game_tab == GameTab::VfsAndState))
        .on_press(Message::SelectTab(GameTab::VfsAndState)),
        button(
            text(s.tab_configuration)
                .size(13)
                .font(if library.game_tab == GameTab::Configuration { bold() } else { semibold() }),
        )
        .padding([7, 16])
        .style(chip_style(library.game_tab == GameTab::Configuration))
        .on_press(Message::SelectTab(GameTab::Configuration)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let mut info = column![
        text(game.name.clone())
            .size(if wide { 44 } else { 36 })
            .font(bold()),
    ]
    .spacing(16);
    if !byline.is_empty() {
        info = info.push(text(byline.join("  ·  ")).size(16).color(MUTED));
    }
    if let Some(notice) = &library.notice {
        info = info.push(notice_bar(notice));
    }
    info = info
        .push(tags)
        .push(space().height(4))
        .push(actions_row.wrap().vertical_spacing(12))
        .push(space().height(6))
        .push(tabs);

    match library.game_tab {
        GameTab::Overview => {
            if let Some(description) = &game.info.description {
                let paragraphs = description
                    .split("\n\n")
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(|p| {
                        text(p.replace('\n', " "))
                            .size(15)
                            .line_height(1.55)
                            .color(fade(FOREGROUND, 0.86))
                            .into()
                    });
                info = info
                    .push(space().height(6))
                    .push(column(paragraphs).spacing(12).max_width(680));
            }
        }
        GameTab::VfsAndState => {
            info = info
                .push(space().height(6))
                .push(vfs_and_state_card(library, index, game));
        }
        GameTab::Configuration => {
            info = info
                .push(space().height(6))
                .push(game_config_card(library, game));
        }
    }
    let body: Element<'_, Message> = if wide {
        row![
            column![hero, facts(library, game)].spacing(24).width(art_width),
            info.width(Fill),
        ]
        .spacing(44)
        .into()
    } else {
        column![hero, info, facts(library, game)].spacing(28).into()
    };
    scrollable(
        column![back, body]
            .spacing(22)
            .padding([24, 44])
            .max_width(1500),
    )
    .into()
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn vfs_and_state_card<'a>(
    library: &'a Library,
    index: usize,
    game: &'a Game,
) -> Element<'a, Message> {
    let s = tr(library);
    let ram_section: Element<'a, Message> = match &game.autosave_info {
        Some(info) => {
            let mut details = format!(
                "{} compressed",
                format_bytes(info.compressed_bytes)
            );
            if let Some(raw) = info.uncompressed_bytes {
                details.push_str(&format!(" · {} machine snapshot", format_bytes(u64::from(raw))));
            }
            details.push_str(&format!(" · saved {}", info.age_label));
            container(
                row![
                    column![
                        row![
                            badge("Paused RAM Snapshot", ACCENT),
                            text(display_path(&game.autosave)).size(11).color(MUTED),
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                        text(details).size(12).color(FOREGROUND),
                    ]
                    .spacing(6)
                    .width(Fill),
                    button(text(s.discard_paused_state).size(12))
                        .padding([6, 12])
                        .style(secondary_button_style)
                        .on_press(Message::DiscardAutosave(index)),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            )
            .padding([12, 14])
            .style(|_| container::Style {
                background: Some(fade(SURFACE, 0.7).into()),
                border: Border {
                    color: fade(ACCENT, 0.3),
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..Default::default()
            })
            .into()
        }
        None => row![
            text("RAM Snapshot:").size(12).font(semibold()).color(MUTED),
            text("Not paused — next launch boots DOS cleanly.")
                .size(12)
                .color(MUTED),
        ]
        .spacing(8)
        .into(),
    };

    let disk_section: Element<'a, Message> = match &game.store_summary {
        Some(summary) => {
            let mode_buttons = row![
                button(text(s.vfs_all_files).size(11).font(semibold()))
                    .padding([5, 12])
                    .style(chip_style(library.vfs_mode == VfsViewMode::AllFiles))
                    .on_press(Message::SetVfsMode(VfsViewMode::AllFiles)),
                button(
                    text(format!(
                        "{} ({})",
                        s.vfs_changes_only, summary.changed_count
                    ))
                    .size(11)
                    .font(semibold())
                )
                .padding([5, 12])
                .style(chip_style(library.vfs_mode == VfsViewMode::ChangesOnly))
                .on_press(Message::SetVfsMode(VfsViewMode::ChangesOnly)),
            ]
            .spacing(6);

            let reset_button: Element<'a, Message> = if summary.changed_count > 0 {
                if library.confirm_reset_overlay {
                    row![
                        button(text("Confirm reset C:").size(11).font(semibold()))
                            .padding([5, 10])
                            .style(|_, _| button::Style {
                                background: Some(fade(WARNING, 0.2).into()),
                                text_color: WARNING,
                                border: Border {
                                    color: WARNING,
                                    width: 1.0,
                                    radius: 14.0.into(),
                                },
                                ..Default::default()
                            })
                            .on_press(Message::ConfirmResetOverlay(index)),
                        button(text("Cancel").size(11))
                            .padding([5, 8])
                            .style(secondary_button_style)
                            .on_press(Message::RequestResetOverlay(false)),
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                    .into()
                } else {
                    button(text(s.reset_c_changes).size(11))
                        .padding([5, 10])
                        .style(secondary_button_style)
                        .on_press(Message::RequestResetOverlay(true))
                        .into()
                }
            } else {
                space().into()
            };

            let current_path = if library.vfs_mode == VfsViewMode::ChangesOnly {
                "C:\\ (Changed files across all folders)".to_owned()
            } else if library.vfs_dir.is_empty() {
                "C:\\".to_owned()
            } else {
                format!("C:\\{}", library.vfs_dir)
            };

            let header = row![
                badge(format!("Disk Overlay · rev {}", summary.revision), ACTIVE),
                text(current_path).size(12).font(Font::MONOSPACE).color(FOREGROUND),
                space::horizontal(),
                mode_buttons,
                reset_button,
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(6);

            let mut file_rows = column![].spacing(6);

            if library.vfs_mode == VfsViewMode::ChangesOnly {
                let changed_entries: Vec<(usize, &crate::native_store_format::OverlayFileEntry)> = summary
                    .files
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.kind.is_changed())
                    .collect();

                if changed_entries.is_empty() {
                    file_rows = file_rows.push(
                        text("No files have been modified or created yet (clean base image).")
                            .size(12)
                            .color(MUTED),
                    );
                } else {
                    for (file_idx, entry) in changed_entries {
                        file_rows = file_rows.push(vfs_file_row(library, file_idx, entry, true));
                    }
                }
            } else {
                let current_dir = &library.vfs_dir;
                let dir_prefix = if current_dir.is_empty() {
                    String::new()
                } else {
                    format!("{current_dir}\\")
                };

                if !current_dir.is_empty() {
                    let parent = match current_dir.rfind('\\') {
                        Some(pos) => current_dir[..pos].to_string(),
                        None => String::new(),
                    };
                    let up_btn = button(
                        row![
                            text("Folder").size(11).font(semibold()).color(MUTED).width(70),
                            text(".. (Up to parent folder)").size(12).font(Font::MONOSPACE).color(ACCENT),
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                    )
                    .width(Fill)
                    .padding([6, 10])
                    .style(nav_style(false))
                    .on_press(Message::NavigateVfsDir(parent));
                    file_rows = file_rows.push(up_btn);
                }

                let mut subdirs: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
                let mut direct_files: Vec<(usize, &crate::native_store_format::OverlayFileEntry)> = Vec::new();

                for (idx, entry) in summary.files.iter().enumerate() {
                    let rel = if dir_prefix.is_empty() {
                        entry.path.as_str()
                    } else if let Some(stripped) = entry.path.strip_prefix(&dir_prefix) {
                        stripped
                    } else {
                        continue;
                    };

                    if let Some((subdir, _)) = rel.split_once('\\') {
                        let has_changed = entry.kind.is_changed();
                        subdirs
                            .entry(subdir.to_string())
                            .and_modify(|ch| *ch |= has_changed)
                            .or_insert(has_changed);
                    } else {
                        direct_files.push((idx, entry));
                    }
                }

                for (subdir, has_changes) in subdirs {
                    let next_dir = if current_dir.is_empty() {
                        subdir.clone()
                    } else {
                        format!("{current_dir}\\{subdir}")
                    };
                    let folder_status: Element<'a, Message> = if has_changes {
                        badge("Modified", ACCENT)
                    } else {
                        space().into()
                    };
                    let dir_btn = button(
                        row![
                            text("Folder").size(11).font(semibold()).color(MUTED).width(70),
                            text(format!("{subdir}\\")).size(12).font(Font::MONOSPACE).color(FOREGROUND).width(Fill),
                            folder_status,
                            text("Open ▾").size(11).color(MUTED),
                        ]
                        .spacing(10)
                        .align_y(Alignment::Center),
                    )
                    .width(Fill)
                    .padding([6, 10])
                    .style(nav_style(false))
                    .on_press(Message::NavigateVfsDir(next_dir));
                    file_rows = file_rows.push(dir_btn);
                }

                for (file_idx, entry) in direct_files {
                    file_rows = file_rows.push(vfs_file_row(library, file_idx, entry, false));
                }
            }

            column![header, file_rows].spacing(10).into()
        }
        None => row![
            text("Disk Overlay (C:):").size(12).font(semibold()).color(MUTED),
            text(format!(
                "Clean base image — changes from gameplay or Setup will be saved to {}.",
                display_path(&game.store_dir)
            ))
            .size(12)
            .color(MUTED),
        ]
        .spacing(8)
        .wrap()
        .vertical_spacing(4)
        .into(),
    };

    container(
        column![
            text("Saved State & Disk Overlay").size(15).font(bold()),
            ram_section,
            disk_section,
        ]
        .spacing(12),
    )
    .max_width(760)
    .padding([16, 20])
    .style(|_| container::Style {
        background: Some(fade(SIDEBAR, 0.82).into()),
        border: Border {
            color: fade(EDGE, 0.9),
            width: 1.0,
            radius: 14.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn vfs_file_row<'a>(
    library: &'a Library,
    file_idx: usize,
    entry: &'a crate::native_store_format::OverlayFileEntry,
    show_full_path: bool,
) -> Element<'a, Message> {
    use crate::native_store_format::OverlayFileKind;
    let selected = library.selected_save_file == Some(file_idx);
    let kind_color = match entry.kind {
        OverlayFileKind::Created => ACTIVE,
        OverlayFileKind::Modified => ACCENT,
        OverlayFileKind::Renamed | OverlayFileKind::Deleted => WARNING,
        OverlayFileKind::Directory | OverlayFileKind::Unchanged => MUTED,
    };
    let file_display_name = if show_full_path {
        format!("C:\\{}", entry.path)
    } else {
        entry.path.rsplit('\\').next().unwrap_or(&entry.path).to_string()
    };
    let mut path_label = file_display_name;
    if entry.kind == OverlayFileKind::Renamed
        && let Some(base) = &entry.base_path
    {
        path_label.push_str(&format!(" (from {base})"));
    }
    let size_label = entry
        .size
        .map_or_else(String::new, |s| format_bytes(u64::from(s)));
    let inspect_hint = if selected {
        "Hide ▴"
    } else {
        "Inspect ▾"
    };
    let item_row = button(
        row![
            container(
                text(entry.kind.label())
                    .size(11)
                    .font(semibold())
                    .color(kind_color),
            )
            .width(70),
            text(path_label)
                .size(12)
                .font(Font::MONOSPACE)
                .color(FOREGROUND)
                .width(Fill),
            text(size_label).size(12).color(MUTED),
            text(inspect_hint).size(11).color(ACCENT),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .width(Fill)
    .padding([7, 10])
    .style(nav_style(selected))
    .on_press(Message::SelectSaveFile(Some(file_idx)));

    if selected {
        let content: Element<'a, Message> = if let Some(preview) = &entry.preview {
            text(preview.clone())
                .size(11)
                .font(Font::MONOSPACE)
                .color(fade(FOREGROUND, 0.9))
                .into()
        } else {
            text("Binary / preview not available or empty.")
                .size(11)
                .color(MUTED)
                .into()
        };
        column![
            item_row,
            container(content)
                .width(Fill)
                .padding([10, 12])
                .style(|_| container::Style {
                    background: Some(INK.into()),
                    border: Border {
                        color: EDGE,
                        width: 1.0,
                        radius: 8.0.into(),
                    },
                    ..Default::default()
                }),
        ]
        .spacing(4)
        .into()
    } else {
        item_row.into()
    }
}

fn game_config_card<'a>(library: &'a Library, game: &'a Game) -> Element<'a, Message> {
    let mut preset_ids = vec!["default".to_string()];
    for p in HardwarePreset::builtins() {
        preset_ids.push(p.id);
    }
    for p in &library.global.presets {
        if !preset_ids.contains(&p.id) {
            preset_ids.push(p.id.clone());
        }
    }
    let current_preset = game
        .spec
        .preset
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let preset_buttons = preset_ids.into_iter().map(|id| {
        let selected = current_preset == id;
        let label = if id == "default" {
            format!("Global Default ({})", library.global.default_preset)
        } else {
            id.clone()
        };
        button(text(label).size(12))
            .padding([6, 12])
            .style(chip_style(selected))
            .on_press(Message::GameConfig(GameEdit::Preset(id)))
            .into()
    });

    let current_mode = game.spec.cpu_mode.unwrap_or_default();
    let mode_buttons = [
        (CpuMode::Fixed, "Fixed MHz"),
        (CpuMode::Adaptive, "Adaptive Idle"),
        (CpuMode::SpeedTiers, "Speed Tiers"),
    ]
    .into_iter()
    .map(|(mode, label)| {
        button(text(label).size(12))
            .padding([6, 12])
            .style(chip_style(current_mode == mode))
            .on_press(Message::GameConfig(GameEdit::CpuMode(mode)))
            .into()
    });

    let mem_buttons = [4u8, 8, 16, 32, 64].into_iter().map(|mib| {
        button(text(format!("{mib} MiB")).size(12))
            .padding([6, 12])
            .style(chip_style(game.spec.hardware.memory_mib == mib))
            .on_press(Message::GameConfig(GameEdit::MemoryMib(mib)))
            .into()
    });

    let midi_buttons = [("gm", "General MIDI"), ("mt32", "MT-32")]
        .into_iter()
        .map(|(id, label)| {
            let selected = game.spec.output.game_midi.eq_ignore_ascii_case(id);
            button(text(label).size(12))
                .padding([6, 12])
                .style(chip_style(selected))
                .on_press(Message::GameConfig(GameEdit::GameMidi(id.into())))
                .into()
        });

    let mut actions_col = column![].spacing(6);
    for (idx, action) in game.spec.game.actions.iter().enumerate() {
        actions_col = actions_col.push(
            row![
                text(&action.name).size(13).font(semibold()).width(140),
                text(&action.executable).size(13).color(MUTED).width(Fill),
                button(text("Remove").size(12))
                    .padding([4, 10])
                    .style(nav_style(false))
                    .on_press(Message::GameConfig(GameEdit::RemoveAction(idx))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
    }
    let add_action_row = row![
        text_input("Label (e.g. Sound Setup)", &library.new_action_name)
            .on_input(|v| Message::GameConfig(GameEdit::NewActionName(v)))
            .padding([8, 12])
            .size(13)
            .width(180),
        text_input("Executable (e.g. SETUP.EXE)", &library.new_action_exe)
            .on_input(|v| Message::GameConfig(GameEdit::NewActionExe(v)))
            .padding([8, 12])
            .size(13)
            .width(220),
        button(text("Add entrypoint").size(12).font(semibold()))
            .padding([8, 14])
            .style(secondary_button_style)
            .on_press(Message::GameConfig(GameEdit::AddAction)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .wrap()
    .vertical_spacing(8);

    let s = tr(library);
    let drive_c_source = game
        .spec
        .mounts
        .iter()
        .find(|m| m.letter == b'C')
        .map_or("", |m| game.spec.binding(&m.source));
    let drive_d_source = game
        .spec
        .mounts
        .iter()
        .find(|m| m.letter == b'D')
        .map_or("", |m| game.spec.binding(&m.source));

    container(
        column![
            text(s.game_configuration).size(18).font(bold()),
            column![
                text("Hardware Preset").size(12).color(MUTED),
                row(preset_buttons).spacing(8).wrap().vertical_spacing(8),
            ]
            .spacing(6),
            row![
                column![
                    text(format!("CPU Clock: {} MHz", game.spec.hardware.cpu_mhz))
                        .size(12)
                        .color(MUTED),
                    slider(1..=200, game.spec.hardware.cpu_mhz, |mhz| {
                        Message::GameConfig(GameEdit::CpuMhz(mhz))
                    })
                    .width(240),
                ]
                .spacing(6),
                column![
                    text("CPU Operating Mode").size(12).color(MUTED),
                    row(mode_buttons).spacing(8).wrap().vertical_spacing(8),
                ]
                .spacing(6),
            ]
            .spacing(24)
            .wrap()
            .vertical_spacing(12),
            column![
                text("Memory (RAM)").size(12).color(MUTED),
                row(mem_buttons).spacing(8).wrap().vertical_spacing(8),
            ]
            .spacing(6),
            row![
                checkbox(game.spec.hardware.sb16)
                    .label("Sound Blaster 16")
                    .on_toggle(|v| Message::GameConfig(GameEdit::Sb16(v))),
                checkbox(game.spec.hardware.mpu401)
                    .label("MPU-401 MIDI")
                    .on_toggle(|v| Message::GameConfig(GameEdit::Mpu401(v))),
                checkbox(game.spec.hardware.gameport)
                    .label("Joystick / Gameport")
                    .on_toggle(|v| Message::GameConfig(GameEdit::Gameport(v))),
            ]
            .spacing(20)
            .wrap()
            .vertical_spacing(8),
            column![
                text("Game MIDI Standard").size(12).color(MUTED),
                row(midi_buttons).spacing(8),
            ]
            .spacing(6),
            row![
                column![
                    text("Drive C: Source (Folder or .zip)").size(12).color(MUTED),
                    text_input("Path to folder or .zip", drive_c_source)
                        .on_input(|v| Message::GameConfig(GameEdit::DriveCSource(v)))
                        .padding([8, 12])
                        .size(13)
                        .width(260),
                ]
                .spacing(6),
                column![
                    text("CD-ROM Drive D: (.iso image)").size(12).color(MUTED),
                    text_input("Optional .iso path", drive_d_source)
                        .on_input(|v| Message::GameConfig(GameEdit::DriveDSource(v)))
                        .padding([8, 12])
                        .size(13)
                        .width(260),
                ]
                .spacing(6),
            ]
            .spacing(16)
            .wrap()
            .vertical_spacing(10),
            column![
                text("Main Executable").size(12).color(MUTED),
                text_input("GAME.EXE", &game.spec.game.executable)
                    .on_input(|v| Message::GameConfig(GameEdit::Executable(v)))
                    .padding([8, 12])
                    .size(13)
                    .width(280),
            ]
            .spacing(6),
            column![
                text("Additional Entrypoints (Setup, Editors)").size(12).color(MUTED),
                actions_col,
                add_action_row,
            ]
            .spacing(8),
            row![
                button(text(s.save_to_profile).size(14).font(bold()))
                    .padding([10, 22])
                    .style(play_style)
                    .on_press(Message::GameConfig(GameEdit::Save)),
                button(text("Close").size(13))
                    .padding([10, 18])
                    .style(nav_style(false))
                    .on_press(Message::GameConfig(GameEdit::Toggle)),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ]
        .spacing(16),
    )
    .max_width(720)
    .padding([20, 24])
    .style(|_| container::Style {
        background: Some(fade(SIDEBAR, 0.92).into()),
        border: Border {
            color: fade(ACCENT, 0.35),
            width: 1.0,
            radius: 16.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn settings_page(library: &Library, compact: bool) -> Element<'_, Message> {
    let s = tr(library);
    let g = &library.global;
    let heading = column![
        text(s.global_settings_title).size(30).font(bold()),
        text(s.global_settings_subtitle)
            .size(13)
            .color(MUTED),
    ]
    .spacing(2);

    let mixer_buttons = [
        ("system", "System audio mixer"),
        ("application", "Internal application mixer"),
    ]
    .into_iter()
    .map(|(id, label)| {
        let selected = g.audio_mixer.eq_ignore_ascii_case(id);
        button(text(label).size(13))
            .padding([7, 14])
            .style(chip_style(selected))
            .on_press(Message::Global(GlobalEdit::AudioMixer(id.into())))
            .into()
    });

    let game_midi_buttons = [("gm", "General MIDI"), ("mt32", "Roland MT-32")]
        .into_iter()
        .map(|(id, label)| {
            let selected = g.game_midi.eq_ignore_ascii_case(id);
            button(text(label).size(13))
                .padding([7, 14])
                .style(chip_style(selected))
                .on_press(Message::Global(GlobalEdit::GameMidi(id.into())))
                .into()
        });

    let audio_section = settings_card(
        "Audio & MIDI",
        column![
            row![
                checkbox(g.audio_enabled)
                    .label("Enable audio output")
                    .on_toggle(|v| Message::Global(GlobalEdit::AudioEnabled(v))),
                space().width(24),
                text(format!("Master volume: {}%", g.audio_volume))
                    .size(13)
                    .color(MUTED),
                slider(0..=100, g.audio_volume, |v| {
                    Message::Global(GlobalEdit::AudioVolume(v))
                })
                .width(200),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(8),
            column![
                text("Output Audio Mixer").size(12).color(MUTED),
                row(mixer_buttons).spacing(8).wrap().vertical_spacing(8),
            ]
            .spacing(6),
            row![
                column![
                    text("MIDI Synthesizer / Output Port").size(12).color(MUTED),
                    row![
                        pick_list(
                            library.midi_choices.as_slice(),
                            Some(&g.midi_output),
                            |choice| Message::Global(GlobalEdit::MidiOutput(choice)),
                        )
                        .width(240),
                        button(text("Refresh ports").size(12))
                            .padding([6, 12])
                            .style(secondary_button_style)
                            .on_press(Message::Global(GlobalEdit::RefreshMidiPorts)),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                ]
                .spacing(6),
                column![
                    text("Default Game MIDI Standard").size(12).color(MUTED),
                    row(game_midi_buttons).spacing(8),
                ]
                .spacing(6),
            ]
            .spacing(28)
            .wrap()
            .vertical_spacing(12),
            column![
                text("MT-32 / CM-32L ROM Directory").size(12).color(MUTED),
                text_input(
                    "Path to MT32_CONTROL.ROM & MT32_PCM.ROM",
                    g.mt32_rom_dir.as_deref().unwrap_or(""),
                )
                .on_input(|v| Message::Global(GlobalEdit::Mt32RomDir(v)))
                .padding([8, 12])
                .size(13)
                .width(420),
            ]
            .spacing(6),
        ]
        .spacing(16),
    );

    let kb_buttons = [
        (KeyboardMode::Bios, "BIOS compatible"),
        (KeyboardMode::At, "Full AT controller"),
    ]
    .into_iter()
    .map(|(mode, label)| {
        button(text(label).size(13))
            .padding([7, 14])
            .style(chip_style(g.keyboard == mode))
            .on_press(Message::Global(GlobalEdit::Keyboard(mode)))
            .into()
    });

    let mouse_buttons = [
        (MouseMode::Auto, "Auto"),
        (MouseMode::Capture, "Captured relative mouse"),
        (MouseMode::NoCapture, "Seamless pointer"),
    ]
    .into_iter()
    .map(|(mode, label)| {
        button(text(label).size(13))
            .padding([7, 14])
            .style(chip_style(g.input.mouse_mode == mode))
            .on_press(Message::Global(GlobalEdit::MouseMode(mode)))
            .into()
    });

    let pad_buttons = (0u8..4).map(|idx| {
        button(text(format!("Pad {}", idx + 1)).size(13))
            .padding([7, 14])
            .style(chip_style(g.input.pad_index == idx))
            .on_press(Message::Global(GlobalEdit::PadIndex(idx)))
            .into()
    });

    let input_section = settings_card(
        "Controllers & Input",
        column![
            row![
                column![
                    text("Keyboard Emulation").size(12).color(MUTED),
                    row(kb_buttons).spacing(8),
                ]
                .spacing(6),
                column![
                    text("Mouse Tracking").size(12).color(MUTED),
                    row(mouse_buttons).spacing(8),
                ]
                .spacing(6),
            ]
            .spacing(28)
            .wrap()
            .vertical_spacing(12),
            row![
                checkbox(g.gameport)
                    .label("Enable Joystick / Gameport by default")
                    .on_toggle(|v| Message::Global(GlobalEdit::Gameport(v))),
                checkbox(g.input.hide_cursor)
                    .label("Hide host cursor over guest screen")
                    .on_toggle(|v| Message::Global(GlobalEdit::HideCursor(v))),
            ]
            .spacing(24)
            .wrap()
            .vertical_spacing(8),
            row![
                column![
                    text("Active Host Gamepad").size(12).color(MUTED),
                    row(pad_buttons).spacing(8),
                ]
                .spacing(6),
                column![
                    text(format!("Stick Deadzone: {}%", g.input.deadzone_percent))
                        .size(12)
                        .color(MUTED),
                    slider(0..=50, g.input.deadzone_percent, |v| {
                        Message::Global(GlobalEdit::Deadzone(v))
                    })
                    .width(200),
                ]
                .spacing(6),
            ]
            .spacing(28)
            .wrap()
            .vertical_spacing(12),
        ]
        .spacing(16),
    );

    let filter_buttons = [
        (VideoFilter::Nearest, "Nearest (sharp pixels)"),
        (VideoFilter::SharpBilinear, "Sharp Bilinear"),
        (VideoFilter::Hq2x, "HQ2x"),
    ]
    .into_iter()
    .map(|(f, label)| {
        button(text(label).size(13))
            .padding([7, 14])
            .style(chip_style(g.video_filter == f))
            .on_press(Message::Global(GlobalEdit::VideoFilter(f)))
            .into()
    });

    let scaling_buttons = [("fit", "Fit window"), ("integer", "Integer scale")]
        .into_iter()
        .map(|(id, label)| {
            button(text(label).size(13))
                .padding([7, 14])
                .style(chip_style(g.scaling.eq_ignore_ascii_case(id)))
                .on_press(Message::Global(GlobalEdit::Scaling(id.into())))
                .into()
        });

    let aspect_buttons = [("4:3", "CRT 4:3"), ("native", "Square pixels")]
        .into_iter()
        .map(|(id, label)| {
            button(text(label).size(13))
                .padding([7, 14])
                .style(chip_style(g.aspect.eq_ignore_ascii_case(id)))
                .on_press(Message::Global(GlobalEdit::Aspect(id.into())))
                .into()
        });

    let lang_buttons = [
        ("auto", "Auto (System)"),
        ("en", "English"),
        ("pl", "Polski"),
    ]
    .into_iter()
    .map(|(id, label)| {
        button(text(label).size(13))
            .padding([7, 14])
            .style(chip_style(g.language.eq_ignore_ascii_case(id)))
            .on_press(Message::Global(GlobalEdit::Language(id.into())))
            .into()
    });

    let display_section = settings_card(
        "Display & Language",
        column![
            column![
                text("Interface Language").size(12).color(MUTED),
                row(lang_buttons).spacing(8).wrap().vertical_spacing(8),
            ]
            .spacing(6),
            column![
                text("Scaling Filter").size(12).color(MUTED),
                row(filter_buttons).spacing(8).wrap().vertical_spacing(8),
            ]
            .spacing(6),
            row![
                column![
                    text("Viewport Scaling").size(12).color(MUTED),
                    row(scaling_buttons).spacing(8),
                ]
                .spacing(6),
                column![
                    text("Aspect Ratio").size(12).color(MUTED),
                    row(aspect_buttons).spacing(8),
                ]
                .spacing(6),
            ]
            .spacing(28)
            .wrap()
            .vertical_spacing(12),
        ]
        .spacing(16),
    );

    let mut presets = HardwarePreset::builtins();
    for custom in &g.presets {
        if let Some(existing) = presets.iter_mut().find(|p| p.id == custom.id) {
            *existing = custom.clone();
        } else {
            presets.push(custom.clone());
        }
    }
    let preset_buttons = presets.iter().map(|p| {
        let selected = g.default_preset == p.id;
        button(
            column![
                text(p.name.clone()).size(13).font(semibold()),
                text(format!("{} MHz · {} MiB RAM", p.cpu_mhz, p.memory_mib))
                    .size(11)
                    .color(MUTED),
            ]
            .spacing(2),
        )
        .padding([10, 16])
        .style(chip_style(selected))
        .on_press(Message::Global(GlobalEdit::DefaultPreset(p.id.clone())))
        .into()
    });

    let mut custom_list = column![].spacing(6);
    for custom in &g.presets {
        custom_list = custom_list.push(
            row![
                text(custom.name.clone()).size(13).font(semibold()).width(180),
                text(format!(
                    "{} MHz · {} · {} MiB RAM",
                    custom.cpu_mhz,
                    custom.cpu_mode.as_str(),
                    custom.memory_mib
                ))
                .size(12)
                .color(MUTED)
                .width(Fill),
                button(text("Remove").size(12))
                    .padding([4, 10])
                    .style(nav_style(false))
                    .on_press(Message::Global(GlobalEdit::RemovePreset(custom.id.clone()))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
    }

    let new_preset_modes = [
        (CpuMode::Fixed, "Fixed"),
        (CpuMode::Adaptive, "Adaptive"),
        (CpuMode::SpeedTiers, "Speed Tiers"),
    ]
    .into_iter()
    .map(|(m, label)| {
        button(text(label).size(12))
            .padding([5, 10])
            .style(chip_style(library.new_preset_mode == m))
            .on_press(Message::Global(GlobalEdit::NewPresetMode(m)))
            .into()
    });

    let new_preset_mems = [4u8, 8, 16, 32, 64].into_iter().map(|mib| {
        button(text(format!("{mib} MiB")).size(12))
            .padding([5, 10])
            .style(chip_style(library.new_preset_memory == mib))
            .on_press(Message::Global(GlobalEdit::NewPresetMemory(mib)))
            .into()
    });

    let add_preset_form = column![
        text("Create Custom Hardware Preset").size(12).color(MUTED),
        row![
            text_input("Preset name (e.g. Fast 486)", &library.new_preset_name)
                .on_input(|v| Message::Global(GlobalEdit::NewPresetName(v)))
                .padding([8, 12])
                .size(13)
                .width(200),
            text(format!("{} MHz", library.new_preset_mhz))
                .size(12)
                .color(FOREGROUND),
            slider(1..=200, library.new_preset_mhz, |v| {
                Message::Global(GlobalEdit::NewPresetMhz(v))
            })
            .width(150),
            row(new_preset_modes).spacing(6),
            row(new_preset_mems).spacing(6),
            button(text("Add preset").size(12).font(semibold()))
                .padding([7, 14])
                .style(secondary_button_style)
                .on_press(Message::Global(GlobalEdit::AddPreset)),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .wrap()
        .vertical_spacing(8),
    ]
    .spacing(6);

    let presets_section = settings_card(
        "Default Hardware & Custom Presets",
        column![
            text("Used when a game profile does not specify its own preset or custom MHz.")
                .size(13)
                .color(MUTED),
            row(preset_buttons).spacing(10).wrap().vertical_spacing(10),
            custom_list,
            add_preset_form,
        ]
        .spacing(14),
    );

    let mut page = column![].spacing(22).padding([30.0, PAGE_PADDING]);
    if compact {
        let mut chip_items = filters(library);
        chip_items.push((s.global_settings, Filter::Settings));
        let chips = chip_items.into_iter().map(|(label, filter)| {
            let selected = library.filter == filter;
            button(text(label).size(13))
                .padding([6, 14])
                .style(chip_style(selected))
                .on_press(Message::Filter(filter))
                .into()
        });
        page = page
            .push(heading)
            .push(row(chips).spacing(8).wrap().vertical_spacing(8));
    } else {
        page = page.push(heading);
    }
    if let Some(notice) = &library.notice {
        page = page.push(notice_bar(notice));
    }
    let body = column![
        audio_section,
        input_section,
        display_section,
        presets_section
    ]
    .spacing(20)
    .max_width(920);
    page.push(scrollable(container(body).padding(iced::Padding {
        right: 16.0,
        bottom: 32.0,
        ..Default::default()
    })))
    .into()
}

fn settings_card<'a>(
    title: &'a str,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    container(
        column![text(title).size(17).font(bold()), content.into()].spacing(14),
    )
    .width(Fill)
    .padding([20, 24])
    .style(|_| container::Style {
        background: Some(fade(SIDEBAR, 0.85).into()),
        border: Border {
            color: EDGE,
            width: 1.0,
            radius: 14.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn tag(label: &str) -> Element<'_, Message> {
    container(text(label).size(12).font(semibold()).color(FOREGROUND))
        .padding([5, 12])
        .style(|_| container::Style {
            background: Some(fade(Color::WHITE, 0.08).into()),
            border: Border {
                color: fade(Color::WHITE, 0.12),
                width: 1.0,
                radius: 20.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn facts<'a>(library: &'a Library, game: &'a Game) -> Element<'a, Message> {
    let s = tr(library);
    let list = |labels: &[String]| (!labels.is_empty()).then(|| labels.join(", "));
    let profile = game
        .profile
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let rows = [
        (s.released, game.info.released.map(|year| year.to_string())),
        (s.developer, list(&game.info.developers)),
        (s.publisher, list(&game.info.publishers)),
        (s.genre, list(&game.info.genres)),
        (s.gameplay, list(&game.info.gameplay)),
        (
            "Program",
            Some(game.program.clone()).filter(|p| !p.is_empty()),
        ),
        ("Drive C:", game.drive.clone()),
        ("Profile", profile),
    ];
    let rows = rows.into_iter().filter_map(|(label, value)| {
        Some(
            row![
                text(label).size(13).color(MUTED).width(100),
                text(value?).size(13).color(FOREGROUND),
            ]
            .spacing(12)
            .into(),
        )
    });
    container(column(rows).spacing(10))
        .width(Fill)
        .padding([18, 20])
        .style(|_| container::Style {
            background: Some(fade(SIDEBAR, 0.78).into()),
            border: Border {
                color: fade(EDGE, 0.8),
                width: 1.0,
                radius: 14.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// Two related colors derived from the title, stable across launches.
fn hue_colors(name: &str) -> (Color, Color) {
    let hash = name.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    let hue = (hash % 360) as f32;
    (hsl(hue, 0.42, 0.36), hsl(hue + 38.0, 0.5, 0.13))
}

fn hsl(hue: f32, saturation: f32, lightness: f32) -> Color {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = (hue % 360.0) / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = lightness - c / 2.0;
    Color::from_rgb(r + m, g + m, b + m)
}

/// Up to two capitals from significant words: "Heroes of Might and Magic" → "HM".
fn initials(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().next().is_some_and(char::is_uppercase) || word.len() > 3)
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .take(2)
        .collect()
}

fn shorten(value: &str, length: usize) -> String {
    let mut chars = value.chars();
    let mut short: String = chars.by_ref().take(length.max(1)).collect();
    if chars.next().is_some() {
        short.pop();
        short.push('…');
    }
    short
}

/// A profile directory as the user would type it, keeping its last part.
fn display_path(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let text = match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) => format!("~/{}", rest.display()),
        None if path.is_relative() => format!("./{}", path.display()),
        None => path.display().to_string(),
    };
    let count = text.chars().count();
    if count <= 30 {
        return text;
    }
    let tail: String = text.chars().skip(count - 29).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_use_initials_and_stable_colors() {
        assert_eq!(initials("Heroes of Might and Magic"), "HM");
        assert_eq!(initials("Sid Meier’s Civilization"), "SM");
        assert_eq!(initials("rise of the triad"), "RT");
        assert_eq!(hue_colors("ROTT"), hue_colors("ROTT"));
        let color = hsl(120.0, 1.0, 0.5);
        assert!(color.g > 0.99 && color.r < 0.01 && color.b < 0.01);
    }

    #[test]
    fn long_text_keeps_its_useful_end() {
        assert_eq!(shorten("Rise of the Triad", 8), "Rise of…");
        assert_eq!(shorten("ROTT", 8), "ROTT");
        assert_eq!(display_path(std::path::Path::new("games")), "./games");
        let deep = std::path::Path::new("/srv/very/long/path/to/many/doslet/profiles");
        let shown = display_path(deep);
        assert!(shown.starts_with('…') && shown.ends_with("doslet/profiles"));
        assert_eq!(shown.chars().count(), 30);
    }
}
