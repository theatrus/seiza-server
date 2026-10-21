//! Find the picture inside an upload whose edges are flat bands.
//!
//! A phone screenshot of a photo carries the bars the phone drew above and
//! below it, and a registered stack can carry blank edges. A plate solution
//! extends over those bands as over the sky, so catalog objects and
//! constellation figures would be drawn on them. The bands are rows or
//! columns of one colour running in from an edge; the picture is what they
//! leave.

use crate::models::ContentRegion;
use image::{DynamicImage, GenericImageView};

/// How far, per channel, a pixel may stray from its band's colour: enough
/// for JPEG noise in a black bar, far less than a star or sky noise spans.
const TOLERANCE: u8 = 10;
/// Thinner bands are left alone; a line of flat pixels at an edge is not
/// worth reporting.
const MIN_BAND_PX: u32 = 2;
/// The picture must keep at least this share of each side. An image almost
/// all one colour is not a picture with bands.
const MIN_CONTENT_FRACTION: f64 = 0.25;

/// The picture inside `image`, or `None` when no edge has a band.
pub fn find(image: &DynamicImage) -> Option<ContentRegion> {
    let (width, height) = image.dimensions();
    if width < 4 * MIN_BAND_PX || height < 4 * MIN_BAND_PX {
        return None;
    }
    let pixel = |x: u32, y: u32| {
        let [r, g, b, _] = image.get_pixel(x, y).0;
        [r, g, b]
    };
    let top = band(height, |line| (0..width).map(move |x| pixel(x, line)));
    let bottom = band(height - top, |line| {
        (0..width).map(move |x| pixel(x, height - 1 - line))
    });
    let rows = top..height - bottom;
    let left = band(width, |line| rows.clone().map(move |y| pixel(line, y)));
    let right = band(width - left, |line| {
        rows.clone().map(move |y| pixel(width - 1 - line, y))
    });
    let trim = |band: u32| if band < MIN_BAND_PX { 0 } else { band };
    let (top, bottom, left, right) = (trim(top), trim(bottom), trim(left), trim(right));
    if top + bottom + left + right == 0 {
        return None;
    }
    let region = ContentRegion {
        x: left,
        y: top,
        width: width - left - right,
        height: height - top - bottom,
    };
    let enough = |part: u32, whole: u32| part as f64 >= whole as f64 * MIN_CONTENT_FRACTION;
    (enough(region.width, width) && enough(region.height, height)).then_some(region)
}

/// The number of lines, of at most `lines`, from an edge inward whose
/// pixels all lie within [`TOLERANCE`] of the edge's first pixel.
fn band<I>(lines: u32, line: impl Fn(u32) -> I) -> u32
where
    I: Iterator<Item = [u8; 3]>,
{
    let Some(colour) = line(0).next() else {
        return 0;
    };
    let near = |pixel: [u8; 3]| {
        pixel
            .iter()
            .zip(colour)
            .all(|(&value, reference)| value.abs_diff(reference) <= TOLERANCE)
    };
    (0..lines)
        .take_while(|&index| line(index).all(near))
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma, Rgb, RgbImage};

    /// A noisy star field: no two neighbours alike, as in a real sky.
    fn sky(x: u32, y: u32) -> [u8; 3] {
        let value = 40 + ((x * 7919 + y * 104_729) % 61) as u8;
        [value, value.saturating_add(5), value.saturating_sub(3)]
    }

    #[test]
    fn a_screenshot_s_bars_are_left_out() {
        // A 1170 x 2532 phone screenshot: black bars of 388 and 378 rows,
        // with a little JPEG noise in them, around a photo.
        let (width, height) = (117, 253);
        let image = RgbImage::from_fn(width, height, |x, y| {
            if y < 39 || y >= height - 38 {
                Rgb([((x + y) % 4) as u8, 0, ((x * y) % 3) as u8])
            } else {
                Rgb(sky(x, y))
            }
        });
        assert_eq!(
            find(&DynamicImage::ImageRgb8(image)),
            Some(ContentRegion {
                x: 0,
                y: 39,
                width: 117,
                height: 176
            })
        );
    }

    #[test]
    fn a_stack_s_blank_edges_on_every_side_are_left_out() {
        let image = GrayImage::from_fn(200, 150, |x, y| {
            if (12..190).contains(&x) && (5..143).contains(&y) {
                Luma([sky(x, y)[0]])
            } else {
                Luma([0])
            }
        });
        assert_eq!(
            find(&DynamicImage::ImageLuma8(image)),
            Some(ContentRegion {
                x: 12,
                y: 5,
                width: 178,
                height: 138
            })
        );
    }

    #[test]
    fn a_white_bar_counts_as_a_band_too() {
        let image = RgbImage::from_fn(100, 100, |x, y| {
            if x >= 80 {
                Rgb([250, 252, 255])
            } else {
                Rgb(sky(x, y))
            }
        });
        assert_eq!(
            find(&DynamicImage::ImageRgb8(image)),
            Some(ContentRegion {
                x: 0,
                y: 0,
                width: 80,
                height: 100
            })
        );
    }

    #[test]
    fn a_picture_to_its_edges_has_no_region() {
        let image = RgbImage::from_fn(120, 80, |x, y| Rgb(sky(x, y)));
        assert_eq!(find(&DynamicImage::ImageRgb8(image)), None);
    }

    #[test]
    fn one_flat_line_at_an_edge_is_not_a_band() {
        let image = RgbImage::from_fn(120, 80, |x, y| {
            if y == 0 {
                Rgb([0, 0, 0])
            } else {
                Rgb(sky(x, y))
            }
        });
        assert_eq!(find(&DynamicImage::ImageRgb8(image)), None);
    }

    #[test]
    fn a_star_on_the_edge_ends_the_band_there() {
        let image = RgbImage::from_fn(120, 80, |x, y| match (x, y) {
            (60, 3) => Rgb([255, 255, 255]),
            (_, 0..10) => Rgb([0, 0, 0]),
            _ => Rgb(sky(x, y)),
        });
        assert_eq!(
            find(&DynamicImage::ImageRgb8(image)),
            Some(ContentRegion {
                x: 0,
                y: 3,
                width: 120,
                height: 77
            })
        );
    }

    #[test]
    fn an_image_nearly_all_one_colour_has_no_region() {
        let image = RgbImage::from_fn(100, 100, |x, y| {
            if (45..55).contains(&y) {
                Rgb(sky(x, y))
            } else {
                Rgb([0, 0, 0])
            }
        });
        assert_eq!(find(&DynamicImage::ImageRgb8(image)), None);
        let flat = RgbImage::from_pixel(100, 100, Rgb([0, 0, 0]));
        assert_eq!(find(&DynamicImage::ImageRgb8(flat)), None);
    }
}
