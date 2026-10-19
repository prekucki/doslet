//! Library presentation: a sidebar, a grid of cover cards and game pages.
use super::{Filter, Game, Library, Message};
use crate::ui::{ACCENT, EDGE, FOREGROUND, MUTED};
use iced::widget::{
    button, center, column, container, hover, image, responsive, row, scrollable, space, stack,
    svg, text, text_input,
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
            Some((index, game)) => game_page(index, game, width, launch(index, library.starting)),
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

fn sidebar(library: &Library) -> Element<'_, Message> {
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
    let mut nav = column![section("LIBRARY")].spacing(4);
    let mut genres = false;
    for (label, filter) in filters(library) {
        if matches!(filter, Filter::Genre(_)) && !genres {
            genres = true;
            nav = nav.push(space().height(14)).push(section("GENRES"));
        }
        let count = library.games.iter().filter(|g| filter.admits(g)).count();
        nav = nav.push(nav_item(label, count, library.filter == filter, filter));
    }
    let mut footer = column![section("PROFILES")].spacing(6);
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
    let mut filters = vec![("All games", Filter::All)];
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

fn nav_item(label: &str, count: usize, selected: bool, filter: Filter) -> Element<'_, Message> {
    let content = row![
        text(label)
            .size(14)
            .font(if selected { semibold() } else { Font::DEFAULT }),
        space::horizontal(),
        text(count.to_string()).size(12).color(MUTED),
    ]
    .align_y(Alignment::Center);
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

fn library_page(library: &Library, width: f32, compact: bool) -> Element<'_, Message> {
    let title = match &library.filter {
        Filter::All => "All games".to_owned(),
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
        let chips = filters(library).into_iter().map(|(label, filter)| {
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
        center(
            column![
                text("No games here").size(20).font(semibold()),
                text(if library.search.trim().is_empty() {
                    "Add a launch profile to ./games or save one from Doslet's settings."
                } else {
                    "Try another search or choose All games."
                })
                .size(14)
                .color(MUTED),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .height(320)
        .into()
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
    }
    let overlay = container(play_button(index, state, false))
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

fn badge(label: &str, color: Color) -> Element<'_, Message> {
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

fn play_button(index: usize, state: Launch, large: bool) -> Element<'static, Message> {
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

fn game_page(index: usize, game: &Game, width: f32, state: Launch) -> Element<'_, Message> {
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
        game_details(index, game, width, state),
    ]
    .width(Fill)
    .height(Fill)
    .into()
}

fn game_details(index: usize, game: &Game, width: f32, state: Launch) -> Element<'_, Message> {
    let wide = width >= 900.0;
    let usable = (width - 88.0).max(240.0);
    let art_width = if wide {
        (usable * 0.52).min(720.0)
    } else {
        usable.min(720.0)
    };
    let back = button(
        row![text("‹").size(22), text("Library").size(14)]
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
    } else {
        text("Enter plays · Esc returns")
            .size(12)
            .color(fade(MUTED, 0.8))
            .into()
    };
    let mut info = column![
        text(game.name.clone())
            .size(if wide { 44 } else { 36 })
            .font(bold()),
    ]
    .spacing(16);
    if !byline.is_empty() {
        info = info.push(text(byline.join("  ·  ")).size(16).color(MUTED));
    }
    info = info.push(tags).push(space().height(4)).push(
        row![play_button(index, state, true), status]
            .spacing(20)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(12),
    );
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
    let body: Element<'_, Message> = if wide {
        row![
            column![hero, facts(game)].spacing(24).width(art_width),
            info.width(Fill),
        ]
        .spacing(44)
        .into()
    } else {
        column![hero, info, facts(game)].spacing(28).into()
    };
    scrollable(
        column![back, body]
            .spacing(22)
            .padding([24, 44])
            .max_width(1500),
    )
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

fn facts(game: &Game) -> Element<'_, Message> {
    let list = |labels: &[String]| (!labels.is_empty()).then(|| labels.join(", "));
    let profile = game
        .profile
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let rows = [
        ("Released", game.info.released.map(|year| year.to_string())),
        ("Developer", list(&game.info.developers)),
        ("Publisher", list(&game.info.publishers)),
        ("Genre", list(&game.info.genres)),
        ("Gameplay", list(&game.info.gameplay)),
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
