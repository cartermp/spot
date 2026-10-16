use iced::widget::svg;

fn handle(svg_str: &'static str) -> svg::Handle {
    svg::Handle::from_memory(svg_str.as_bytes())
}

/// Groove 5-bar rounded green waveform mark in the top-left of the sidebar.
#[allow(dead_code)]
pub fn groove_logo() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 28 28" fill="none">
            <rect x="2" y="10" width="3.2" height="8" rx="1.6" fill="#14964B"/>
            <rect x="7.2" y="6" width="3.2" height="16" rx="1.6" fill="#14964B"/>
            <rect x="12.4" y="2.5" width="3.2" height="23" rx="1.6" fill="#14964B"/>
            <rect x="17.6" y="7" width="3.2" height="14" rx="1.6" fill="#14964B"/>
            <rect x="22.8" y="10.5" width="3.2" height="7" rx="1.6" fill="#14964B"/>
        </svg>"##,
    )
}

/// Small 3-bar green playing indicator next to the active track in "Up Next".
pub fn active_bars() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none">
            <rect x="2" y="6" width="2.5" height="8" rx="1.2" fill="#14964B"/>
            <rect x="6.75" y="2.5" width="2.5" height="11.5" rx="1.2" fill="#14964B"/>
            <rect x="11.5" y="5" width="2.5" height="9" rx="1.2" fill="#14964B"/>
        </svg>"##,
    )
}

pub fn nav_home() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#2C3036" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <path d="M3 10.5L12 3l9 7.5"/>
            <path d="M5.5 9.5V20a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1V9.5"/>
        </svg>"##,
    )
}

pub fn nav_search() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#2C3036" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="11" cy="11" r="7"/>
            <line x1="20" y1="20" x2="16.2" y2="16.2"/>
        </svg>"##,
    )
}

pub fn nav_podcasts() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#2C3036" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <rect x="9" y="3" width="6" height="10" rx="3"/>
            <path d="M5.5 10.5a6.5 6.5 0 0 0 13 0"/>
            <line x1="12" y1="17" x2="12" y2="21"/>
            <line x1="8.5" y1="21" x2="15.5" y2="21"/>
        </svg>"##,
    )
}

pub fn nav_now_playing() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#17191C" stroke-width="2.1" stroke-linecap="round">
            <line x1="5" y1="19" x2="5" y2="11"/>
            <line x1="9.5" y1="19" x2="9.5" y2="5"/>
            <line x1="14" y1="19" x2="14" y2="13"/>
            <line x1="18.5" y1="19" x2="18.5" y2="8"/>
        </svg>"##,
    )
}

pub fn nav_library() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#2C3036" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <rect x="4" y="4" width="3.5" height="16" rx="1"/>
            <rect x="9.5" y="4" width="3.5" height="16" rx="1"/>
            <path d="M15.5 4.5l4 15-3.2.8-4-15z"/>
        </svg>"##,
    )
}

pub fn plus_icon() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="none" stroke="#5A5F66" stroke-width="1.7" stroke-linecap="round">
            <line x1="10" y1="4" x2="10" y2="16"/>
            <line x1="4" y1="10" x2="16" y2="10"/>
        </svg>"##,
    )
}

pub fn spotify_badge() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none">
            <circle cx="12" cy="12" r="11" fill="#14964B"/>
            <path d="M16.9 10.6C13.7 8.8 8.5 8.6 5.5 9.5" stroke="#FFFFFF" stroke-width="1.9" stroke-linecap="round"/>
            <path d="M15.9 13.7C13.3 12.2 9.1 12.0 6.5 12.8" stroke="#FFFFFF" stroke-width="1.6" stroke-linecap="round"/>
            <path d="M14.9 16.6C12.7 15.3 9.5 15.2 7.4 15.8" stroke="#FFFFFF" stroke-width="1.4" stroke-linecap="round"/>
        </svg>"##,
    )
}

pub fn heart_icon(liked: bool) -> svg::Handle {
    if liked {
        handle(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#14964B" stroke="#14964B" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"/>
            </svg>"##,
        )
    } else {
        handle(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#363A40" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
                <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"/>
            </svg>"##,
        )
    }
}

pub fn dots_horizontal() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#363A40">
            <circle cx="5.5" cy="12" r="1.6"/>
            <circle cx="12" cy="12" r="1.6"/>
            <circle cx="18.5" cy="12" r="1.6"/>
        </svg>"##,
    )
}

pub fn shuffle_icon(active: bool) -> svg::Handle {
    if active {
        handle(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#14964B" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
                <polyline points="16 3 21 3 21 8"/>
                <line x1="4" y1="20" x2="21" y2="3"/>
                <polyline points="21 16 21 21 16 21"/>
                <line x1="15" y1="15" x2="21" y2="21"/>
                <line x1="4" y1="4" x2="9" y2="9"/>
            </svg>"##,
        )
    } else {
        handle(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#22252A" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                <polyline points="16 3 21 3 21 8"/>
                <line x1="4" y1="20" x2="21" y2="3"/>
                <polyline points="21 16 21 21 16 21"/>
                <line x1="15" y1="15" x2="21" y2="21"/>
                <line x1="4" y1="4" x2="9" y2="9"/>
            </svg>"##,
        )
    }
}

pub fn prev_icon() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#22252A">
            <rect x="5" y="5" width="2.4" height="14" rx="1"/>
            <path d="M18.5 5.4a1 1 0 0 1 1.5.86v11.48a1 1 0 0 1-1.5.86l-9.8-5.74a1 1 0 0 1 0-1.72l9.8-5.74z"/>
        </svg>"##,
    )
}

pub fn next_icon() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#22252A">
            <rect x="16.6" y="5" width="2.4" height="14" rx="1"/>
            <path d="M5.5 5.4a1 1 0 0 0-1.5.86v11.48a1 1 0 0 0 1.5.86l9.8-5.74a1 1 0 0 0 0-1.72L5.5 5.4z"/>
        </svg>"##,
    )
}

pub fn play_icon() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#FFFFFF">
            <path d="M8 5.2a1 1 0 0 1 1.52-.85l10.2 6.8a1 1 0 0 1 0 1.7l-10.2 6.8A1 1 0 0 1 8 18.8V5.2z"/>
        </svg>"##,
    )
}

pub fn pause_icon() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="#FFFFFF">
            <rect x="7" y="5" width="3.6" height="14" rx="1.3"/>
            <rect x="13.4" y="5" width="3.6" height="14" rx="1.3"/>
        </svg>"##,
    )
}

pub fn repeat_icon(active: bool) -> svg::Handle {
    if active {
        handle(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#14964B" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
                <polyline points="17 1 21 5 17 9"/>
                <path d="M3 11V9a4 4 0 0 1 4-4h14"/>
                <polyline points="7 23 3 19 7 15"/>
                <path d="M21 13v2a4 4 0 0 1-4 4H3"/>
            </svg>"##,
        )
    } else {
        handle(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#22252A" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                <polyline points="17 1 21 5 17 9"/>
                <path d="M3 11V9a4 4 0 0 1 4-4h14"/>
                <polyline points="7 23 3 19 7 15"/>
                <path d="M21 13v2a4 4 0 0 1-4 4H3"/>
            </svg>"##,
        )
    }
}

pub fn volume_icon() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#5A5F66" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" fill="#5A5F66"/>
            <path d="M15.54 8.46a5 5 0 0 1 0 7.07"/>
            <path d="M19.07 4.93a10 10 0 0 1 0 14.14"/>
        </svg>"##,
    )
}

pub fn airplay_speaker_icon() -> svg::Handle {
    handle(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#454950" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <path d="M6 17a7 7 0 1 1 12 0"/>
            <path d="M8.8 14.5a3.5 3.5 0 1 1 6.4 0"/>
            <path d="M12 14l-4 6h8l-4-6z" fill="#454950"/>
        </svg>"##,
    )
}
