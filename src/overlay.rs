use crate::models::{OverlayObject, SolutionResponse};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use seiza::wcs::Wcs;
use std::fmt::Write;

#[derive(Debug, Clone, Copy)]
pub struct OverlayOptions {
    pub objects: bool,
    pub grid: bool,
    pub outlines: bool,
}

impl Default for OverlayOptions {
    fn default() -> Self {
        Self {
            objects: true,
            grid: false,
            outlines: true,
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
            render_object(
                &mut objects,
                object,
                width as f64,
                height as f64,
                options.outlines,
                solution.pixel_scale_arcsec_per_pixel,
            );
        }
    }
    let GridMarkup {
        lines: grid_lines,
        labels: grid_labels,
    } = if options.grid {
        render_grid(solution)
    } else {
        GridMarkup::default()
    };
    let grid_font_size = grid_label_font_size(width as f64);
    let grid_label_stroke = grid_font_size * 0.12;
    let center_x = width as f64 / 2.0;
    let center_y = height as f64 / 2.0;
    let projection = if solution.wcs.sip.is_some() {
        "TAN-SIP"
    } else {
        "TAN"
    };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="Annotated Seiza plate solution">
<style>
  .marker {{ fill: none; stroke-width: 2.2; vector-effect: non-scaling-stroke; }}
  .satellite-predicted {{ stroke-dasharray: 8 5; opacity: .88; }}
  .satellite-pixel-aligned {{ stroke-width: 2.8; }}
  .label {{ fill: #f8fbff; stroke: #05090e; stroke-width: 4; paint-order: stroke; font: 600 15px ui-sans-serif, system-ui, sans-serif; }}
  .detail {{ fill: #c7d5e5; stroke: #05090e; stroke-width: 4; paint-order: stroke; font: 13px ui-monospace, monospace; }}
  .grid-line {{ fill: none; stroke: #7ddbe8; stroke-width: 1.2; stroke-dasharray: 7 5; opacity: .72; vector-effect: non-scaling-stroke; }}
  .grid-label {{ fill: #b9f3f7; stroke: #05090e; stroke-width: {grid_label_stroke:.2}; paint-order: stroke; font: 700 {grid_font_size:.2}px ui-monospace, monospace; }}
  .direction-tail {{ stroke-linecap: round; stroke-linejoin: round; }}
</style>
<defs><clipPath id="image-frame"><rect width="{width}" height="{height}" /></clipPath></defs>
<image href="data:image/png;base64,{encoded}" width="{width}" height="{height}" preserveAspectRatio="none" />
<g clip-path="url(#image-frame)" class="grid-lines">{grid_lines}</g>
<g class="grid-labels">{grid_labels}</g>
<g>{objects}</g>
<g stroke="#f2c66d" fill="none" stroke-width="2" vector-effect="non-scaling-stroke">
  <circle cx="{center_x}" cy="{center_y}" r="18" />
  <path d="M {left} {center_y} H {right} M {center_x} {top} V {bottom}" />
</g>
<text class="detail" x="18" y="26">RA {ra:.8}°  Dec {dec:.8}°  Scale {scale:.5} arcsec/px</text>
<text class="detail" x="18" y="47">ICRS / {projection} · {stars} matched stars · RMS {rms:.4} arcsec</text>
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
        projection = projection,
    )
}

#[derive(Debug, Default)]
struct GridMarkup {
    lines: String,
    labels: String,
}

fn render_grid(solution: &SolutionResponse) -> GridMarkup {
    let width = solution.image_width as f64;
    let height = solution.image_height as f64;
    let wcs = solution.wcs.to_seiza();
    let (ra_min, ra_max, dec_min, dec_max) = sky_bounds(&wcs, width, height);
    let center_dec_cos = solution.center_dec_deg.to_radians().cos().abs().max(0.05);
    let angular_span = (dec_max - dec_min)
        .max((ra_max - ra_min) * center_dec_cos)
        .max(solution.pixel_scale_arcsec_per_pixel / 3600.0);
    let dec_step = nice_grid_step(angular_span / 5.0);
    let ra_step = nice_grid_step(angular_span / center_dec_cos / 5.0);
    let font_size = grid_label_font_size(width);
    let geometry = GridGeometry {
        width,
        height,
        font_size,
    };
    let mut output = GridMarkup::default();

    let mut ra = (ra_min / ra_step).floor() * ra_step;
    while ra <= ra_max + ra_step && output.lines.len() < 250_000 {
        let samples = sample_curve(96, dec_min - dec_step, dec_max + dec_step, |dec| {
            wcs.world_to_pixel(normalize_ra(ra), dec.clamp(-89.999_999, 89.999_999))
        });
        render_grid_curve(
            &mut output.lines,
            &mut output.labels,
            &samples,
            &format_ra(normalize_ra(ra)),
            GridAxis::Ra,
            geometry,
        );
        ra += ra_step;
    }

    let mut dec = (dec_min / dec_step).floor() * dec_step;
    while dec <= dec_max + dec_step && dec <= 90.0 && output.lines.len() < 500_000 {
        if dec >= -90.0 {
            let samples = sample_curve(96, ra_min - ra_step, ra_max + ra_step, |ra| {
                wcs.world_to_pixel(normalize_ra(ra), dec.clamp(-89.999_999, 89.999_999))
            });
            render_grid_curve(
                &mut output.lines,
                &mut output.labels,
                &samples,
                &format_dec(dec),
                GridAxis::Dec,
                geometry,
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

#[derive(Debug, Clone, Copy)]
struct GridGeometry {
    width: f64,
    height: f64,
    font_size: f64,
}

fn render_grid_curve(
    lines: &mut String,
    labels: &mut String,
    samples: &[Option<(f64, f64)>],
    label: &str,
    axis: GridAxis,
    geometry: GridGeometry,
) {
    let GridGeometry {
        width,
        height,
        font_size,
    } = geometry;
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
        if *x >= 4.0 && *x <= width - 4.0 && *y >= 4.0 && *y <= height - 4.0 {
            points_in_frame.push((*x, *y));
        }
    }
    if path.matches(['M', 'L']).count() < 2 || points_in_frame.is_empty() {
        return;
    }
    let _ = write!(lines, r#"<path class="grid-line" d="{path}" />"#);
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
    let padding = (font_size * 0.45).max(6.0);
    let label_width = label.chars().count() as f64 * font_size * 0.7;
    let minimum_baseline = padding + font_size * 1.08;
    let maximum_baseline = height - padding - font_size * 0.25;
    let y = clamp_or_center(
        match axis {
            GridAxis::Ra => y + font_size * 1.35,
            GridAxis::Dec => y - padding,
        },
        minimum_baseline,
        maximum_baseline,
        height / 2.0,
    );
    let (x, anchor) = match axis {
        GridAxis::Ra => (
            clamp_or_center(
                x,
                padding + label_width / 2.0,
                width - padding - label_width / 2.0,
                width / 2.0,
            ),
            "middle",
        ),
        GridAxis::Dec => (
            clamp_or_center(
                x + padding,
                padding,
                width - padding - label_width,
                width / 2.0,
            ),
            if label_width + padding * 2.0 <= width {
                "start"
            } else {
                "middle"
            },
        ),
    };
    let _ = write!(
        labels,
        r#"<text class="grid-label" x="{x:.2}" y="{y:.2}" text-anchor="{anchor}">{label}</text>"#,
    );
}

fn clamp_or_center(value: f64, minimum: f64, maximum: f64, center: f64) -> f64 {
    if minimum <= maximum {
        value.clamp(minimum, maximum)
    } else {
        center
    }
}

fn grid_label_font_size(width: f64) -> f64 {
    (width / 60.0).max(18.0).min(width / 18.0).max(6.0)
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

fn render_object(
    output: &mut String,
    object: &OverlayObject,
    width: f64,
    height: f64,
    show_outlines: bool,
    pixel_scale_arcsec_per_pixel: f64,
) {
    let color = match object.kind.as_str() {
        "star" | "double-star" => "#ffe45e",
        "identified-star" => "#b7a6ff",
        "transient" => "#ff4fd8",
        "comet" => "#6df2a2",
        "asteroid" => "#ffad5c",
        "satellite" => satellite_color(object),
        _ => deep_sky_catalog_color(&object.name),
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
    let display_label = if label == designation {
        format!("{designation}{magnitude}")
    } else {
        format!("{label} ({designation}){magnitude}")
    };
    let encompasses_frame = encompasses_frame(object, width, height);
    if encompasses_frame {
        let _ = write!(
            output,
            r#"<text class="label" x="{x:.2}" y="{y:.2}" fill="{color}">{display_label}</text>"#,
            x = object.x.clamp(16.0, width - 16.0),
            y = object.y.clamp(68.0, height - 16.0),
        );
        return;
    }

    match object.kind.as_str() {
        "star" | "double-star" | "identified-star" => {
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
            let size = (width / 75.0).max(14.0);
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
                let vector_length = moving_body_vector_length(
                    size,
                    object.motion_arcsec_per_hour,
                    pixel_scale_arcsec_per_pixel,
                );
                render_direction_tail(output, object, size, angle, vector_length, color);
            }
        }
        "satellite" => render_outlines(output, object, color),
        _ => {
            if object.outlines.is_empty() || !show_outlines {
                let radius_x = object.semi_major_px.max(10.0).min(width * 2.0);
                let radius_y = if object.angle_deg.is_none() {
                    radius_x
                } else {
                    object.semi_minor_px.max(10.0).min(height * 2.0)
                };
                let _ = write!(
                    output,
                    r#"<ellipse class="marker" stroke="{color}" cx="{x:.2}" cy="{y:.2}" rx="{radius_x:.2}" ry="{radius_y:.2}" transform="rotate({angle:.2} {x:.2} {y:.2})" />"#,
                    x = object.x,
                    y = object.y,
                    angle = object.angle_deg.unwrap_or(0.0),
                );
            } else {
                render_outlines(output, object, color);
            }
        }
    }
    let _ = write!(
        output,
        r#"<text class="label" x="{x:.2}" y="{y:.2}">{display_label}</text>"#,
        x = (object.x + 14.0).clamp(8.0, width - 8.0),
        y = (object.y - 14.0).clamp(18.0, height - 8.0),
    );
}

fn deep_sky_catalog_color(name: &str) -> &'static str {
    let name = name.trim().to_ascii_uppercase();
    if catalog_prefix(&name, "PGC") {
        "#a1aed8"
    } else if catalog_prefix(&name, "UGC") {
        "#79aff5"
    } else if catalog_prefix(&name, "LBN") {
        "#a2d96f"
    } else if catalog_prefix(&name, "CED") || catalog_prefix(&name, "CEDERBLAD") {
        "#70d7d0"
    } else if catalog_prefix(&name, "LDN") || numbered_designation(&name, "B") {
        "#b4a3f0"
    } else if catalog_prefix(&name, "SNR") {
        "#f18782"
    } else if sharpless_designation(&name) || catalog_prefix(&name, "VDB") {
        "#ee9a78"
    } else if numbered_designation(&name, "M") {
        "#f2ca72"
    } else if numbered_designation(&name, "NGC") {
        "#55cfff"
    } else if numbered_designation(&name, "IC") {
        "#72dfb9"
    } else {
        "#c1d1d3"
    }
}

fn satellite_color(object: &OverlayObject) -> &'static str {
    match object
        .outlines
        .iter()
        .find(|outline| outline.role == "predicted-track")
        .and_then(|outline| outline.level.as_deref())
    {
        Some("high") => "#ff4d5a",
        Some("possible") => "#ffd166",
        _ => "#43d9e6",
    }
}

fn catalog_prefix(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|rest| {
        rest.is_empty()
            || rest
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_whitespace())
    })
}

fn numbered_designation(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix).is_some_and(|rest| {
        rest.trim_start()
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
    })
}

fn sharpless_designation(name: &str) -> bool {
    name.strip_prefix("SH").is_some_and(|rest| {
        rest.trim_start().strip_prefix('2').is_some_and(|rest| {
            rest.chars()
                .next()
                .is_some_and(|character| character == '-' || character.is_ascii_whitespace())
        })
    })
}

fn render_outlines(output: &mut String, object: &OverlayObject, color: &str) {
    for outline in &object.outlines {
        let (class_name, outline_color) = match (object.kind.as_str(), outline.role.as_str()) {
            ("satellite", "predicted-track") => {
                ("marker object-outline satellite-predicted", color)
            }
            ("satellite", "pixel-aligned-track") => {
                ("marker object-outline satellite-pixel-aligned", "#7cff6b")
            }
            _ => ("marker object-outline", color),
        };
        for contour in &outline.contours {
            let Some(([first_x, first_y], rest)) = contour.points.split_first() else {
                continue;
            };
            let mut path = format!("M {first_x:.2} {first_y:.2}");
            for [x, y] in rest {
                let _ = write!(path, " L {x:.2} {y:.2}");
            }
            if contour.closed {
                path.push_str(" Z");
            }
            let _ = write!(
                output,
                r#"<path class="{class_name}" data-geometry-id="{geometry_id}" data-outline-level="{level}" data-outline-role="{role}" data-outline-quality="{quality}" stroke="{outline_color}" d="{path}" />"#,
                geometry_id = xml_escape(&outline.geometry_id),
                level = xml_escape(outline.level.as_deref().unwrap_or("")),
                role = xml_escape(&outline.role),
                quality = xml_escape(&outline.quality),
            );
        }
    }
}

fn render_direction_tail(
    output: &mut String,
    object: &OverlayObject,
    size: f64,
    angle_degrees: f64,
    vector_length_pixels: Option<f64>,
    color: &str,
) {
    let radians = angle_degrees.to_radians();
    let along = |distance: f64| {
        (
            object.x + radians.cos() * size * distance,
            object.y + radians.sin() * size * distance,
        )
    };
    let offset = |point: (f64, f64), distance: f64| {
        (
            point.0 - radians.sin() * size * distance,
            point.1 + radians.cos() * size * distance,
        )
    };
    let default_tip_distance = if object.kind == "comet" { 4.0 } else { 4.5 };
    let tip_distance = vector_length_pixels
        .filter(|length| length.is_finite())
        .map(|length| (length.abs() / size.abs().max(f64::EPSILON)).max(1.5))
        .unwrap_or(default_tip_distance);
    let (path, class_name) = if object.kind == "comet" {
        let root = along(1.15);
        let span = tip_distance - 1.15;
        let shoulder = along(1.15 + span * 0.75);
        let flare = (span * 0.18).clamp(0.35, 0.85);
        let tip = along(tip_distance);
        let upper = offset(shoulder, flare);
        let lower = offset(shoulder, -flare);
        (
            format!(
                "M {:.2} {:.2} L {:.2} {:.2} M {:.2} {:.2} L {:.2} {:.2} M {:.2} {:.2} L {:.2} {:.2}",
                root.0,
                root.1,
                tip.0,
                tip.1,
                root.0,
                root.1,
                upper.0,
                upper.1,
                root.0,
                root.1,
                lower.0,
                lower.1,
            ),
            "comet-tail",
        )
    } else {
        let root = along(1.2);
        let span = tip_distance - 1.2;
        let tip = along(tip_distance);
        let arrow_root = along(1.2 + span * 0.73);
        let arrow_width = (span * 0.2).clamp(0.45, 0.9);
        let upper = offset(arrow_root, arrow_width);
        let lower = offset(arrow_root, -arrow_width);
        (
            format!(
                "M {:.2} {:.2} L {:.2} {:.2} M {:.2} {:.2} L {:.2} {:.2} L {:.2} {:.2}",
                root.0, root.1, tip.0, tip.1, upper.0, upper.1, tip.0, tip.1, lower.0, lower.1,
            ),
            "asteroid-tail",
        )
    };
    let motion_attributes = object
        .motion_arcsec_per_hour
        .map_or_else(String::new, |speed| {
            let length = vector_length_pixels
                .map(|length| format!(r#" data-motion-vector-length="{length:.2}""#))
                .unwrap_or_default();
            format!(r#" data-motion-arcsec-per-hour="{speed:.4}"{length}"#)
        });
    let _ = write!(
        output,
        r#"<path class="marker direction-tail {class_name}" data-direction-angle="{angle_degrees:.2}"{motion_attributes} stroke="{color}" d="{path}" />"#,
    );
}

fn moving_body_vector_length(
    marker_size: f64,
    motion_arcsec_per_hour: Option<f64>,
    pixel_scale_arcsec_per_pixel: f64,
) -> Option<f64> {
    let motion = motion_arcsec_per_hour?;
    if !motion.is_finite() || motion < 0.0 || pixel_scale_arcsec_per_pixel <= 0.0 {
        return None;
    }
    let size = marker_size.abs().max(f64::EPSILON);
    let three_hour_track = motion * 3.0 / pixel_scale_arcsec_per_pixel;
    Some(three_hour_track.clamp(size * 3.0, size * 9.0))
}

fn encompasses_frame(object: &OverlayObject, width: f64, height: f64) -> bool {
    if object.semi_major_px <= 0.0 {
        return false;
    }
    let radians = object.angle_deg.unwrap_or(0.0).to_radians();
    let (sin, cos) = radians.sin_cos();
    let semi_minor_px = if object.angle_deg.is_none() {
        object.semi_major_px
    } else {
        object.semi_minor_px.max(1.0)
    };
    [(0.0, 0.0), (width, 0.0), (width, height), (0.0, height)]
        .into_iter()
        .all(|(x, y)| {
            let dx = x - object.x;
            let dy = y - object.y;
            let u = (dx * cos + dy * sin) / object.semi_major_px;
            let v = (-dx * sin + dy * cos) / semi_minor_px;
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
    use crate::models::{OverlayContour, OverlayOutline, WcsResponse};

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
                sip: None,
            },
            footprint: [[0.0; 2]; 4],
            objects: vec![OverlayObject {
                stable_id: Some("test:A&B".into()),
                name: "A&B".into(),
                common_name: "<target>".into(),
                kind: "galaxy".into(),
                mag: Some(8.5),
                x: 50.0,
                y: 40.0,
                semi_major_px: 10.0,
                semi_minor_px: 5.0,
                angle_deg: Some(20.0),
                source: Some("deep_sky".into()),
                catalog_source: Some("test".into()),
                aliases: Vec::new(),
                parent_ids: Vec::new(),
                alternate_ids: Vec::new(),
                alternate_sources: Vec::new(),
                ra_deg: Some(12.0),
                dec_deg: Some(-4.0),
                discovered: None,
                near_capture: None,
                distance_au: None,
                motion_arcsec_per_hour: None,
                direction_pa_deg: None,
                direction_angle_deg: None,
                outlines: Vec::new(),
            }],
            catalog_version: None,
            capture_time: None,
            statistics: None,
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
    fn unknown_orientation_renders_a_conservative_circle() {
        let mut solution = solution();
        solution.objects[0].angle_deg = None;
        let svg = render_svg(
            &solution,
            &Bytes::from_static(b"png"),
            OverlayOptions::default(),
        );
        assert!(svg.contains("rx=\"10.00\" ry=\"10.00\""));
        assert!(svg.contains("rotate(0.00 50.00 40.00)"));
    }

    #[test]
    fn projected_catalog_outlines_replace_the_fallback_ellipse() {
        let mut solution = solution();
        solution.objects[0].outlines = vec![OverlayOutline {
            geometry_id: "openngc:NGC1#outline-1".into(),
            source_record_id: "openngc:NGC1".into(),
            role: "brightness-level".into(),
            quality: "catalog".into(),
            level: Some("1".into()),
            contours: vec![OverlayContour {
                closed: true,
                points: vec![[10.0, 20.0], [30.0, 40.0], [50.0, 20.0]],
            }],
        }];
        let svg = render_svg(
            &solution,
            &Bytes::from_static(b"png"),
            OverlayOptions::default(),
        );
        assert!(svg.contains("class=\"marker object-outline\""));
        assert!(svg.contains("data-outline-level=\"1\""));
        assert!(svg.contains("M 10.00 20.00 L 30.00 40.00 L 50.00 20.00 Z"));
        assert!(!svg.contains("<ellipse class=\"marker\""));
    }

    #[test]
    fn satellite_tracks_render_as_open_risk_paths_without_duplicate_labels() {
        let mut tracked = solution();
        tracked.objects = vec![OverlayObject {
            stable_id: Some("satellite:norad:25544".into()),
            name: "ISS (ZARYA)".into(),
            common_name: String::new(),
            kind: "satellite".into(),
            mag: None,
            x: 30.0,
            y: 30.0,
            semi_major_px: 0.0,
            semi_minor_px: 0.0,
            angle_deg: None,
            source: Some("satellite_prediction".into()),
            catalog_source: Some("CelesTrak active".into()),
            aliases: Vec::new(),
            parent_ids: Vec::new(),
            alternate_ids: vec!["NORAD 25544".into()],
            alternate_sources: Vec::new(),
            ra_deg: None,
            dec_deg: None,
            discovered: None,
            near_capture: None,
            distance_au: None,
            motion_arcsec_per_hour: None,
            direction_pa_deg: None,
            direction_angle_deg: None,
            outlines: vec![
                OverlayOutline {
                    geometry_id: "satellite:norad:25544:predicted-track".into(),
                    source_record_id: "satellite:norad:25544".into(),
                    role: "predicted-track".into(),
                    quality: "propagated".into(),
                    level: Some("low".into()),
                    contours: vec![OverlayContour {
                        closed: false,
                        points: vec![[10.0, 20.0], [30.0, 30.0], [50.0, 40.0]],
                    }],
                },
                OverlayOutline {
                    geometry_id: "satellite:norad:25544:pixel-aligned-track".into(),
                    source_record_id: "satellite:norad:25544".into(),
                    role: "pixel-aligned-track".into(),
                    quality: "detected".into(),
                    level: Some("detected".into()),
                    contours: vec![OverlayContour {
                        closed: false,
                        points: vec![[12.0, 22.0], [32.0, 32.0], [52.0, 42.0]],
                    }],
                },
            ],
        }];

        let svg = render_svg(
            &tracked,
            &Bytes::from_static(b"png"),
            OverlayOptions::default(),
        );
        assert!(svg.contains("stroke=\"#43d9e6\""));
        assert!(svg.contains("stroke=\"#7cff6b\""));
        assert!(svg.contains("data-outline-role=\"predicted-track\""));
        assert!(svg.contains("data-outline-role=\"pixel-aligned-track\""));
        assert!(svg.contains("M 10.00 20.00 L 30.00 30.00 L 50.00 40.00\""));
        assert!(svg.contains(">ISS (ZARYA)</text>"));
        assert!(!svg.contains("ISS (ZARYA) (ISS (ZARYA))"));
        assert!(!svg.contains("<ellipse class=\"marker\""));
    }

    #[test]
    fn deep_sky_catalogs_use_a_restrained_marker_palette() {
        assert_eq!(deep_sky_catalog_color("M 31"), "#f2ca72");
        assert_eq!(deep_sky_catalog_color("NGC7000"), "#55cfff");
        assert_eq!(deep_sky_catalog_color("IC 5070"), "#72dfb9");
        assert_eq!(deep_sky_catalog_color("Sh 2-101"), "#ee9a78");
        assert_eq!(deep_sky_catalog_color("LBN 437"), "#a2d96f");
        assert_eq!(deep_sky_catalog_color("LDN 935"), "#b4a3f0");
        assert_eq!(deep_sky_catalog_color("SNR G120.1+1.4"), "#f18782");
        assert_eq!(deep_sky_catalog_color("UGC 123"), "#79aff5");
        assert_eq!(deep_sky_catalog_color("PGC 123"), "#a1aed8");
        assert_eq!(deep_sky_catalog_color("Abell 426"), "#c1d1d3");
    }

    #[test]
    fn direct_svg_uses_catalog_color_for_ellipses_and_outlines() {
        let mut colored = solution();
        colored.objects[0].name = "LDN 935".into();
        let ellipse_svg = render_svg(
            &colored,
            &Bytes::from_static(b"png"),
            OverlayOptions::default(),
        );
        assert!(ellipse_svg.contains("<ellipse class=\"marker\" stroke=\"#b4a3f0\""));

        colored.objects[0].name = "Sh2-101".into();
        colored.objects[0].outlines = vec![OverlayOutline {
            geometry_id: "openngc:Sh2-101#outline-1".into(),
            source_record_id: "openngc:Sh2-101".into(),
            role: "brightness-level".into(),
            quality: "catalog".into(),
            level: Some("1".into()),
            contours: vec![OverlayContour {
                closed: true,
                points: vec![[10.0, 20.0], [30.0, 40.0], [50.0, 20.0]],
            }],
        }];
        let outline_svg = render_svg(
            &colored,
            &Bytes::from_static(b"png"),
            OverlayOptions::default(),
        );
        assert!(outline_svg.contains(
            "class=\"marker object-outline\" data-geometry-id=\"openngc:Sh2-101#outline-1\" data-outline-level=\"1\" stroke=\"#ee9a78\""
        ));

        let fallback_svg = render_svg(
            &colored,
            &Bytes::from_static(b"png"),
            OverlayOptions {
                outlines: false,
                ..OverlayOptions::default()
            },
        );
        assert!(!fallback_svg.contains("class=\"marker object-outline\""));
        assert!(fallback_svg.contains("<ellipse class=\"marker\" stroke=\"#ee9a78\""));
    }

    #[test]
    fn grid_option_projects_coordinate_graticule() {
        let svg = render_svg(
            &solution(),
            &Bytes::from_static(b"png"),
            OverlayOptions {
                objects: false,
                grid: true,
                ..OverlayOptions::default()
            },
        );
        assert!(svg.contains("class=\"grid-line\""));
        assert!(svg.contains("RA "));
        assert!(svg.contains("Dec "));
        assert!(!svg.contains("&lt;target&gt;"));
        assert!(svg.contains("class=\"grid-lines\""));
        assert!(svg.contains("class=\"grid-labels\""));
        assert!(
            svg.find("class=\"grid-labels\"").unwrap() > svg.find("class=\"grid-lines\"").unwrap()
        );
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
                ..OverlayOptions::default()
            },
        );
        assert!(svg.contains("class=\"grid-line\""));
        assert!(svg.contains("RA 00h"));
    }

    #[test]
    fn large_overlays_scale_coordinate_labels_for_native_rendering() {
        let mut large = solution();
        large.image_width = 4096;
        large.image_height = 2400;
        large.wcs.crpix = [2048.0, 1200.0];
        let svg = render_svg(
            &large,
            &Bytes::from_static(b"png"),
            OverlayOptions {
                objects: false,
                grid: true,
                ..OverlayOptions::default()
            },
        );
        assert!(svg.contains("font: 700 68.27px ui-monospace"));
        assert!(svg.contains("stroke-width: 8.19"));
    }

    #[test]
    fn moving_bodies_render_catalog_direction_tails() {
        let mut moving = solution();
        let mut comet = moving.objects[0].clone();
        comet.name = "C/2026 A1".into();
        comet.common_name = "Test comet".into();
        comet.kind = "comet".into();
        comet.direction_angle_deg = Some(18.0);
        comet.motion_arcsec_per_hour = Some(24.0);
        let mut asteroid = comet.clone();
        asteroid.name = "(12345)".into();
        asteroid.common_name = "Test asteroid".into();
        asteroid.kind = "asteroid".into();
        asteroid.direction_angle_deg = Some(136.0);
        asteroid.motion_arcsec_per_hour = Some(2.0);
        moving.objects = vec![comet, asteroid];

        let svg = render_svg(
            &moving,
            &Bytes::from_static(b"png"),
            OverlayOptions::default(),
        );
        assert!(svg.contains("direction-tail comet-tail"));
        assert!(svg.contains("data-direction-angle=\"18.00\""));
        assert!(svg.contains("data-motion-arcsec-per-hour=\"24.0000\""));
        assert!(svg.contains("data-motion-vector-length=\"60.00\""));
        assert!(svg.contains("direction-tail asteroid-tail"));
        assert!(svg.contains("data-direction-angle=\"136.00\""));
        assert!(svg.contains("data-motion-arcsec-per-hour=\"2.0000\""));
        assert!(svg.contains("data-motion-vector-length=\"42.00\""));
    }
}
