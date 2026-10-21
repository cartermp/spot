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

    /// Signature Spot emerald accent (`#14964B`) — used for Artists & primary active states.
    pub const EMERALD: Color = Color::from_rgb(0.078, 0.588, 0.294);
    /// Distinct warm copper-amber accent (`#B55811`) — used for Songs in Search & Search History.
    pub const SONG_ACCENT: Color = Color::from_rgb(0.710, 0.345, 0.067);
    /// Soft warm amber badge pill fill for Song chips.
    pub const SONG_BADGE_BG: Color = Color::from_rgba(0.710, 0.345, 0.067, 0.13);
    /// Soft emerald badge pill fill for Artist chips.
    pub const ARTIST_BADGE_BG: Color = Color::from_rgba(0.078, 0.588, 0.294, 0.12);
    /// Distinct deep indigo-slate accent (`#485EB0`) — used for Podcasts in Search & Search History.
    pub const PODCAST_ACCENT: Color = Color::from_rgb(0.282, 0.369, 0.690);
    /// Soft indigo badge pill fill for Podcast chips.
    pub const PODCAST_BADGE_BG: Color = Color::from_rgba(0.282, 0.369, 0.690, 0.13);
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

pub fn recent_search_song_chip_card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba(1.0, 0.985, 0.965, 0.88))),
        border: Border {
            color: Color::from_rgba(0.710, 0.345, 0.067, 0.28),
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }
}

pub fn recent_search_artist_chip_card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba(0.965, 0.995, 0.978, 0.88))),
        border: Border {
            color: Color::from_rgba(0.078, 0.588, 0.294, 0.26),
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }
}

pub fn recent_search_podcast_chip_card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba(0.968, 0.975, 0.996, 0.88))),
        border: Border {
            color: Color::from_rgba(0.282, 0.369, 0.690, 0.25),
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }
}

pub fn search_group_column_card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba(1.0, 1.0, 1.0, 0.56))),
        border: Border {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.055),
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

/// Minimal vertical scrollbar geometry (`7px` scroller inside an `11px` hover target).
pub fn elegant_scrollbar_direction() -> iced::widget::scrollable::Direction {
    iced::widget::scrollable::Direction::Vertical(
        iced::widget::scrollable::Scrollbar::new()
            .width(7.0)
            .scroller_width(7.0)
            .margin(2.0),
    )
}

/// Whisper-thin disappearing scrollbar that:
/// - Is completely invisible when not scrolling and not hovered (`scroll_alpha == 0.0`)
/// - Appears as a barely-there `2.5px` translucent sliver while scrolling (via transparent GPU SDF inset border)
/// - Expands to a `7.0px` pill with a subtle track rail when hovered or dragged
pub fn elegant_scrollbar(
    scroll_alpha: f32,
) -> impl Fn(&Theme, iced::widget::scrollable::Status) -> iced::widget::scrollable::Style {
    move |_theme, status| {
        let (scrollbar_hovered, scrollbar_dragged) = match status {
            iced::widget::scrollable::Status::Hovered {
                is_horizontal_scrollbar_hovered,
                is_vertical_scrollbar_hovered,
            } => (
                is_horizontal_scrollbar_hovered || is_vertical_scrollbar_hovered,
                false,
            ),
            iced::widget::scrollable::Status::Dragged {
                is_horizontal_scrollbar_dragged,
                is_vertical_scrollbar_dragged,
            } => (
                true,
                is_horizontal_scrollbar_dragged || is_vertical_scrollbar_dragged,
            ),
            iced::widget::scrollable::Status::Active => (false, false),
        };

        let clamped_alpha = scroll_alpha.clamp(0.0, 1.0);

        let rail = if scrollbar_hovered || scrollbar_dragged {
            // Expanded pill on hover/drag (`7.0px` wide)
            let thumb_alpha = if scrollbar_dragged { 0.44 } else { 0.30 };
            iced::widget::scrollable::Rail {
                background: Some(Background::Color(Color::from_rgba(0.11, 0.14, 0.16, 0.04))),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: 3.5.into(),
                },
                scroller: iced::widget::scrollable::Scroller {
                    color: Color::from_rgba(0.11, 0.14, 0.16, thumb_alpha),
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: 3.5.into(),
                    },
                },
            }
        } else if clamped_alpha > 0.005 {
            // Barely-there `2.5px` sliver while scrolling (`7.0 - 2 * 2.25 = 2.5px` interior via transparent border)
            iced::widget::scrollable::Rail {
                background: None,
                border: Border::default(),
                scroller: iced::widget::scrollable::Scroller {
                    color: Color::from_rgba(0.11, 0.14, 0.16, 0.18 * clamped_alpha),
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 2.25,
                        radius: 3.5.into(),
                    },
                },
            }
        } else {
            // Completely hidden when idle and not hovering over the scrollbar
            iced::widget::scrollable::Rail {
                background: None,
                border: Border::default(),
                scroller: iced::widget::scrollable::Scroller {
                    color: Color::TRANSPARENT,
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: 3.5.into(),
                    },
                },
            }
        };

        iced::widget::scrollable::Style {
            container: container::Style::default(),
            vertical_rail: rail,
            horizontal_rail: rail,
            gap: None,
        }
    }
}

pub fn update_modal_backdrop(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba(0.06, 0.08, 0.10, 0.44))),
        ..Default::default()
    }
}

pub fn update_modal_card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgb(0.988, 0.988, 0.980))),
        border: Border {
            color: Color::from_rgba(0.08, 0.28, 0.19, 0.16),
            width: 1.0,
            radius: 20.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.03, 0.07, 0.09, 0.26),
            offset: Vector::new(0.0, 18.0),
            blur_radius: 46.0,
        },
        ..Default::default()
    }
}

pub fn update_modal_version_strip(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgb(0.949, 0.955, 0.946))),
        border: Border {
            color: Color::from_rgba(0.078, 0.588, 0.294, 0.18),
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

pub fn update_modal_notes_box(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::WHITE)),
        border: Border {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.065),
            width: 1.0,
            radius: 14.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.04, 0.06, 0.08, 0.035),
            offset: Vector::new(0.0, 3.0),
            blur_radius: 10.0,
        },
        ..Default::default()
    }
}

pub fn update_modal_verified_banner(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgba(0.078, 0.588, 0.294, 0.10))),
        border: Border {
            color: Color::from_rgba(0.078, 0.588, 0.294, 0.30),
            width: 1.0,
            radius: 11.0.into(),
        },
        ..Default::default()
    }
}

pub fn update_modal_primary_btn(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => Color::from_rgb(0.118, 0.388, 0.278),
        button::Status::Pressed => Color::from_rgb(0.082, 0.290, 0.204),
        button::Status::Disabled => Color::from_rgba(0.102, 0.337, 0.239, 0.45),
        _ => Color::from_rgb(0.102, 0.337, 0.239),
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: Color::WHITE,
        border: Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.14),
            width: 1.0,
            radius: 11.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.04, 0.16, 0.11, 0.22),
            offset: Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
    }
}

pub fn update_modal_secondary_btn(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => Color::from_rgb(0.914, 0.918, 0.906),
        button::Status::Pressed => Color::from_rgb(0.878, 0.882, 0.870),
        _ => Color::from_rgb(0.945, 0.945, 0.935),
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: palette::INK_PRIMARY,
        border: Border {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.08),
            width: 1.0,
            radius: 11.0.into(),
        },
        shadow: Shadow::default(),
    }
}


