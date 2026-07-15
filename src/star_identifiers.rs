use seiza::{
    catalog::angular_separation_deg,
    star_ids::{StarIdentifierCatalog, StarLookupMatch, StarNameCatalog, StarNameKind},
    wcs::Wcs,
};
use serde::Serialize;
use std::{collections::HashMap, io, path::Path};

const RA_BINS: usize = 72;
const DEC_BINS: usize = 36;

#[derive(Debug, Clone)]
pub struct StarLabel {
    pub designation: String,
    pub catalog: StarNameCatalog,
    pub kind: StarNameKind,
    pub detail: String,
    pub ra: f64,
    pub dec: f64,
    pub mag: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StarIdentifierMatch {
    pub designation: String,
    pub stable_id: String,
    pub catalog: String,
    pub kind: String,
    pub detail: String,
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub mag: Option<f32>,
}

/// Searchable stellar identifiers plus a compact spatial index over the
/// human-facing designation records in a Seiza `SEIZASI1` sidecar.
pub struct StarIdentifierLayer {
    catalog: StarIdentifierCatalog,
    tiles: Vec<Vec<StarLabel>>,
    label_count: usize,
}

impl StarIdentifierLayer {
    pub fn open(path: &Path) -> io::Result<Self> {
        let catalog = StarIdentifierCatalog::open(path)?;
        let mut preferred = HashMap::<String, StarLabel>::new();

        // Names are sorted by a normalized key. Walking the alphanumeric
        // first-character buckets uses the public indexed lookup API and
        // touches only the textual section needed for image labels.
        for prefix in ('0'..='9').chain('A'..='Z') {
            for star in catalog.search_names(&prefix.to_string(), usize::MAX)? {
                let candidate = StarLabel {
                    designation: star.designation.to_owned(),
                    catalog: star.catalog,
                    kind: star.kind,
                    detail: star.detail.to_owned(),
                    ra: star.ra,
                    dec: star.dec,
                    mag: star.mag,
                };
                match preferred.entry(star.stable_id.to_owned()) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(candidate);
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if label_rank(&candidate) < label_rank(entry.get()) {
                            entry.insert(candidate);
                        }
                    }
                }
            }
        }

        let label_count = preferred.len();
        let mut tiles = vec![Vec::new(); RA_BINS * DEC_BINS];
        for label in preferred.into_values() {
            tiles[tile_index(label.ra, label.dec)].push(label);
        }
        for tile in &mut tiles {
            tile.sort_by(|left, right| {
                left.mag
                    .unwrap_or(f32::INFINITY)
                    .total_cmp(&right.mag.unwrap_or(f32::INFINITY))
                    .then_with(|| left.designation.cmp(&right.designation))
            });
        }

        Ok(Self {
            catalog,
            tiles,
            label_count,
        })
    }

    pub fn len(&self) -> usize {
        self.catalog.len()
    }

    pub fn is_empty(&self) -> bool {
        self.catalog.is_empty()
    }

    pub fn label_count(&self) -> usize {
        self.label_count
    }

    pub fn attribution(&self) -> &str {
        self.catalog.attribution()
    }

    pub fn epoch(&self) -> f64 {
        self.catalog.epoch()
    }

    pub fn search(
        &self,
        query: &str,
        prefix: bool,
        limit: usize,
    ) -> io::Result<Vec<StarIdentifierMatch>> {
        let matches = if prefix {
            self.catalog
                .search_names(query, limit)?
                .into_iter()
                .map(|star| StarIdentifierMatch {
                    designation: star.designation.to_owned(),
                    stable_id: star.stable_id.to_owned(),
                    catalog: star.catalog.as_str().to_owned(),
                    kind: star.kind.as_str().to_owned(),
                    detail: star.detail.to_owned(),
                    ra_deg: star.ra,
                    dec_deg: star.dec,
                    mag: star.mag,
                })
                .collect()
        } else {
            self.catalog
                .lookup_query(query)?
                .into_iter()
                .take(limit)
                .map(|star| match star {
                    StarLookupMatch::Identifier(star) => StarIdentifierMatch {
                        designation: star.identifier.to_string(),
                        stable_id: star.identifier.stable_id(),
                        catalog: star.identifier.namespace().as_str().to_owned(),
                        kind: "identifier".to_owned(),
                        detail: String::new(),
                        ra_deg: star.ra,
                        dec_deg: star.dec,
                        mag: Some(star.mag),
                    },
                    StarLookupMatch::Name(star) => StarIdentifierMatch {
                        designation: star.designation.to_owned(),
                        stable_id: star.stable_id.to_owned(),
                        catalog: star.catalog.as_str().to_owned(),
                        kind: star.kind.as_str().to_owned(),
                        detail: star.detail.to_owned(),
                        ra_deg: star.ra,
                        dec_deg: star.dec,
                        mag: star.mag,
                    },
                })
                .collect()
        };
        Ok(matches)
    }

    pub fn labels_in_footprint(
        &self,
        wcs: &Wcs,
        dimensions: (u32, u32),
        mag_limit: f32,
        limit: usize,
    ) -> Vec<StarLabel> {
        if limit == 0 {
            return Vec::new();
        }
        let width = dimensions.0 as f64;
        let height = dimensions.1 as f64;
        let center = wcs.pixel_to_world(width / 2.0, height / 2.0);
        let radius = [(0.0, 0.0), (width, 0.0), (width, height), (0.0, height)]
            .into_iter()
            .map(|(x, y)| wcs.pixel_to_world(x, y))
            .map(|corner| angular_separation_deg(center.0, center.1, corner.0, corner.1))
            .fold(0.0, f64::max)
            .min(180.0);

        let dec_min = (center.1 - radius).clamp(-90.0, 90.0);
        let dec_max = (center.1 + radius).clamp(-90.0, 90.0);
        let dec_start = dec_bin(dec_min);
        let dec_end = dec_bin(dec_max);
        let reaches_pole = dec_min <= -90.0 || dec_max >= 90.0;
        let edge_cos = dec_min.abs().max(dec_max.abs()).to_radians().cos().abs();
        let ra_half_span = if reaches_pole || edge_cos < 0.05 {
            180.0
        } else {
            (radius / edge_cos).min(180.0)
        };

        let mut labels = Vec::new();
        for dec_index in dec_start..=dec_end {
            for ra_index in 0..RA_BINS {
                let tile_center_ra = (ra_index as f64 + 0.5) * 360.0 / RA_BINS as f64;
                if wrapped_ra_distance(tile_center_ra, center.0)
                    > ra_half_span + 180.0 / RA_BINS as f64
                {
                    continue;
                }
                for label in &self.tiles[dec_index * RA_BINS + ra_index] {
                    if label.mag.is_some_and(|mag| mag > mag_limit)
                        || angular_separation_deg(center.0, center.1, label.ra, label.dec)
                            > radius * 1.05
                    {
                        continue;
                    }
                    let Some((x, y)) = wcs.world_to_pixel(label.ra, label.dec) else {
                        continue;
                    };
                    if x >= 0.0 && x <= width && y >= 0.0 && y <= height {
                        labels.push(label.clone());
                    }
                }
            }
        }
        labels.sort_by(|left, right| {
            left.mag
                .unwrap_or(f32::INFINITY)
                .total_cmp(&right.mag.unwrap_or(f32::INFINITY))
                .then_with(|| label_rank(left).cmp(&label_rank(right)))
                .then_with(|| left.designation.cmp(&right.designation))
        });
        labels.truncate(limit);
        labels
    }
}

fn tile_index(ra: f64, dec: f64) -> usize {
    let ra = ra.rem_euclid(360.0);
    let ra_bin = ((ra / 360.0 * RA_BINS as f64).floor() as usize).min(RA_BINS - 1);
    dec_bin(dec) * RA_BINS + ra_bin
}

fn dec_bin(dec: f64) -> usize {
    (((dec.clamp(-90.0, 90.0) + 90.0) / 180.0 * DEC_BINS as f64).floor() as usize).min(DEC_BINS - 1)
}

fn wrapped_ra_distance(left: f64, right: f64) -> f64 {
    let difference = (left - right).rem_euclid(360.0);
    difference.min(360.0 - difference)
}

fn label_rank(label: &StarLabel) -> (u8, bool, usize, &str) {
    let kind = match label.kind {
        StarNameKind::ProperName => 0,
        StarNameKind::BayerFlamsteed => 1,
        StarNameKind::VariableStar => 2,
        StarNameKind::DoubleStar => 3,
    };
    (
        kind,
        label.mag.is_none(),
        label.designation.len(),
        &label.designation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use seiza::star_ids::{
        StarIdentifier, StarIdentifierCatalogBuilder, StarNameCatalog, StarNameKind,
    };

    fn fixture() -> (std::path::PathBuf, StarIdentifierLayer) {
        let path = std::env::temp_dir().join(format!(
            "seiza-server-star-identifiers-{}.bin",
            uuid::Uuid::now_v7()
        ));
        let mut builder = StarIdentifierCatalogBuilder::new(2025.5, "test catalog");
        let tycho = StarIdentifier::Tycho2 {
            region: 5949,
            number: 2777,
            component: 1,
        };
        builder.add(tycho, 10.0, 20.0, 6.2).unwrap();
        builder
            .add_name(
                StarNameCatalog::BrightStarCatalog,
                StarNameKind::BayerFlamsteed,
                "Alpha Test",
                "hr:1",
                "",
                10.0,
                20.0,
                Some(6.2),
            )
            .unwrap();
        builder
            .add_name(
                StarNameCatalog::IauCatalogOfStarNames,
                StarNameKind::ProperName,
                "Test Star",
                "hr:1",
                "",
                10.0,
                20.0,
                Some(6.2),
            )
            .unwrap();
        builder.write_to(&path).unwrap();
        let layer = StarIdentifierLayer::open(&path).unwrap();
        (path, layer)
    }

    #[test]
    fn searches_numeric_and_textual_identifiers() {
        let (path, layer) = fixture();
        let numeric = layer.search("TYC 5949-2777-1", false, 10).unwrap();
        assert_eq!(numeric[0].stable_id, "tycho2:5949-2777-1");
        let named = layer.search("Test", true, 10).unwrap();
        assert_eq!(named[0].designation, "Test Star");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn indexes_one_preferred_label_per_stable_star() {
        let (path, layer) = fixture();
        assert_eq!(layer.label_count(), 1);
        let wcs = Wcs {
            crval: (10.0, 20.0),
            crpix: (100.0, 100.0),
            cd: [[-0.001, 0.0], [0.0, -0.001]],
        };
        let labels = layer.labels_in_footprint(&wcs, (200, 200), 10.0, 10);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].designation, "Test Star");
        std::fs::remove_file(path).unwrap();
    }
}
