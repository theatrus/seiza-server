use crate::models::{OverlayObject, SolutionResponse};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use std::fmt::Write;

/// Render a self-contained solution overlay. The marker vocabulary is adapted
/// from Tenrankai's Apache-2.0 `AstroOverlay` component so both Seiza-based
/// applications present catalog objects consistently.
pub fn render_svg(solution: &SolutionResponse, preview_png: &Bytes) -> String {
    let width = solution.image_width;
    let height = solution.image_height;
    let encoded = STANDARD.encode(preview_png);
    let mut objects = String::new();
    for object in &solution.objects {
        render_object(&mut objects, object, width as f64, height as f64);
    }
    let center_x = width as f64 / 2.0;
    let center_y = height as f64 / 2.0;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="Annotated Seiza plate solution">
<style>
  .marker {{ fill: none; stroke-width: 2.2; vector-effect: non-scaling-stroke; }}
  .label {{ fill: #f8fbff; stroke: #05090e; stroke-width: 4; paint-order: stroke; font: 600 15px ui-sans-serif, system-ui, sans-serif; }}
  .detail {{ fill: #c7d5e5; stroke: #05090e; stroke-width: 4; paint-order: stroke; font: 13px ui-monospace, monospace; }}
</style>
<image href="data:image/png;base64,{encoded}" width="{width}" height="{height}" preserveAspectRatio="none" />
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

fn render_object(output: &mut String, object: &OverlayObject, width: f64, height: f64) {
    let color = match object.kind.as_str() {
        "star" | "double-star" => "#ffe45e",
        "transient" => "#ff4fd8",
        "comet" => "#6df2a2",
        "asteroid" => "#ffad5c",
        _ => "#5ee7f2",
    };
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
    let encompasses_frame = object.semi_major_px > width.hypot(height);
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
            }],
        }
    }

    #[test]
    fn overlay_embeds_preview_and_escapes_labels() {
        let svg = render_svg(&solution(), &Bytes::from_static(b"png"));
        assert!(svg.contains("data:image/png;base64,cG5n"));
        assert!(svg.contains("&lt;target&gt;"));
        assert!(svg.contains("A&amp;B"));
    }
}
