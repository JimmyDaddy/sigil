use ratatui::style::Color;

pub(crate) fn contrast_ratio(foreground: Color, background: Color) -> Option<f32> {
    let foreground = relative_luminance(foreground)?;
    let background = relative_luminance(background)?;
    let (lighter, darker) = if foreground >= background {
        (foreground, background)
    } else {
        (background, foreground)
    };
    Some((lighter + 0.05) / (darker + 0.05))
}

fn relative_luminance(color: Color) -> Option<f32> {
    let (red, green, blue) = rgb(color)?;
    Some(0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue))
}

pub(crate) fn rgb(color: Color) -> Option<(f32, f32, f32)> {
    match color {
        Color::Rgb(red, green, blue) => Some((
            red as f32 / 255.0,
            green as f32 / 255.0,
            blue as f32 / 255.0,
        )),
        Color::Black => Some((0.0, 0.0, 0.0)),
        Color::White => Some((1.0, 1.0, 1.0)),
        Color::Red => Some((0.5, 0.0, 0.0)),
        Color::Green => Some((0.0, 0.5, 0.0)),
        Color::Yellow => Some((0.5, 0.5, 0.0)),
        Color::Blue => Some((0.0, 0.0, 0.5)),
        Color::Magenta => Some((0.5, 0.0, 0.5)),
        Color::Cyan => Some((0.0, 0.5, 0.5)),
        Color::Gray => Some((0.5, 0.5, 0.5)),
        Color::DarkGray => Some((0.25, 0.25, 0.25)),
        Color::LightRed => Some((1.0, 0.0, 0.0)),
        Color::LightGreen => Some((0.0, 1.0, 0.0)),
        Color::LightYellow => Some((1.0, 1.0, 0.0)),
        Color::LightBlue => Some((0.0, 0.0, 1.0)),
        Color::LightMagenta => Some((1.0, 0.0, 1.0)),
        Color::LightCyan => Some((0.0, 1.0, 1.0)),
        Color::Indexed(_) | Color::Reset => None,
    }
}

fn linear(channel: f32) -> f32 {
    if channel <= 0.03928 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}
