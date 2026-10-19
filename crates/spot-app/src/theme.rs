use iced::widget::{button, container, pick_list, slider};
use iced::{Background, Border, Color, Font, Shadow, Theme, Vector};

/// Embedded Instrument Serif font bytes (matches the editorial serif in the Spot UI).
pub const INSTRUMENT_SERIF_BYTES: &[u8] =
    include_bytes!("../../../assets/fonts/InstrumentSerif-Regular.ttf");

/// Editorial serif font used for "Spot", track titles ("Higher Ground"), and "Up Next".
pub const SERIF_FONT: Font = Font::with_name("Instrument Serif");

/// Palette constants for the Spot UI.
pub mod palette {
    use iced::Color;

    /// Left sidebar translucent warm stone surface (`#E5E6EA`).
    pub const SIDEBAR_BG: Color = Color::from_rgb(0.898, 0.902, 0.918);
    /// Active navigation pill background (`#F5F6F8`).
    pub const SIDEBAR_ACTIVE_PILL: Color = Color::from_rgb(0.961, 0.965, 0.973);
    /// Hover pill in sidebar.
    pub const SIDEBAR_HOVER_PILL: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.45);

    /// Center "Now Playing" warm alabaster canvas (`#F4F4F2`).
    pub const CENTER_BG: Color = Color::from_rgb(0.957, 0.957, 0.949);

    /// Right "Up Next" queue background (`#EFEFED`).
    pub const QUEUE_BG: Color = Color::from_rgb(0.937, 0.937, 0.929);

    /// Subtle hairline border between panes (`rgba(0, 0, 0, 0.065)`).
    pub const DIVIDER: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.065);

    /// Primary charcoal ink (`#17191C`).
    pub const INK_PRIMARY: Color = Color::from_rgb(0.090, 0.098, 0.110);
    /// Secondary muted slate ink (`#686D76`).
    pub const INK_SECONDARY: Color = Color::from_rgb(0.408, 0.427, 0.463);
    /// Subtle caption/timestamp ink (`#7B8089`).
    pub const INK_MUTED: Color = Color::from_rgb(0.482, 0.502, 0.537);

    /// Signature Spot emerald accent (`#14964B`).
    pub const EMERALD: Color = Color::from_rgb(0.078, 0.588, 0.294);
    /// Deeper forest scrubber track fill (`#1C6B4A`).
    pub const SCRUBBER_FILL: Color = Color::from_rgb(0.110, 0.420, 0.290);
    /// Unfilled slider track (`#DCDEE0`).
    pub const SLIDER_TRACK: Color = Color::from_rgb(0.863, 0.871, 0.878);

    /// Deep forest charcoal circular Play/Pause button (`#0E271D`).
    pub const PLAY_BUTTON_BG: Color = Color::from_rgb(0.055, 0.153, 0.114);
    pub const PLAY_BUTTON_HOVER: Color = Color::from_rgb(0.082, 0.215, 0.161);

    /// Output device pill surface (`#EAEAE7`).
    pub const DEVICE_PILL_BG: Color = Color::from_rgb(0.918, 0.918, 0.906);
}

pub fn sidebar_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(palette::SIDEBAR_BG)),
        border: Border {
            color: palette::DIVIDER,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

pub fn center_stage_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(palette::CENTER_BG)),
        ..Default::default()
    }
}

pub fn queue_pane_container(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(palette::QUEUE_BG)),
        border: Border {
            color: palette::DIVIDER,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

pub fn album_art_shadow_card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::WHITE)),
        border: Border {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.04),
            width: 1.0,
            radius: 16.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.06, 0.11, 0.14, 0.18),
            offset: Vector::new(0.0, 14.0),
            blur_radius: 34.0,
        },
        ..Default::default()
    }
}

pub fn nav_pill_button(is_active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let bg = if is_active {
            Some(Background::Color(palette::SIDEBAR_ACTIVE_PILL))
        } else {
            match status {
                button::Status::Hovered | button::Status::Pressed => {
                    Some(Background::Color(palette::SIDEBAR_HOVER_PILL))
                }
                _ => None,
            }
        };

        button::Style {
            background: bg,
            text_color: palette::INK_PRIMARY,
            border: Border {
                color: if is_active {
                    Color::from_rgba(0.0, 0.0, 0.0, 0.04)
                } else {
                    Color::TRANSPARENT
                },
                width: 1.0,
                radius: 10.0.into(),
            },
            shadow: if is_active {
                Shadow {
                    color: Color::from_rgba(0.0, 0.0, 0.0, 0.04),
                    offset: Vector::new(0.0, 2.0),
                    blur_radius: 6.0,
                }
            } else {
                Shadow::default()
            },
        }
    }
}

pub fn subtle_row_button(is_selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let bg = match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.045)))
            }
            _ if is_selected => Some(Background::Color(Color::from_rgba(0.078, 0.588, 0.294, 0.07))),
            _ => None,
        };
        button::Style {
            background: bg,
            text_color: palette::INK_PRIMARY,
            border: Border {
                radius: 9.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

pub fn icon_ghost_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.055))),
        button::Status::Pressed => Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.09))),
        _ => None,
    };
    button::Style {
        background: bg,
        text_color: palette::INK_PRIMARY,
        border: Border {
            radius: 20.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn play_circle_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg_color = match status {
        button::Status::Hovered | button::Status::Pressed => palette::PLAY_BUTTON_HOVER,
        _ => palette::PLAY_BUTTON_BG,
    };
    button::Style {
        background: Some(Background::Color(bg_color)),
        text_color: Color::WHITE,
        border: Border {
            radius: 28.0.into(),
            ..Default::default()
        },
        shadow: Shadow {
            color: Color::from_rgba(0.04, 0.14, 0.10, 0.26),
            offset: Vector::new(0.0, 6.0),
            blur_radius: 16.0,
        },
    }
}

pub fn seek_slider_style(_theme: &Theme, _status: slider::Status) -> slider::Style {
    slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(palette::SCRUBBER_FILL),
                Background::Color(palette::SLIDER_TRACK),
            ),
            width: 3.5,
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 6.5 },
            background: Background::Color(Color::WHITE),
            border_width: 1.5,
            border_color: Color::from_rgba(0.11, 0.42, 0.29, 0.45),
        },
    }
}

pub fn volume_slider_style(_theme: &Theme, _status: slider::Status) -> slider::Style {
    slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(Color::from_rgb(0.45, 0.48, 0.51)),
                Background::Color(palette::SLIDER_TRACK),
            ),
            width: 3.0,
            border: Border {
                radius: 2.0.into(),
                ..Default::default()
            },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 6.0 },
            background: Background::Color(Color::WHITE),
            border_width: 1.0,
            border_color: Color::from_rgba(0.0, 0.0, 0.0, 0.20),
        },
    }
}

pub fn device_pick_list_style(_theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let bg = match status {
        pick_list::Status::Hovered | pick_list::Status::Opened => {
            Color::from_rgb(0.890, 0.890, 0.878)
        }
        _ => palette::DEVICE_PILL_BG,
    };
    pick_list::Style {
        text_color: palette::INK_SECONDARY,
        placeholder_color: palette::INK_MUTED,
        handle_color: palette::INK_SECONDARY,
        background: Background::Color(bg),
        border: Border {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.05),
            width: 1.0,
            radius: 14.0.into(),
        },
    }
}

pub fn artist_bio_card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.72))),
        border: Border {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.055),
            width: 1.0,
            radius: 14.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.04, 0.07, 0.09, 0.05),
            offset: Vector::new(0.0, 4.0),
            blur_radius: 14.0,
        },
        ..Default::default()
    }
}

pub fn recent_search_chip_card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.70))),
        border: Border {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.055),
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }
}
