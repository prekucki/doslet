#[cfg(target_arch = "wasm32")]
use crate::app::Doslet;
use crate::app::Message;
#[cfg(not(target_arch = "wasm32"))]
use crate::desktop::Doslet;
use doslet_runtime::SessionStatus;
use doslet_x86::Gpr16;
use iced::widget::{
    button, column, container, image, opaque, pick_list, responsive, row, rule, scrollable, space,
    stack, svg, text, tooltip,
};
use iced::{Border, Color, Element, Fill, Theme, theme};
mod audio;
mod cpu;
mod drives;
mod input;
mod memory;
mod sheet;

const INK: Color = Color::from_rgb8(17, 18, 21);
pub(crate) const PANEL: Color = Color::from_rgb8(25, 27, 31);
pub(crate) const EDGE: Color = Color::from_rgb8(48, 51, 57);
pub(crate) const MUTED: Color = Color::from_rgb8(151, 156, 165);
pub(crate) const ACCENT: Color = Color::from_rgb8(177, 195, 220);
pub(crate) const FOREGROUND: Color = Color::from_rgb8(235, 237, 240);
const HOT: Color = Color::from_rgb8(255, 179, 105);
const COLD: Color = Color::from_rgb8(125, 194, 255);
const HOVER: Color = Color::from_rgb8(41, 44, 50);
const DANGER: Color = Color::from_rgb8(244, 150, 146);
const STAGE: Color = Color::from_rgb8(20, 21, 24);
const RUNNING: Color = Color::from_rgb8(133, 183, 151);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Cpu,
    Audio,
    Display,
    Memory,
    Drives,
    Input,
    Details,
    #[cfg(target_arch = "wasm32")]
    Games,
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Open(Panel),
    Close,
    DismissNotice,
    AudioPage(AudioPage),
    StatusBar(StatusBar),
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum AudioPage {
    #[default]
    Output,
    Hardware,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusBar {
    #[default]
    Automatic,
    Compact,
    Meters,
}

impl std::fmt::Display for StatusBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Automatic => "Automatic",
            Self::Compact => "Compact",
            Self::Meters => "CPU + audio meters",
        })
    }
}

#[derive(Clone, Default)]
pub struct Workspace {
    pub panel: Option<Panel>,
    pub audio_page: AudioPage,
    pub status_bar: StatusBar,
    pub cpu: Option<crate::cpu_config::Settings>,
    #[cfg(not(target_arch = "wasm32"))]
    pub cpu_profile_form: Option<crate::cpu_config::ProfileForm>,
}

pub fn theme() -> Theme {
    Theme::custom(
        "Doslet graphite",
        theme::Palette {
            background: INK,
            text: FOREGROUND,
            primary: ACCENT,
            success: ACCENT,
            warning: Color::from_rgb8(228, 193, 133),
            danger: DANGER,
        },
    )
}

pub fn view(app: &Doslet) -> Element<'_, Message> {
    responsive(move |size| workspace(app, size)).into()
}

fn chrome(_: &Theme) -> container::Style {
    container::Style {
        background: Some(PANEL.into()),
        ..Default::default()
    }
}

fn status_button(
    icon: Icon,
    label: String,
    description: &'static str,
    panel_kind: Panel,
) -> Element<'static, Message> {
    let symbol = svg(svg::Handle::from_memory(icon.bytes()))
        .width(14)
        .height(14)
        .style(|_, _| svg::Style { color: Some(MUTED) });
    let mut content = row![symbol].spacing(6).align_y(iced::Center);
    if !label.is_empty() {
        content = content.push(text(label).size(11).color(MUTED));
    }
    tooltip(
        button(content)
            .padding([3, 6])
            .height(24)
            .style(ghost)
            .on_press(Message::Ui(Action::Open(panel_kind))),
        container(text(description).size(12))
            .padding([6, 10])
            .style(panel),
        tooltip::Position::Top,
    )
    .into()
}

fn footer(app: &Doslet, size: iced::Size) -> Element<'_, Message> {
    let width = size.width;
    let meters = match app.workspace.status_bar {
        StatusBar::Automatic => width >= 760.0 && size.height >= 360.0,
        StatusBar::Compact => false,
        StatusBar::Meters => true,
    };
    let inline_meters = meters && width >= 700.0;
    let status = app.session.status();
    let color = match status {
        SessionStatus::Running => RUNNING,
        SessionStatus::Faulted => DANGER,
        _ => MUTED,
    };
    let progress = if app.configurator.pending {
        Some("Preparing…")
    } else if app.session.pending_commit() {
        Some("Saving…")
    } else if app.session.storage_waiting() {
        Some("Loading…")
    } else if app.session.dirty_overlay() {
        Some("Unsaved changes")
    } else {
        None
    };
    let label = progress.unwrap_or(status_label(status));
    let dot = container(space::horizontal().width(5))
        .height(5)
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: 3.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    let mut state = row![dot].spacing(7).align_y(iced::Center);
    if width >= 520.0 || progress.is_some() || meters {
        state = state.push(text(label).size(11).color(MUTED));
    }
    let hint = if app.has_mouse_capture() {
        format!("{label} · Ctrl+Esc to release mouse")
    } else {
        label.to_owned()
    };
    let state = tooltip(
        state,
        container(text(hint).size(12)).padding([6, 10]).style(panel),
        tooltip::Position::Top,
    );
    #[cfg(not(target_arch = "wasm32"))]
    let speed = if app.v86_host_clock {
        Some("v86 · MAX".to_owned())
    } else if let Some(tiers) = app.speed_tiers.filter(|t| t.enabled) {
        Some(format!("{} MHz · Auto", tiers.grey_mhz))
    } else if app.analysis.controller_enabled {
        Some(format!("{} MHz · Auto", app.analysis.effective_mhz))
    } else {
        None
    };
    #[cfg(target_arch = "wasm32")]
    let speed = (app.cpu_settings.mode == crate::cpu_config::Mode::Tiers
        && app.cpu_tiers_available)
        .then(|| format!("{} MHz · Auto", app.cpu_settings.grey));
    let speed =
        speed.unwrap_or_else(|| format!("{} MHz", app.configurator.active.hardware.cpu_mhz));
    let cpu = if width < 380.0 {
        String::new()
    } else if width < 620.0 {
        speed.split(" · ").next().unwrap_or_default().to_owned()
    } else {
        speed
    };
    let dimensions = app.display.dimensions().map_or_else(
        || "—".into(),
        |size| format!("{}×{}", size.width as u32, size.height as u32),
    );
    let audio = if width < 520.0 {
        ""
    } else if app.configurator.active.hardware.sb16 {
        "SB16"
    } else if app.configurator.active.hardware.mpu401 {
        "MIDI"
    } else {
        "Off"
    };
    let cpu_control = if inline_meters {
        cpu_indicator(app, width < 900.0)
    } else {
        status_button(Icon::Cpu, cpu, "CPU settings and activity", Panel::Cpu)
    };
    let audio_control = if inline_meters {
        audio_spectrum(app)
    } else {
        status_button(Icon::Audio, audio.into(), "Audio settings", Panel::Audio)
    };
    let mut summary = row![state, space::horizontal()]
        .spacing(6)
        .align_y(iced::Center);
    if !meters || inline_meters {
        summary = summary.push(cpu_control);
    }
    summary = summary.push(status_button(
        Icon::Display,
        dimensions,
        "Display and scaling",
        Panel::Display,
    ));
    if !meters || inline_meters {
        summary = summary.push(audio_control);
    }
    let summary = container(summary)
        .padding([2, 12])
        .height(if inline_meters { 48 } else { 28 })
        .width(Fill);
    let content: Element<'_, Message> = if meters && !inline_meters {
        column![
            summary,
            container(
                row![
                    cpu_indicator(app, true),
                    space::horizontal(),
                    audio_spectrum(app),
                ]
                .spacing(8)
                .align_y(iced::Center)
            )
            .padding([2, 12])
            .height(44)
            .width(Fill)
        ]
        .into()
    } else {
        summary.into()
    };
    container(content).width(Fill).style(chrome).into()
}

fn workspace(app: &Doslet, size: iced::Size) -> Element<'_, Message> {
    let narrow = size.width < 700.0;
    let status = app.session.status();
    let can_run = matches!(
        status,
        SessionStatus::Ready | SessionStatus::Paused | SessionStatus::Running
    );
    let (icon, label) = match status {
        SessionStatus::Running => (Icon::Pause, "Pause"),
        SessionStatus::Paused => (Icon::Play, "Resume"),
        SessionStatus::Ready => (Icon::Play, "Run"),
        _ => (Icon::Restart, "Restart"),
    };
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut identity = row![].spacing(8).align_y(iced::Center);
    #[cfg(not(target_arch = "wasm32"))]
    if app.from_library {
        identity = identity
            .push(
                button(
                    row![text("‹").size(18), text("Library").size(13)]
                        .spacing(6)
                        .align_y(iced::Center),
                )
                .padding([4, 10])
                .style(ghost)
                .on_press(Message::ReturnToLibrary),
            )
            .push(container(rule::vertical(1)).height(18));
    }
    let title_length = ((size.width - 210.0).max(21.0) / 7.0) as usize;
    let identity = identity.push(
        text(shorten(&app.name, title_length.min(64)))
            .size(13)
            .color(FOREGROUND),
    );
    let mut actions = row![icon_button(
        icon,
        label,
        if can_run {
            Message::RunPause
        } else {
            Message::Reset
        },
        false
    )]
    .spacing(4)
    .align_y(iced::Center);
    if can_run {
        actions = actions.push(icon_button(Icon::Restart, "Restart", Message::Reset, false));
    }
    actions = actions
        .push(fullscreen_button(app, 32, 18))
        .push(icon_button(
            Icon::Menu,
            "Info and settings",
            Message::Ui(Action::Open(Panel::Details)),
            false,
        ));
    #[cfg(not(target_arch = "wasm32"))]
    {
        actions = actions.push(quit_button(app, 32, 18));
    }
    let toolbar = container(row![identity, space::horizontal(), actions].align_y(iced::Center))
        .padding([4, 12])
        .height(40)
        .width(Fill)
        .style(chrome);
    // Full screen swaps the toolbar for a thin strip and hides the footer.
    let fullscreen = app.display_sizing.fullscreen;
    let mut shell = if fullscreen {
        column![fullscreen_strip(app, can_run, icon, label), viewport(app)]
    } else {
        column![toolbar, rule::horizontal(1), viewport(app)]
    }
    .height(Fill)
    .width(Fill);
    let notice = app
        .session
        .fault()
        .map(|fault| {
            app.session
                .v86_last_error()
                .map_or_else(|| fault.to_string(), |detail| format!("{fault}: {detail}"))
        })
        .or_else(|| app.display_sizing.error().map(str::to_owned))
        .or_else(|| app.notice.clone());
    if let Some(notice) = notice {
        let label = shorten(&notice, if narrow { 40 } else { 100 });
        let mut alert = row![
            tooltip(
                text(label).size(12).color(DANGER),
                container(text(notice).size(13))
                    .padding(12)
                    .max_width(540)
                    .style(panel),
                tooltip::Position::Top
            ),
            space::horizontal(),
            small_button("Details", Message::Ui(Action::Open(Panel::Details))),
        ]
        .spacing(8)
        .align_y(iced::Center);
        if app.session.fault().is_none() && app.display_sizing.error().is_none() {
            alert = alert.push(small_button("×", Message::Ui(Action::DismissNotice)));
        }
        shell = shell.push(container(alert).padding([2, 12]).width(Fill));
    }
    if !fullscreen {
        shell = shell.push(rule::horizontal(1)).push(footer(app, size));
    }
    let shell: Element<'_, Message> = shell.into();
    if let Some(kind) = app.workspace.panel {
        let (title, content) = match kind {
            Panel::Cpu => return modal(shell, cpu::dialog(app, size), narrow),
            Panel::Audio => return modal(shell, audio::dialog(app, size), narrow),
            Panel::Drives => return modal(shell, drives::dialog(app, size), narrow),
            Panel::Input => return modal(shell, input::dialog(app, size), narrow),
            Panel::Display => ("Display", display_panel(app)),
            Panel::Memory => ("Memory", memory::details(app)),
            Panel::Details => ("Info", details_panel(app)),
            #[cfg(target_arch = "wasm32")]
            Panel::Games => ("Open a game", games_panel(app)),
        };
        let content = if kind != Panel::Details
            && let Some(notice) = &app.notice
        {
            column![content, text(notice).size(12).color(DANGER)]
                .spacing(14)
                .into()
        } else {
            content
        };
        let header = row![
            text(title).size(18),
            space::horizontal(),
            small_button("Close", Message::Ui(Action::Close))
        ]
        .align_y(iced::Center);
        let dialog = container(
            column![
                header,
                rule::horizontal(1),
                scrollable(container(content).padding(iced::Padding {
                    right: 12.0,
                    ..Default::default()
                }))
                .height(Fill)
            ]
            .spacing(18),
        )
        .padding(22)
        .max_width(520)
        .width(Fill)
        .height((size.height - 40.0).clamp(
            160.0,
            match kind {
                Panel::Details => 620.0,
                Panel::Memory => 520.0,
                _ => 320.0,
            },
        ))
        .style(panel);
        return modal(shell, dialog.into(), narrow);
    }
    shell
}

fn modal<'a>(
    base: Element<'a, Message>,
    dialog: Element<'a, Message>,
    narrow: bool,
) -> Element<'a, Message> {
    stack![
        base,
        iced::widget::mouse_area(
            container(opaque(dialog))
                .center_x(Fill)
                .center_y(Fill)
                .padding(if narrow { 8 } else { 16 })
                .style(backdrop)
        )
        .on_press(Message::Ui(Action::Close))
    ]
    .into()
}

fn backdrop(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgba8(0, 0, 0, 0.48).into()),
        ..Default::default()
    }
}

fn video_indicator(app: &Doslet, compact: bool, icon_only: bool) -> Element<'_, Message> {
    let fps = app
        .display_sizing
        .fps()
        .map_or_else(|| "— FPS".into(), |fps| format!("{fps:.0} FPS"));
    let label = if compact {
        shorten(&app.video_mode_label, 14).to_string()
    } else {
        app.video_mode_label.clone()
    };
    let label = format!("{label} · {fps}");
    let icon = svg(svg::Handle::from_memory(Icon::Display.bytes()))
        .width(24)
        .height(24)
        .style(|_, _| svg::Style {
            color: Some(ACCENT),
        });
    let content: Element<'_, Message> = if icon_only {
        row![icon, text(fps).size(12).font(iced::Font::MONOSPACE)]
            .spacing(9)
            .align_y(iced::Center)
            .into()
    } else {
        row![icon, text(label).size(12).font(iced::Font::MONOSPACE)]
            .spacing(9)
            .align_y(iced::Center)
            .into()
    };
    tooltip(
        button(content)
            .padding([6, 8])
            .style(ghost)
            .on_press(Message::Ui(Action::Open(Panel::Display))),
        container(text("Display settings").size(12))
            .padding([6, 10])
            .style(panel),
        tooltip::Position::Top,
    )
    .into()
}

fn fullscreen_button(app: &Doslet, side: u32, glyph: u32) -> Element<'static, Message> {
    let on = app.display_sizing.fullscreen;
    icon_button_sized(
        if on { Icon::Windowed } else { Icon::Fullscreen },
        if on {
            "Exit full screen"
        } else {
            "Full screen"
        },
        Message::DisplaySizing(crate::presentation::SizingMessage::Fullscreen(!on)),
        false,
        side,
        glyph,
    )
}

/// Switches the machine off. From the library this returns there without a
/// suspended state, so no half-finished game waits to be resumed; otherwise
/// it closes Doslet.
#[cfg(not(target_arch = "wasm32"))]
fn quit_button(app: &Doslet, side: u32, glyph: u32) -> Element<'static, Message> {
    let (label, message) = if app.from_library {
        ("Shut down (next launch starts afresh)", Message::ShutDown)
    } else {
        ("Quit", Message::ExitRequested)
    };
    icon_button_sized(Icon::Power, label, message, false, side, glyph)
}

/// Slim bar shown instead of the toolbar in full screen, with the same
/// controls as the toolbar so the player can pause, restart, change the
/// display, leave full screen or quit.
fn fullscreen_strip(
    app: &Doslet,
    can_run: bool,
    icon: Icon,
    label: &'static str,
) -> Element<'static, Message> {
    const SIDE: u32 = 24;
    const GLYPH: u32 = 14;
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut left = row![].spacing(2).align_y(iced::Center);
    #[cfg(not(target_arch = "wasm32"))]
    if app.from_library {
        left = left.push(icon_button_sized(
            Icon::Back,
            "Back to library (resume later)",
            Message::ReturnToLibrary,
            false,
            SIDE,
            GLYPH,
        ));
    }
    let mut left = left.push(icon_button_sized(
        icon,
        label,
        if can_run {
            Message::RunPause
        } else {
            Message::Reset
        },
        false,
        SIDE,
        GLYPH,
    ));
    if can_run {
        left = left.push(icon_button_sized(
            Icon::Restart,
            "Restart",
            Message::Reset,
            false,
            SIDE,
            GLYPH,
        ));
    }
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut right = row![
        icon_button_sized(
            Icon::Display,
            "Display settings",
            Message::Ui(Action::Open(Panel::Display)),
            false,
            SIDE,
            GLYPH,
        ),
        fullscreen_button(app, SIDE, GLYPH),
    ]
    .spacing(2)
    .align_y(iced::Center);
    #[cfg(not(target_arch = "wasm32"))]
    {
        right = right.push(quit_button(app, SIDE, GLYPH));
    }
    container(row![left, space::horizontal(), right].align_y(iced::Center))
        .padding([0, 6])
        .width(Fill)
        .into()
}

/// Desktop only: a browser page can only use the browser's full screen.
#[cfg(not(target_arch = "wasm32"))]
fn fullscreen_style_field(app: &Doslet) -> Element<'_, Message> {
    use crate::presentation::{FullscreenStyle, SizingMessage};
    let sizing = &app.display_sizing;
    let style = sizing.fullscreen_style;
    // Leaving must undo what entering did, so the style is fixed meanwhile.
    let locked = sizing.fullscreen || sizing.leaving_fullscreen();
    let choice = |label: &'static str, target: FullscreenStyle| {
        button(text(label).size(13))
            .padding([8, 12])
            .style(move |theme, status| segment(theme, status, style == target))
            .on_press_maybe((!locked).then_some(Message::DisplaySizing(
                SizingMessage::FullscreenStyle(target),
            )))
    };
    column![
        field(
            "Full screen",
            row![
                choice("Borderless window", FullscreenStyle::Borderless),
                choice("Native", FullscreenStyle::Native),
            ]
            .spacing(6)
            .into()
        ),
        text(match style {
            FullscreenStyle::Borderless => {
                "Borderless window maximizes the window without its frame. Desktop \
                 panels may stay visible, but the compositor never switches modes."
            }
            FullscreenStyle::Native => {
                "Native uses the compositor's full screen. On exit the picture \
                 pauses for a second before the window mode changes."
            }
        })
        .size(12)
        .color(MUTED),
    ]
    .spacing(6)
    .into()
}

fn display_panel(app: &Doslet) -> Element<'_, Message> {
    use crate::presentation::{DisplayFilter, FrameRate, ScreenShape, SizingMessage};
    let perfect = app.display_sizing.pixel_perfect;
    let shape = app.display_sizing.shape;
    let shape_button = |label: &'static str, target: ScreenShape| {
        button(text(label).size(13))
            .padding([8, 12])
            .style(move |theme, status| segment(theme, status, shape == target))
            // Pixel perfect keeps the raw raster proportions, so the shape
            // only applies to Fit.
            .on_press_maybe(
                (!perfect).then_some(Message::DisplaySizing(SizingMessage::Shape(target))),
            )
    };
    let dimensions = app.display.dimensions().map_or_else(
        || "No video output".into(),
        |size| {
            let mut line = format!("{} × {}", size.width as u32, size.height as u32);
            if app.video_refresh_mhz != 0 {
                line += &format!(" · {:.1} Hz", f64::from(app.video_refresh_mhz) / 1000.0);
            }
            if let Some(fps) = app.display_sizing.fps() {
                line += &format!(" · {fps:.0} fps");
            }
            line
        },
    );
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut panel = column![
        text(dimensions).size(13).color(MUTED),
        field(
            "Scaling",
            row![
                button(text("Pixel perfect").size(13))
                    .padding([8, 12])
                    .style(move |theme, status| segment(theme, status, perfect))
                    .on_press_maybe(
                        app.display
                            .dimensions()
                            .map(|_| Message::DisplaySizing(SizingMessage::PixelPerfect))
                    ),
                button(text("Fit window").size(13))
                    .padding([8, 12])
                    .style(move |theme, status| segment(theme, status, !perfect))
                    .on_press(Message::DisplaySizing(SizingMessage::Fit)),
            ]
            .spacing(6)
            .into()
        ),
        field(
            "Screen shape",
            row![
                shape_button("4:3", ScreenShape::Standard),
                shape_button("16:9", ScreenShape::Wide),
                shape_button("16:9 smart", ScreenShape::Stretch),
            ]
            .spacing(6)
            .into()
        ),
    ]
    .spacing(18);
    #[cfg(not(target_arch = "wasm32"))]
    {
        panel = panel.push(fullscreen_style_field(app));
    }
    panel
        .push(field(
            "Filter",
            if perfect {
                // Pixel perfect only enlarges whole pixels.
                text("None in pixel perfect").size(14).color(MUTED).into()
            } else {
                pick_list(
                    DisplayFilter::ALL,
                    Some(app.display_sizing.filter),
                    |filter| Message::DisplaySizing(SizingMessage::Filter(filter)),
                )
                .padding(8)
                .text_size(14)
                .width(Fill)
                .into()
            },
        ))
        .push(
            text(if perfect {
                "Pixel perfect only enlarges whole pixels at the original proportions: no \
             filter and no screen shape. Choose Fit window to use them."
                    .to_owned()
            } else if app.display_sizing.filter.needs_gpu() && !app.display_sizing.gpu {
                format!(
                    "No hardware GPU: {} is shown instead of {}.",
                    app.display_sizing.active_filter(),
                    app.display_sizing.filter
                )
            } else {
                match shape {
                    ScreenShape::Standard => "4:3 is the proportion of a CRT monitor.",
                    ScreenShape::Wide => "16:9 stretches the whole picture evenly.",
                    ScreenShape::Stretch if !app.display_sizing.gpu => {
                        "16:9 smart needs a hardware GPU; the picture is stretched evenly."
                    }
                    ScreenShape::Stretch => {
                        "16:9 smart keeps the centre near 4:3 and stretches only the \
                     edges. The mouse follows the picture."
                    }
                }
                .to_owned()
            })
            .size(12)
            .color(MUTED),
        )
        .push(
            text(
                "The filter is applied first; the screen shape is then applied to the \
                 filtered picture, so they combine.",
            )
            .size(12)
            .color(MUTED),
        )
        .push(
            button(
                text(if app.display_sizing.crt {
                    "CRT scanlines: on"
                } else {
                    "CRT scanlines: off"
                })
                .size(13),
            )
            .padding([8, 12])
            .style(move |theme, status| segment(theme, status, app.display_sizing.crt))
            .on_press(Message::DisplaySizing(SizingMessage::Crt(
                !app.display_sizing.crt,
            ))),
        )
        .push(field(
            "Frame rate",
            pick_list(
                FrameRate::ALL,
                Some(app.display_sizing.frame_rate),
                |rate| Message::DisplaySizing(SizingMessage::FrameRate(rate)),
            )
            .padding(8)
            .text_size(14)
            .width(Fill)
            .into(),
        ))
        .push(
            text(
                "How often a new picture is shown. Hz is the emulated VGA refresh and \
                 fps the pictures actually shown; the game's own speed does not change.",
            )
            .size(12)
            .color(MUTED),
        )
        .into()
}

fn details_panel(app: &Doslet) -> Element<'_, Message> {
    let registers = app.session.registers();
    let register = |label: &'static str, value: String| {
        row![
            text(label).size(12).color(MUTED).width(76),
            text(value).size(13)
        ]
        .align_y(iced::Center)
    };
    let mut step = button(text("Step instruction").size(13))
        .padding([7, 12])
        .style(secondary);
    if matches!(
        app.session.status(),
        SessionStatus::Ready | SessionStatus::Paused
    ) {
        step = step.on_press(Message::Step);
    }
    let mut body = column![
        text(&app.name).size(15),
        field(
            "Status bar",
            pick_list(
                [StatusBar::Automatic, StatusBar::Compact, StatusBar::Meters],
                Some(app.workspace.status_bar),
                |mode| Message::Ui(Action::StatusBar(mode)),
            )
            .text_size(13)
            .padding(8)
            .width(Fill)
            .into()
        ),
        text("Automatic shows meters in larger windows. Applies to this session.")
            .size(12)
            .color(MUTED),
        column![
            row![cpu_indicator(app, false), memory::indicator(app)]
                .spacing(12)
                .wrap(),
            row![audio_spectrum(app), video_indicator(app, true, false)]
                .spacing(12)
                .wrap(),
            row![drives::indicator(app), input::indicator()]
                .spacing(12)
                .wrap(),
        ]
        .spacing(12),
        rule::horizontal(1),
        text(app.storage_label()).size(13).color(MUTED),
        text(format!("{} instructions", app.session.steps()))
            .size(13)
            .color(MUTED),
        column![
            register(
                "CS:EIP",
                format!("{:04X}:{:08X}", registers.cs, registers.ip)
            ),
            register("AX", format!("{:04X}", registers.get(Gpr16::Ax))),
            register("CX", format!("{:04X}", registers.get(Gpr16::Cx))),
            register("DI", format!("{:04X}", registers.get(Gpr16::Di))),
        ]
        .spacing(4),
        row![step, small_button("Restart", Message::Reset)].spacing(8),
        rule::horizontal(1),
        text(app.audio_label()).size(12).color(MUTED),
        text(app.guest_audio_description()).size(12).color(MUTED),
        text(app.midi_activity.description()).size(12).color(MUTED),
        text(app.midi_mode_label()).size(12).color(MUTED),
    ]
    .spacing(14);
    #[cfg(target_arch = "wasm32")]
    {
        body = body.push(small_button(
            "Open a game…",
            Message::Ui(Action::Open(Panel::Games)),
        ));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(tiers) = app.speed_tiers {
            body = body.push(speed_tier_table(app));
            body = body.push(
                column![
                    text(format!(
                        "Speed tiers · hot {} / cold {} / grey {} MHz",
                        tiers
                            .hot_mhz
                            .map_or_else(|| "MAX".into(), |m| m.to_string()),
                        tiers.cold_mhz,
                        tiers.grey_mhz
                    ))
                    .size(12),
                    row![
                        text(format!("Hot {:.1}%", tiers.percent(0)))
                            .color(Color::from_rgb8(240, 170, 100)),
                        text(format!("Cold {:.1}%", tiers.percent(1)))
                            .color(Color::from_rgb8(120, 180, 240)),
                        text(format!("Grey {:.1}%", tiers.percent(2))).color(MUTED),
                        text(format!("IRQ {:.1}%", tiers.percent(3))).color(MUTED),
                    ]
                    .spacing(12),
                    small_button(
                        if tiers.enabled {
                            "Use normal CPU"
                        } else {
                            "Enable speed tiers"
                        },
                        Message::SpeedTiersToggle
                    ),
                ]
                .spacing(8),
            );
        }
        let (mcp_label, mcp_action) = match app.mcp {
            Some(address) => (format!("MCP http://{address}/mcp"), "Stop MCP"),
            None => ("MCP disabled".into(), "Enable MCP · screen and input"),
        };
        body = body.push(
            column![
                text(mcp_label).size(12).color(MUTED),
                small_button(mcp_action, Message::McpToggle)
            ]
            .spacing(8),
        );
        let (label, action) = match app.gdb {
            Some((address, connected)) => (
                format!(
                    "GDB {address} · {}",
                    if connected { "connected" } else { "listening" }
                ),
                "Stop GDB",
            ),
            None => ("GDB disabled".into(), "Enable GDB · 127.0.0.1:9001"),
        };
        body = body.push(
            column![
                text(label).size(12).color(MUTED),
                small_button(action, Message::GdbToggle),
            ]
            .spacing(8),
        );
        body = body.push(
            text(format!(
                "Core: {} MHz · emulation lag {:.1} ms",
                app.configurator.active.hardware.cpu_mhz,
                app.lag_nanos as f64 / 1e6,
            ))
            .size(12)
            .color(MUTED),
        );
    }
    if let Some(fault) = app.session.fault() {
        let message = app
            .session
            .v86_last_error()
            .map_or_else(|| fault.to_string(), |detail| format!("{fault}: {detail}"));
        body = body.push(text(message).size(13).color(DANGER));
    }
    if let Some(notice) = &app.notice {
        body = body.push(text(notice).size(13).color(MUTED));
    }
    body.into()
}

#[cfg(target_arch = "wasm32")]
fn games_panel(app: &Doslet) -> Element<'_, Message> {
    let programs = app
        .archive
        .as_ref()
        .map_or(&[][..], |archive| archive.programs.as_slice());
    let mut open = button(
        text(if app.importing {
            "Opening…"
        } else {
            "Choose ZIP…"
        })
        .size(14),
    )
    .padding([10, 16])
    .style(secondary);
    if !app.importing {
        open = open.on_press(Message::OpenArchive);
    }
    let mut start = button(text("Start game").size(14))
        .padding([10, 16])
        .style(primary);
    if app.program.is_some() && !app.importing {
        start = start.on_press(Message::StartArchive);
    }
    column![
        open,
        field(
            "Program",
            pick_list(programs, app.program.as_ref(), Message::ProgramSelected)
                .placeholder("Choose EXE / COM")
                .width(Fill)
                .text_size(14)
                .padding(9)
                .into()
        ),
        row![space::horizontal(), start],
    ]
    .spacing(20)
    .into()
}

pub(crate) fn field<'a, M: 'a>(label: &'a str, input: Element<'a, M>) -> Element<'a, M> {
    column![text(label).size(12).color(MUTED), input]
        .spacing(6)
        .width(Fill)
        .into()
}

fn small_button(label: &str, message: Message) -> iced::widget::Button<'_, Message> {
    button(text(label).size(13))
        .padding([6, 10])
        .style(ghost)
        .on_press(message)
}

#[derive(Clone, Copy)]
enum Icon {
    Play,
    Pause,
    Restart,
    Cpu,
    Ram,
    Audio,
    Disk,
    Input,
    Display,
    Menu,
    Fullscreen,
    Windowed,
    #[cfg(not(target_arch = "wasm32"))]
    Power,
    #[cfg(not(target_arch = "wasm32"))]
    Back,
}

impl Icon {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Play => include_bytes!("icons/play.svg"),
            Self::Pause => include_bytes!("icons/pause.svg"),
            Self::Restart => include_bytes!("icons/restart.svg"),
            Self::Cpu => include_bytes!("icons/cpu.svg"),
            Self::Ram => include_bytes!("icons/ram.svg"),
            Self::Audio => include_bytes!("icons/audio.svg"),
            Self::Disk => include_bytes!("icons/disk.svg"),
            Self::Input => include_bytes!("icons/input.svg"),
            Self::Display => include_bytes!("icons/display.svg"),
            Self::Menu => include_bytes!("icons/menu.svg"),
            Self::Fullscreen => include_bytes!("icons/fullscreen.svg"),
            Self::Windowed => include_bytes!("icons/windowed.svg"),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Power => include_bytes!("icons/power.svg"),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Back => include_bytes!("icons/back.svg"),
        }
    }
}

fn icon_button(
    icon: Icon,
    label: &'static str,
    message: Message,
    prominent: bool,
) -> Element<'static, Message> {
    icon_button_sized(icon, label, message, prominent, 32, 18)
}

fn icon_button_sized(
    icon: Icon,
    label: &'static str,
    message: Message,
    prominent: bool,
    side: u32,
    glyph: u32,
) -> Element<'static, Message> {
    let icon = svg(svg::Handle::from_memory(icon.bytes()))
        .width(glyph)
        .height(glyph)
        .style(move |_, _| svg::Style {
            color: Some(if prominent { INK } else { FOREGROUND }),
        });
    tooltip(
        button(container(icon).center_x(Fill).center_y(Fill))
            .width(side)
            .height(side)
            .padding(0)
            .style(if prominent { primary } else { ghost })
            .on_press(message),
        container(text(label).size(12))
            .padding([6, 10])
            .style(panel),
        tooltip::Position::Bottom,
    )
    .gap(6)
    .into()
}

fn viewport(app: &Doslet) -> Element<'_, Message> {
    let viewport: Element<'_, Message> = if let Some(frame) = app.display_sizing.image() {
        let handle = frame.image.clone();
        let pixels = frame.source_size;
        let sampling = frame.filter.sampling();
        let stretched = app.display_sizing.stretched();
        let (pointer_width, pointer_height) = app.pointer_dimensions();
        // Keep the host cursor visible while the window leaves full screen.
        let cursor = if app.hide_host_cursor() && !app.display_sizing.leaving_fullscreen() {
            iced::mouse::Interaction::Hidden
        } else {
            iced::mouse::Interaction::None
        };
        responsive(move |available| {
            let size = app.display_sizing.image_size(available, pixels);
            let (width, height) = (size.width, size.height);
            let pixels: Element<'_, Message> = image(handle.clone())
                .filter_method(sampling)
                .content_fit(iced::ContentFit::Fill)
                .width(width)
                .height(height)
                .into();
            let filter = frame.filter;
            let pixels = if filter == crate::presentation::DisplayFilter::Ravu {
                iced::widget::shader(crate::ravu::Frame {
                    image: handle.clone(),
                    warp: stretched,
                })
                .width(width)
                .height(height)
                .into()
            } else if filter == crate::presentation::DisplayFilter::Xbrz || stretched {
                let program = if filter == crate::presentation::DisplayFilter::Xbrz {
                    crate::xbrz::Frame::xbrz(handle.clone(), stretched)
                } else {
                    crate::xbrz::Frame::stretch(
                        handle.clone(),
                        sampling == iced::widget::image::FilterMethod::Linear,
                    )
                };
                iced::widget::shader(program)
                    .width(width)
                    .height(height)
                    .into()
            } else {
                pixels
            };
            let content = container(
                iced::widget::mouse_area(pixels)
                    .interaction(cursor)
                    .on_move(move |position| {
                        Message::PointerMoved(
                            ({
                                let fraction = position.x / width;
                                if stretched {
                                    crate::presentation::stretch_source(fraction)
                                } else {
                                    fraction
                                }
                            } * f32::from(pointer_width))
                            .clamp(0.0, f32::from(pointer_width - 1))
                                as u16,
                            (position.y / height * f32::from(pointer_height))
                                .clamp(0.0, f32::from(pointer_height - 1))
                                as u16,
                        )
                    })
                    .on_press(Message::PointerButton(0, true))
                    .on_release(Message::PointerButton(0, false))
                    .on_right_press(Message::PointerButton(1, true))
                    .on_right_release(Message::PointerButton(1, false))
                    .on_exit(Message::PointerLeft),
            );
            let screen: Element<'_, Message> = if app.session.status() == SessionStatus::Paused {
                stack![
                    content,
                    container(
                        row![
                            svg(svg::Handle::from_memory(Icon::Pause.bytes()))
                                .width(26)
                                .height(26),
                            text("PAUSED").size(28).font(iced::Font::MONOSPACE)
                        ]
                        .spacing(12)
                        .align_y(iced::Center)
                    )
                    .center_x(Fill)
                    .center_y(Fill)
                    .style(|_| container::Style {
                        background: Some(Color::from_rgba8(0, 0, 0, 0.55).into()),
                        ..Default::default()
                    })
                ]
                .into()
            } else {
                content.into()
            };
            if app.display_sizing.pixel_perfect
                && (width > available.width || height > available.height)
            {
                scrollable(
                    container(screen)
                        .center_x(available.width.max(width))
                        .center_y(available.height.max(height)),
                )
                .direction(scrollable::Direction::Both {
                    vertical: scrollable::Scrollbar::default(),
                    horizontal: scrollable::Scrollbar::default(),
                })
                .width(Fill)
                .height(Fill)
                .into()
            } else {
                container(screen).center_x(Fill).center_y(Fill).into()
            }
        })
        .into()
    } else {
        container(
            column![
                text("Ready to play").size(22),
                text("Press the play button to start.")
                    .size(14)
                    .color(MUTED)
            ]
            .spacing(8)
            .align_x(iced::Center),
        )
        .center_x(Fill)
        .center_y(Fill)
        .into()
    };
    let viewport = iced::widget::sensor(viewport)
        .on_show(|size| Message::DisplaySizing(crate::presentation::SizingMessage::Viewport(size)))
        .on_resize(|size| {
            Message::DisplaySizing(crate::presentation::SizingMessage::Viewport(size))
        });
    let fullscreen = app.display_sizing.fullscreen;
    container(viewport)
        .padding(if fullscreen { 0 } else { 8 })
        .width(Fill)
        .height(Fill)
        .style(move |_| container::Style {
            background: Some(if fullscreen { Color::BLACK } else { STAGE }.into()),
            ..Default::default()
        })
        .into()
}

#[cfg(not(target_arch = "wasm32"))]
fn speed_tier_table(app: &Doslet) -> Element<'_, Message> {
    let colors = [
        Color::from_rgb8(255, 179, 105),
        Color::from_rgb8(125, 194, 255),
        MUTED,
        Color::from_rgb8(194, 155, 255),
    ];
    let shares = app.tier_meter.shares;
    let heading = if app.gdb.is_some_and(|(_, connected)| connected) {
        "Speed tiers · GDB controls execution".into()
    } else if app.session.status() != SessionStatus::Running {
        "Speed tiers · paused".into()
    } else if let Some(s) = shares {
        format!("Speed tiers · last {:.1} s", s.seconds)
    } else {
        "Speed tiers · sampling…".into()
    };
    let mut table = row![
        column![
            text(" ").size(12),
            text("Steps").size(12),
            text("Guest time").size(12)
        ]
        .spacing(3)
    ]
    .spacing(12);
    for (i, label) in ["Hot", "Cold", "Grey", "IRQ"].into_iter().enumerate() {
        let value = |cpu: bool| {
            shares.map_or_else(
                || "—".into(),
                |s| format!("{:.1}%", if cpu { s.cpu[i] } else { s.guest[i] }),
            )
        };
        table = table.push(
            column![
                text(label).size(12).color(colors[i]),
                text(value(true)).size(12).color(colors[i]),
                text(value(false)).size(12).color(colors[i]),
            ]
            .width(52)
            .spacing(3),
        );
    }
    container(column![text(heading).size(11).color(FOREGROUND), table].spacing(5))
        .padding([8, 10])
        .style(|_| container::Style {
            background: Some(Color::from_rgba8(17, 18, 21, 0.84).into()),
            border: Border {
                radius: 7.0.into(),
                color: EDGE,
                width: 1.0,
            },
            ..Default::default()
        })
        .into()
}

pub(crate) fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(PANEL.into()),
        border: Border {
            color: EDGE,
            width: 1.0,
            radius: 10.0.into(),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.25),
            offset: iced::Vector::new(0.0, 2.0),
            blur_radius: 24.0,
        },
        ..Default::default()
    }
}

fn cpu_indicator(app: &Doslet, compact: bool) -> Element<'_, Message> {
    #[cfg(not(target_arch = "wasm32"))]
    let tier_targets = app
        .speed_tiers
        .filter(|tiers| tiers.enabled)
        .map(|tiers| (tiers.hot_mhz, tiers.cold_mhz, tiers.grey_mhz));
    #[cfg(target_arch = "wasm32")]
    let tier_targets = (app.cpu_settings.mode == crate::cpu_config::Mode::Tiers
        && app.cpu_tiers_available)
        .then_some((
            app.cpu_settings.hot,
            app.cpu_settings.cold,
            app.cpu_settings.grey,
        ));
    #[cfg(not(target_arch = "wasm32"))]
    let speed = if app.v86_host_clock {
        "v86 · MAX".into()
    } else if let Some((_, _, grey)) = tier_targets {
        format!("{grey} MHz · Auto")
    } else if app.analysis.controller_enabled {
        format!("{} MHz · Auto", app.analysis.effective_mhz)
    } else {
        format!("{} MHz", app.configurator.active.hardware.cpu_mhz)
    };
    #[cfg(target_arch = "wasm32")]
    let speed = if let Some((_, _, grey)) = tier_targets {
        format!("{grey} MHz · Auto")
    } else {
        format!("{} MHz", app.configurator.active.hardware.cpu_mhz)
    };
    let activity = app.cpu_activity;
    let usage = activity.map_or_else(|| "—".into(), |a| format!("{:.0}%", a.busy()));
    let colored = tier_targets.is_some();
    let shares = activity.map_or([0.0; 4], |a| a.shares);
    let colors = if colored {
        [HOT, COLD, MUTED, EDGE]
    } else {
        [ACCENT, ACCENT, ACCENT, EDGE]
    };
    let mut track = row![];
    if activity.is_some() {
        for (share, color) in shares.into_iter().zip(colors) {
            if share > 0.0 {
                track = track.push(
                    container(space::horizontal())
                        .width(iced::Length::FillPortion(
                            (share * 100.0).round().max(1.0) as u16
                        ))
                        .height(4)
                        .style(move |_| container::Style {
                            background: Some(color.into()),
                            ..Default::default()
                        }),
                );
            }
        }
    }
    let track_width = if compact { 104 } else { 146 };
    let track = container(track)
        .width(track_width)
        .height(4)
        .style(|_| container::Style {
            background: Some(EDGE.into()),
            ..Default::default()
        });
    let icon = svg(svg::Handle::from_memory(Icon::Cpu.bytes()))
        .width(20)
        .height(20)
        .style(|_, _| svg::Style {
            color: Some(ACCENT),
        });
    let control = button(
        row![
            icon,
            column![
                row![
                    text(speed).size(12),
                    space::horizontal(),
                    text(usage).size(12).color(MUTED)
                ],
                track
            ]
            .spacing(5)
            .width(track_width),
        ]
        .spacing(9)
        .align_y(iced::Center),
    )
    .padding([6, 8])
    .style(ghost)
    .on_press(Message::Ui(Action::Open(Panel::Cpu)));
    #[allow(unused_mut)]
    let mut description = column![
        text("Emulated CPU").size(13),
        text(if colored {
            "Executed work relative to the configured Grey speed."
        } else {
            "Guest time outside detected idle loops."
        })
        .size(12),
        text("Recent 250 ms of game time · not your computer's CPU usage.")
            .size(12)
            .color(MUTED),
    ]
    .spacing(6);
    if let Some((hot, cold, grey)) = tier_targets {
        description = description.push(
            text(format!(
                "Targets: Hot {} · Cold {} MHz · Grey / IRQ {} MHz",
                hot.map_or_else(|| "MAX".into(), |mhz| format!("{mhz} MHz")),
                cold,
                grey,
            ))
            .size(12),
        );
        if let Some(a) = activity {
            description = description.push(
                row![
                    text(format!("Hot {:.0}%", a.shares[0])).size(12).color(HOT),
                    text(format!("Cold {:.0}%", a.shares[1]))
                        .size(12)
                        .color(COLD),
                    text(format!("Grey {:.0}%", a.shares[2]))
                        .size(12)
                        .color(MUTED),
                    text(format!("Headroom {:.0}%", a.shares[3]))
                        .size(12)
                        .color(FOREGROUND),
                ]
                .spacing(10),
            );
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    if tier_targets.is_none() && app.analysis.controller_enabled {
        description = description.push(
            text(format!(
                "Configured ceiling: {} MHz",
                app.analysis.configured_mhz
            ))
            .size(12),
        );
    }
    tooltip(
        control,
        container(description).padding(12).style(panel),
        tooltip::Position::Top,
    )
    .into()
}

fn audio_spectrum(app: &Doslet) -> Element<'_, Message> {
    let mut bars = iced::widget::Row::new().spacing(2);
    for level in app.audio_spectrum() {
        let lit = (level * 8.0).round() as usize;
        let mut bar = iced::widget::Column::new().spacing(1);
        for segment in (0..8).rev() {
            let color = if segment >= lit {
                EDGE
            } else if segment >= 7 {
                Color::from_rgb8(226, 125, 113)
            } else if segment >= 5 {
                Color::from_rgb8(221, 192, 106)
            } else {
                Color::from_rgb8(117, 202, 145)
            };
            bar = bar.push(
                container(space::horizontal().width(4))
                    .height(1)
                    .style(move |_| container::Style {
                        background: Some(color.into()),
                        ..Default::default()
                    }),
            );
        }
        bars = bars.push(bar);
    }
    let h = app.configurator.active.hardware;
    let lamp = app
        .midi_activity
        .lamp(app.session.status() == SessionStatus::Running);
    let midi_color = if !h.mpu401 {
        EDGE
    } else {
        match lamp {
            crate::midi_activity::Lamp::Note => ACCENT,
            crate::midi_activity::Lamp::Data => Color::from_rgb8(228, 193, 133),
            crate::midi_activity::Lamp::Idle => MUTED,
        }
    };
    let sound = h.sound();
    let digital_badge = match sound {
        doslet_config::SoundCard::Sb16 => "SB16",
        doslet_config::SoundCard::Sb => "SB",
        doslet_config::SoundCard::Adlib | doslet_config::SoundCard::None => "",
    };
    let fm_badge = match sound {
        doslet_config::SoundCard::Sb16 => "OPL3",
        doslet_config::SoundCard::Sb | doslet_config::SoundCard::Adlib => "OPL2",
        doslet_config::SoundCard::None => "",
    };
    let badges = row![
        text(digital_badge)
            .size(10)
            .width(34)
            .color(if sound.has_dsp() { MUTED } else { EDGE }),
        text(fm_badge)
            .size(10)
            .width(34)
            .color(if sound.has_fm() { MUTED } else { EDGE }),
        text(if h.mpu401 { "MIDI" } else { "" })
            .size(10)
            .width(28)
            .color(midi_color),
    ];
    let icon = svg(svg::Handle::from_memory(Icon::Audio.bytes()))
        .width(24)
        .height(24)
        .style(|_, _| svg::Style {
            color: Some(ACCENT),
        });
    tooltip(
        button(
            row![icon, column![bars, badges].spacing(3).width(96)]
                .spacing(9)
                .align_y(iced::Center),
        )
        .padding([4, 6])
        .style(ghost)
        .on_press(Message::Ui(Action::Open(Panel::Audio))),
        container(
            column![
                text("Audio").size(12),
                text(app.guest_audio_description()).size(12).color(MUTED),
            ]
            .spacing(4),
        )
        .padding([6, 10])
        .style(panel),
        tooltip::Position::Top,
    )
    .into()
}

pub(crate) fn guest_pcm_description(rate: u32, bits: u8, channels: u8) -> String {
    if bits == 0 {
        "Guest PCM · not configured".into()
    } else {
        let khz = format!("{:.3}", f64::from(rate) / 1000.0);
        let khz = khz.trim_end_matches('0').trim_end_matches('.');
        format!(
            "Guest PCM · {khz} kHz · {bits}-bit · {}",
            if channels == 2 { "Stereo" } else { "Mono" },
        )
    }
}

pub(crate) fn primary(_: &Theme, status: button::Status) -> button::Style {
    let disabled = status == button::Status::Disabled;
    button::Style {
        background: Some(
            if disabled {
                HOVER
            } else if status == button::Status::Hovered {
                Color::from_rgb8(207, 216, 229)
            } else {
                ACCENT
            }
            .into(),
        ),
        text_color: if disabled { MUTED } else { INK },
        border: Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(crate) fn secondary(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(
            if status == button::Status::Hovered {
                HOVER
            } else {
                PANEL
            }
            .into(),
        ),
        text_color: if status == button::Status::Disabled {
            MUTED
        } else {
            FOREGROUND
        },
        border: Border {
            color: EDGE,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

pub(crate) fn ghost(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: (status == button::Status::Hovered).then_some(HOVER.into()),
        text_color: if status == button::Status::Hovered {
            FOREGROUND
        } else {
            MUTED
        },
        border: Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(crate) fn segment(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    if selected {
        button::Style {
            background: Some(Color { a: 0.10, ..ACCENT }.into()),
            text_color: ACCENT,
            border: Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    } else {
        ghost(theme, status)
    }
}

fn shorten(value: &str, length: usize) -> String {
    let mut chars = value.chars();
    let mut short: String = chars.by_ref().take(length).collect();
    if chars.next().is_some() {
        short.push('…');
    }
    short
}

fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Ready => "Ready",
        SessionStatus::Running => "Running",
        SessionStatus::Paused => "Paused",
        SessionStatus::Exited(_) => "Finished",
        SessionStatus::Halted => "Halted",
        SessionStatus::Faulted => "Stopped",
    }
}
