//! Mapping a source tool's RGB colour to the nearest Dbento colour tag.

use crate::Color;

/// What an RGB colour maps to: a tag, no colour at all, or nothing safe to pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorMatch {
    Tag(Color),
    Blank,
    Unknown,
}

/// The tag for an RGB colour, by HSV: near-black and greys are gray, near-white is blank, and
/// the hue picks the rest. Only a saturated red hue is red, so a pastel pink never claims
/// production.
pub(crate) fn from_rgb(r: u8, g: u8, b: u8) -> ColorMatch {
    let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
    let max = r.max(g).max(b);
    let delta = max - r.min(g).min(b);
    let v = max / 255.0;
    if v < 0.15 {
        return ColorMatch::Tag(Color::Gray);
    }
    let s = delta / max;
    if s < 0.12 {
        return if v > 0.94 {
            ColorMatch::Blank
        } else {
            ColorMatch::Tag(Color::Gray)
        };
    }
    let sector = if max == r {
        (g - b) / delta
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    let hue = (60.0 * sector).rem_euclid(360.0);
    ColorMatch::Tag(match hue {
        h if !(15.0..345.0).contains(&h) => {
            if s < 0.5 {
                return ColorMatch::Unknown;
            }
            Color::Red
        }
        h if h < 45.0 => Color::Orange,
        h if h < 70.0 => Color::Yellow,
        h if h < 165.0 => Color::Green,
        h if h < 200.0 => Color::Teal,
        h if h < 255.0 => Color::Blue,
        _ => Color::Purple,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_maps_to_the_nearest_tag_by_hue() {
        use ColorMatch::{Blank, Tag, Unknown};
        for ((r, g, b), want) in [
            ((255, 0, 0), Tag(Color::Red)),
            ((255, 0, 30), Tag(Color::Red)),
            ((250, 207, 207), Unknown),
            ((200, 60, 60), Tag(Color::Red)),
            ((255, 128, 0), Tag(Color::Orange)),
            ((255, 255, 0), Tag(Color::Yellow)),
            ((214, 250, 207), Tag(Color::Green)),
            ((0, 128, 128), Tag(Color::Teal)),
            ((0, 0, 255), Tag(Color::Blue)),
            ((128, 0, 128), Tag(Color::Purple)),
            ((255, 0, 200), Tag(Color::Purple)),
            ((255, 255, 255), Blank),
            ((245, 245, 245), Blank),
            ((128, 128, 128), Tag(Color::Gray)),
            ((10, 10, 10), Tag(Color::Gray)),
        ] {
            assert_eq!(from_rgb(r, g, b), want, "rgb({r},{g},{b})");
        }
    }
}
