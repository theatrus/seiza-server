use crate::models::{OverlayObject, SolutionResponse};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use seiza::wcs::Wcs;
use std::fmt::Write;

#[derive(Debug, Clone, Copy)]
pub struct OverlayOptions {
    pub objects: bool,
    pub grid: bool,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            objects: true,
            grid: false,
        }
    }
}

/// Render a self-contained solution overlay. The marker vocabulary is adapted
/// from Tenrankai's Apache-2.0 `AstroOverlay` component so both Seiza-based
/// applications present catalog objects consistently.
pub fn render_svg(
    solution: &SolutionResponse,
    preview_png: &Bytes,
    options: OverlayOptions,
) -> String {
    let width = solution.image_width;
    let height = solution.image_height;
    let encoded = STANDARD.encode(preview_png);
    let mut objects = String::new();
    if options.objects {
        for object in &solution.objects {
            render_object(&mut objects, object, width as f64, height as f64);
        }
    }
    let grid = if options.grid {
        render_grid(solution)
    } else {
        String::new()
    };
    let center_x = width as f64 / 2.0;
    let center_y = height as f64 / 2.0;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="Annotated Seiza plate solution">
<style>
  .marker {{ fill: none; stroke-width: 2.2; vector-effect: non-scaling-stroke; }}
  .label {{ fill: #f8fbff; stroke: #05090e; stroke-width: 4; paint-order: stroke; font: 600 15px ui-sans-serif, system-ui, sans-serif; }}
  .detail {{ fill: #c7d5e5; stroke: #05090e; stroke-width: 4; paint-order: stroke; font: 13px ui-monospace, monospace; }}
  .grid-line {{ fill: none; stroke: #7ddbe8; stroke-width: 1.2; stroke-dasharray: 7 5; opacity: .72; vector-effect: non-scaling-stroke; }}
  .grid-label {{ fill: #b9f3f7; stroke: #05090e; stroke-width: 4; paint-order: stroke; font: 600 13px ui-monospace, monospace; }}
</style>
<defs><clipPath id="image-frame"><rect width="{width}" height="{height}" /></clipPath></defs>
<image href="data:image/png;base64,{encoded}" width="{width}" height="{height}" preserveAspectRatio="none" />
<g clip-path="url(#image-frame)">{grid}</g>
<g>{objects}</g>
<g stroke="#f2c66d" fill="none" stroke-width="2" vector-effect="non-scaling-stroke">
  <circle cx="{center_x}" cy="{center_y}" r="18" />
  <path d="M {left} {center_y} H {right} M {center_x} {top} V {bottom}" />
</g>
<text class="detail" x="18" y="26">RA {ra:.8}°  Dec {dec:.8}°  Scale {scale:.5} arcsec/px</text>
<text class="detail" x="18" y="47">ICRS / TAN · {stars} matched stars · RMS {rms:.4} arcsec</text>
</svg>"##,
        left = center_x - 30.0,
        right = center_x + 30.0,
        top = center_y - 30.0,
        bottom = center_y + 30.0,
        ra = solution.center_ra_deg,
        dec = solution.center_dec_deg,
        scale = solution.pixel_scale_arcsec_per_pixel,
        stars = solution.matched_stars,
        rms = solution.rms_arcsec,
    )
}

fn render_grid(solution: &SolutionResponse) -> String {
    let width = solution.image_width as f64;
    let height = solution.image_height as f64;
    let wcs = Wcs {
        crval: (solution.wcs.crval[0], solution.wcs.crval[1]),
        crpix: (solution.wcs.crpix[0], solution.wcs.crpix[1]),
        cd: solution.wcs.cd,
    };
    let (ra_min, ra_max, dec_min, dec_max) = sky_bounds(&wcs, width, height);
    let center_dec_cos = solution.center_dec_deg.to_radians().cos().abs().max(0.05);
    let angular_span = (dec_max - dec_min)
        .max((ra_max - ra_min) * center_dec_cos)
        .max(solution.pixel_scale_arcsec_per_pixel / 3600.0);
    let dec_step = nice_grid_step(angular_span / 6.0);
    let ra_step = nice_grid_step(angular_span / center_dec_cos / 6.0);
    let mut output = String::new();

    let mut ra = (ra_min / ra_step).floor() * ra_step;
    while ra <= ra_max + ra_step && output.len() < 250_000 {
        let samples = sample_curve(96, dec_min - dec_step, dec_max + dec_step, |dec| {
            wcs.world_to_pixel(normalize_ra(ra), dec.clamp(-89.999_999, 89.999_999))
        });
        render_grid_curve(
            &mut output,
            &samples,
            width,
            height,
            &format_ra(normalize_ra(ra)),
            GridAxis::Ra,
        );
        ra += ra_step;
    }

    let mut dec = (dec_min / dec_step).floor() * dec_step;
    while dec <= dec_max + dec_step && dec <= 90.0 && output.len() < 500_000 {
        if dec >= -90.0 {
            let samples = sample_curve(96, ra_min - ra_step, ra_max + ra_step, |ra| {
                wcs.world_to_pixel(normalize_ra(ra), dec.clamp(-89.999_999, 89.999_999))
            });
            render_grid_curve(
                &mut output,
                &samples,
                width,
                height,
                &format_dec(dec),
                GridAxis::Dec,
            );
        }
        dec += dec_step;
    }
    output
}

fn sky_bounds(wcs: &Wcs, width: f64, height: f64) -> (f64, f64, f64, f64) {
    let center_ra = wcs.pixel_to_world(width / 2.0, height / 2.0).0;
    let mut ra_min = f64::INFINITY;
    let mut ra_max = f64::NEG_INFINITY;
    let mut dec_min = f64::INFINITY;
    let mut dec_max = f64::NEG_INFINITY;
    for x_index in 0..=8 {
        for y_index in 0..=8 {
            let x = width * x_index as f64 / 8.0;
            let y = height * y_index as f64 / 8.0;
            let (ra, dec) = wcs.pixel_to_world(x, y);
            let ra = unwrap_ra(ra, center_ra);
            ra_min = ra_min.min(ra);
            ra_max = ra_max.max(ra);
            dec_min = dec_min.min(dec);
            dec_max = dec_max.max(dec);
        }
    }
    (ra_min, ra_max, dec_min, dec_max)
}

fn sample_curve(
    samples: usize,
    start: f64,
    end: f64,
    project: impl Fn(f64) -> Option<(f64, f64)>,
) -> Vec<Option<(f64, f64)>> {
    (0..=samples)
        .map(|index| {
            let coordinate = start + (end - start) * index as f64 / samples as f64;
            project(coordinate).filter(|(x, y)| x.is_finite() && y.is_finite())
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum GridAxis {
    Ra,
    Dec,
}

fn render_grid_curve(
    output: &mut String,
    samples: &[Option<(f64, f64)>],
    width: f64,
    height: f64,
    label: &str,
    axis: GridAxis,
) {
    let mut path = String::new();
    let mut pen_down = false;
    let mut points_in_frame = Vec::new();
    for sample in samples {
        let Some((x, y)) = sample else {
            pen_down = false;
            continue;
        };
        if *x < -4.0 * width || *x > 5.0 * width || *y < -4.0 * height || *y > 5.0 * height {
            pen_down = false;
            continue;
        }
        let command = if pen_down { 'L' } else { 'M' };
        let _ = write!(path, "{command}{x:.2},{y:.2} ");
        pen_down = true;
        if *x >= 4.0 && *x <= width - 4.0 && *y >= 62.0 && *y <= height - 4.0 {
            points_in_frame.push((*x, *y));
        }
    }
    if path.matches(['M', 'L']).count() < 2 || points_in_frame.is_empty() {
        return;
    }
    let _ = write!(output, r#"<path class="grid-line" d="{path}" />"#);
    let &(x, y) = match axis {
        GridAxis::Ra => points_in_frame
            .iter()
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .expect("grid curve has an in-frame point"),
        GridAxis::Dec => points_in_frame
            .iter()
            .min_by(|left, right| left.0.total_cmp(&right.0))
            .expect("grid curve has an in-frame point"),
    };
    let max_x = (width - 6.0).max(6.0);
    let max_y = (height - 6.0).max(6.0);
    let label_top = 76.0_f64.min(max_y);
    let (x, y, anchor) = match axis {
        GridAxis::Ra => (
            x.clamp(58.0_f64.min(max_x), max_x),
            (y + 15.0).clamp(label_top, max_y),
            "middle",
        ),
        GridAxis::Dec => (
            (x + 6.0).clamp(6.0, max_x),
            (y - 5.0).clamp(label_top, max_y),
            "start",
        ),
    };
    let _ = write!(
        output,
        r#"<text class="grid-label" x="{x:.2}" y="{y:.2}" text-anchor="{anchor}">{label}</text>"#,
    );
}

fn nice_grid_step(target_degrees: f64) -> f64 {
    const STEPS: &[f64] = &[
        1.0 / 3600.0,
        2.0 / 3600.0,
        5.0 / 3600.0,
        10.0 / 3600.0,
        15.0 / 3600.0,
        30.0 / 3600.0,
        1.0 / 60.0,
        2.0 / 60.0,
        5.0 / 60.0,
        10.0 / 60.0,
        15.0 / 60.0,
        30.0 / 60.0,
        1.0,
        2.0,
        5.0,
        10.0,
        15.0,
        30.0,
        45.0,
        90.0,
    ];
    STEPS
        .iter()
        .copied()
        .find(|step| *step >= target_degrees)
        .unwrap_or(90.0)
}

fn unwrap_ra(ra: f64, center_ra: f64) -> f64 {
    center_ra + (ra - center_ra + 540.0).rem_euclid(360.0) - 180.0
}

fn normalize_ra(ra: f64) -> f64 {
    ra.rem_euclid(360.0)
}

fn format_ra(ra_degrees: f64) -> String {
    let total_tenths =
        ((normalize_ra(ra_degrees) / 15.0 * 36_000.0).round() as u64).rem_euclid(864_000);
    let hours = total_tenths / 36_000;
    let minutes = total_tenths % 36_000 / 600;
    let seconds = total_tenths % 600;
    format!(
        "RA {hours:02}h{minutes:02}m{:02}.{}s",
        seconds / 10,
        seconds % 10
    )
}

fn format_dec(dec_degrees: f64) -> String {
    let sign = if dec_degrees.is_sign_negative() {
        '−'
    } else {
        '+'
    };
    let total_tenths = (dec_degrees.abs() * 36_000.0).round() as u64;
    let degrees = total_tenths / 36_000;
    let minutes = total_tenths % 36_000 / 600;
    let seconds = total_tenths % 600;
    format!(
        "Dec {sign}{degrees:02}°{minutes:02}′{:02}.{}″",
        seconds / 10,
        seconds % 10
    )
}

fn render_object(output: &mut String, object: &OverlayObject, width: f64, height: f64) {
    let color = match object.kind.as_str() {
        "star" | "double-star" => "#ffe45e",
        "transient" => "#ff4fd8",
        "comet" => "#6df2a2",
        "asteroid" => "#ffad5c",
        _ => "#5ee7f2",
    };
    if object.kind == "field-star" {
        let _ = write!(
            output,
            r##"<circle class="marker" stroke="#eef7ff" opacity=".78" cx="{x:.2}" cy="{y:.2}" r="3.5" />"##,
            x = object.x,
            y = object.y,
        );
        return;
    }
    let label = if object.common_name.trim().is_empty() {
        object.name.as_str()
    } else {
        object.common_name.as_str()
    };
    let label = xml_escape(label);
    let designation = xml_escape(&object.name);
    let magnitude = object
        .mag
        .map(|mag| format!(" · mag {mag:.1}"))
        .unwrap_or_default();
    let encompasses_frame = encompasses_frame(object, width, height);
    if encompasses_frame {
        let _ = write!(
            output,
            r#"<text class="label" x="{x:.2}" y="{y:.2}" fill="{color}">{label} ({designation}){magnitude}</text>"#,
            x = object.x.clamp(16.0, width - 16.0),
            y = object.y.clamp(68.0, height - 16.0),
        );
        return;
    }

    match object.kind.as_str() {
        "star" | "double-star" => {
            let _ = write!(
                output,
                r#"<path class="marker" stroke="{color}" d="M {x1:.2} {y:.2} H {x2:.2} M {x3:.2} {y:.2} H {x4:.2}" />"#,
                x1 = object.x - 16.0,
                x2 = object.x - 4.0,
                x3 = object.x + 4.0,
                x4 = object.x + 16.0,
                y = object.y,
            );
        }
        "transient" | "comet" | "asteroid" => {
            let size = 10.0;
            let _ = write!(
                output,
                r#"<path class="marker" stroke="{color}" d="M {x:.2} {top:.2} L {right:.2} {y:.2} L {x:.2} {bottom:.2} L {left:.2} {y:.2} Z" />"#,
                x = object.x,
                y = object.y,
                top = object.y - size,
                right = object.x + size,
                bottom = object.y + size,
                left = object.x - size,
            );
            if matches!(object.kind.as_str(), "comet" | "asteroid")
                && let Some(angle) = object.direction_angle_deg
            {
                let radians = angle.to_radians();
                let _ = write!(
                    output,
                    r#"<path class="marker" stroke="{color}" d="M {x1:.2} {y1:.2} L {x2:.2} {y2:.2}" />"#,
                    x1 = object.x + radians.cos() * size * 1.3,
                    y1 = object.y + radians.sin() * size * 1.3,
                    x2 = object.x + radians.cos() * size * 2.4,
                    y2 = object.y + radians.sin() * size * 2.4,
                );
            }
        }
        _ => {
            let radius_x = object.semi_major_px.max(10.0).min(width * 2.0);
            let radius_y = object.semi_minor_px.max(10.0).min(height * 2.0);
            let _ = write!(
                output,
                r#"<ellipse class="marker" stroke="{color}" cx="{x:.2}" cy="{y:.2}" rx="{radius_x:.2}" ry="{radius_y:.2}" transform="rotate({angle:.2} {x:.2} {y:.2})" />"#,
                x = object.x,
                y = object.y,
                angle = object.angle_deg,
            );
        }
    }
    let _ = write!(
        output,
        r#"<text class="label" x="{x:.2}" y="{y:.2}">{label} ({designation}){magnitude}</text>"#,
        x = (object.x + 14.0).clamp(8.0, width - 8.0),
        y = (object.y - 14.0).clamp(18.0, height - 8.0),
    );
}

fn encompasses_frame(object: &OverlayObject, width: f64, height: f64) -> bool {
    if object.semi_major_px <= 0.0 {
        return false;
    }
    let radians = object.angle_deg.to_radians();
    let (sin, cos) = radians.sin_cos();
    [(0.0, 0.0), (width, 0.0), (width, height), (0.0, height)]
        .into_iter()
        .all(|(x, y)| {
            let dx = x - object.x;
            let dy = y - object.y;
            let u = (dx * cos + dy * sin) / object.semi_major_px;
            let v = (-dx * sin + dy * cos) / object.semi_minor_px.max(1.0);
            u * u + v * v <= 1.0
        })
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::WcsResponse;

    fn solution() -> SolutionResponse {
        SolutionResponse {
            center_ra_deg: 12.0,
            center_dec_deg: -4.0,
            pixel_scale_arcsec_per_pixel: 1.2,
            matched_stars: 42,
            rms_arcsec: 0.4,
            image_width: 100,
            image_height: 80,
            wcs: WcsResponse {
                crval: [12.0, -4.0],
                crpix: [50.0, 40.0],
                cd: [[0.1, 0.0], [0.0, 0.1]],
                ctype: ["RA---TAN".into(), "DEC--TAN".into()],
                cunit: ["deg".into(), "deg".into()],
                radesys: "ICRS".into(),
                equinox: 2000.0,
            },
            footprint: [[0.0; 2]; 4],
            objects: vec![OverlayObject {
                name: "A&B".into(),
                common_name: "<target>".into(),
                kind: "galaxy".into(),
                mag: Some(8.5),
                x: 50.0,
                y: 40.0,
                semi_major_px: 10.0,
                semi_minor_px: 5.0,
                angle_deg: 20.0,
                source: Some("deep_sky".into()),
                ra_deg: Some(12.0),
                dec_deg: Some(-4.0),
                discovered: None,
                near_capture: None,
                distance_au: None,
                direction_pa_deg: None,
                direction_angle_deg: None,
            }],
            catalog_version: None,
            capture_time: None,
        }
    }

    #[test]
    fn overlay_embeds_preview_and_escapes_labels() {
        let svg = render_svg(
            &solution(),
            &Bytes::from_static(b"png"),
            OverlayOptions::default(),
        );
        assert!(svg.contains("data:image/png;base64,cG5n"));
        assert!(svg.contains("&lt;target&gt;"));
        assert!(svg.contains("A&amp;B"));
    }

    #[test]
    fn grid_option_projects_coordinate_graticule() {
        let svg = render_svg(
            &solution(),
            &Bytes::from_static(b"png"),
            OverlayOptions {
                objects: false,
                grid: true,
            },
        );
        assert!(svg.contains("class=\"grid-line\""));
        assert!(svg.contains("RA "));
        assert!(svg.contains("Dec "));
        assert!(!svg.contains("&lt;target&gt;"));
    }

    #[test]
    fn grid_handles_right_ascension_wraparound() {
        let mut wrapped = solution();
        wrapped.center_ra_deg = 359.95;
        wrapped.wcs.crval[0] = 359.95;
        let svg = render_svg(
            &wrapped,
            &Bytes::from_static(b"png"),
            OverlayOptions {
                objects: false,
                grid: true,
            },
        );
        assert!(svg.contains("class=\"grid-line\""));
        assert!(svg.contains("RA 00h"));
    }
}
