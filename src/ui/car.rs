//! The car drawing and the regions of it that a department can own.
//!
//! The drawing is a picture of line art, tinted at runtime so that it works
//! in both light and dark mode. On top of it lie hand-traced regions (front
//! wing, sidepods, wheels and so on). Every region has one outline in the
//! side view and one or more in the view from above, so pointing at a part
//! in one view lights it up in the other as well.

use crate::i18n::t;

/// Size of the drawing. Region outlines use the same units.
pub const W: f32 = 1376.0;
pub const H: f32 = 768.0;
/// Outlines with a centre above this line belong to the side view.
pub const SIDE_VIEW_BOTTOM: f32 = 310.0;

/// The line art: white lines on a transparent background.
pub const CAR_PNG: &[u8] = include_bytes!("../../assets/car.png");

/// Loads the line art as a texture.
pub fn load_drawing(ctx: &eframe::egui::Context) -> Option<eframe::egui::TextureHandle> {
    let picture = eframe::icon_data::from_png_bytes(CAR_PNG).ok()?;
    let image = eframe::egui::ColorImage::from_rgba_unmultiplied(
        [picture.width as usize, picture.height as usize],
        &picture.rgba,
    );
    Some(ctx.load_texture("simq-car", image, eframe::egui::TextureOptions::LINEAR))
}

type Point = (f32, f32);

/// One closed outline, ready to be drawn and hit-tested.
pub struct Outline {
    pub points: Vec<Point>,
    /// Triangles that fill the outline, as indices into `points`.
    pub triangles: Vec<[u32; 3]>,
    pub area: f32,
    pub center: Point,
}

pub struct Region {
    /// Stored in the events. Never change an id that has been released.
    pub id: &'static str,
    sv: &'static str,
    en: &'static str,
    pub outlines: Vec<Outline>,
}

impl Region {
    pub fn label(&self) -> &'static str {
        t(self.sv, self.en)
    }

    /// The largest outline in the side view (`true`) or the view from above.
    pub fn largest(&self, side_view: bool) -> Option<&Outline> {
        self.outlines
            .iter()
            .filter(|o| (o.center.1 < SIDE_VIEW_BOTTOM) == side_view)
            .max_by(|a, b| a.area.total_cmp(&b.area))
    }
}

/// All regions of the car.
pub struct Car {
    pub regions: Vec<Region>,
}

impl Car {
    pub fn new() -> Car {
        Car {
            regions: definitions()
                .into_iter()
                .map(|(id, sv, en, outlines)| Region {
                    id,
                    sv,
                    en,
                    outlines: outlines.into_iter().map(outline).collect(),
                })
                .collect(),
        }
    }

    #[cfg(test)]
    pub fn region(&self, id: &str) -> Option<&Region> {
        self.regions.iter().find(|r| r.id == id)
    }

    /// The region at a point of the drawing. Where outlines overlap, the
    /// smallest one wins, so that a small part on top of a large one can be hit.
    pub fn hit(&self, at: Point) -> Option<&Region> {
        self.regions
            .iter()
            .flat_map(|r| r.outlines.iter().map(move |o| (r, o)))
            .filter(|(_, o)| inside(&o.points, at))
            .min_by(|a, b| a.1.area.total_cmp(&b.1.area))
            .map(|(r, _)| r)
    }
}

impl Default for Car {
    fn default() -> Self {
        Car::new()
    }
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

fn signed_area(points: &[Point]) -> f32 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum::<f32>()
        / 2.0
}

/// True if `at` is inside the closed outline (even-odd rule).
fn inside(points: &[Point], at: Point) -> bool {
    let n = points.len();
    let mut hit = false;
    for i in 0..n {
        let (a, b) = (points[i], points[(i + n - 1) % n]);
        if (a.1 > at.1) != (b.1 > at.1) && at.0 < (b.0 - a.0) * (at.1 - a.1) / (b.1 - a.1) + a.0 {
            hit = !hit;
        }
    }
    hit
}

/// Rounds the corners of a hand-traced outline (Chaikin's corner cutting).
fn smooth(points: &[Point]) -> Vec<Point> {
    let n = points.len();
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let (a, b) = (points[i], points[(i + 1) % n]);
        out.push((a.0 * 0.75 + b.0 * 0.25, a.1 * 0.75 + b.1 * 0.25));
        out.push((a.0 * 0.25 + b.0 * 0.75, a.1 * 0.25 + b.1 * 0.75));
    }
    out
}

fn cross(o: Point, a: Point, b: Point) -> f32 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn in_triangle(p: Point, a: Point, b: Point, c: Point) -> bool {
    let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
    let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(negative && positive)
}

/// Splits a simple outline into triangles by cutting off one "ear" at a time.
/// The outline must run counter-clockwise in a y-up sense (positive area).
fn triangulate(points: &[Point]) -> Vec<[u32; 3]> {
    let mut left: Vec<usize> = (0..points.len()).collect();
    let mut triangles = Vec::with_capacity(points.len().saturating_sub(2));
    while left.len() > 3 {
        let n = left.len();
        let ear = (0..n).find(|&i| {
            let (a, b, c) = (
                points[left[(i + n - 1) % n]],
                points[left[i]],
                points[left[(i + 1) % n]],
            );
            cross(a, b, c) > 0.0
                && !left.iter().any(|&k| {
                    let p = points[k];
                    p != a && p != b && p != c && in_triangle(p, a, b, c)
                })
        });
        // A degenerate outline has no proper ear: cut anywhere rather than hang.
        let i = ear.unwrap_or(0);
        triangles.push([
            left[(i + n - 1) % n] as u32,
            left[i] as u32,
            left[(i + 1) % n] as u32,
        ]);
        left.remove(i);
    }
    if left.len() == 3 {
        triangles.push([left[0] as u32, left[1] as u32, left[2] as u32]);
    }
    triangles
}

fn outline((mut points, traced): (Vec<Point>, bool)) -> Outline {
    if traced {
        points = smooth(&smooth(&points));
    }
    if signed_area(&points) < 0.0 {
        points.reverse();
    }
    let area = signed_area(&points);
    let n = points.len() as f32;
    let center = (
        points.iter().map(|p| p.0).sum::<f32>() / n,
        points.iter().map(|p| p.1).sum::<f32>() / n,
    );
    Outline {
        triangles: triangulate(&points),
        points,
        area,
        center,
    }
}

// ---------------------------------------------------------------------------
// The regions, traced over the drawing
// ---------------------------------------------------------------------------

/// A hand-traced outline: its corners are rounded when it is built.
fn traced(points: &[(i32, i32)]) -> (Vec<Point>, bool) {
    (
        points.iter().map(|(x, y)| (*x as f32, *y as f32)).collect(),
        true,
    )
}

/// The same outline on the other side of the car, seen from above.
fn mirrored(points: &[(i32, i32)]) -> (Vec<Point>, bool) {
    let flipped: Vec<(i32, i32)> = points.iter().map(|(x, y)| (*x, 1080 - *y)).collect();
    traced(&flipped)
}

fn circle(cx: f32, cy: f32, radius: f32) -> (Vec<Point>, bool) {
    let points = (0..40)
        .map(|i| {
            let angle = std::f32::consts::TAU * i as f32 / 40.0;
            (cx + radius * angle.cos(), cy + radius * angle.sin())
        })
        .collect();
    (points, false)
}

fn rounded(x0: f32, y0: f32, x1: f32, y1: f32) -> (Vec<Point>, bool) {
    let r = 14.0;
    let mut points = Vec::new();
    for (cx, cy, start) in [
        (x1 - r, y0 + r, -90.0_f32),
        (x1 - r, y1 - r, 0.0),
        (x0 + r, y1 - r, 90.0),
        (x0 + r, y0 + r, 180.0),
    ] {
        for step in 0..=4 {
            let angle = (start + step as f32 * 22.5).to_radians();
            points.push((cx + r * angle.cos(), cy + r * angle.sin()));
        }
    }
    (points, false)
}

type Definition = (
    &'static str,
    &'static str,
    &'static str,
    Vec<(Vec<Point>, bool)>,
);

fn definitions() -> Vec<Definition> {
    const SIDEPOD: [(i32, i32); 12] = [
        (598, 430),
        (640, 400),
        (720, 395),
        (850, 410),
        (950, 440),
        (1040, 470),
        (1000, 498),
        (900, 478),
        (822, 468),
        (790, 446),
        (640, 446),
        (588, 455),
    ];
    const FLOOR: [(i32, i32); 15] = [
        (480, 438),
        (540, 430),
        (600, 410),
        (640, 372),
        (770, 372),
        (1075, 385),
        (1085, 430),
        (1040, 468),
        (950, 438),
        (850, 408),
        (720, 393),
        (640, 398),
        (598, 428),
        (560, 470),
        (500, 485),
    ];
    const FRONT_ARMS: [(i32, i32); 4] = [(318, 418), (392, 418), (432, 497), (308, 508)];
    const REAR_ARMS: [(i32, i32); 6] = [
        (1060, 495),
        (1125, 425),
        (1172, 425),
        (1172, 520),
        (1140, 522),
        (1090, 505),
    ];
    vec![
        (
            "front_wing",
            "Framvinge",
            "Front wing",
            vec![
                traced(&[
                    (57, 272),
                    (98, 256),
                    (160, 205),
                    (229, 190),
                    (235, 250),
                    (245, 274),
                    (190, 281),
                    (120, 281),
                    (62, 278),
                ]),
                traced(&[
                    (122, 350),
                    (232, 345),
                    (230, 440),
                    (200, 468),
                    (165, 512),
                    (165, 570),
                    (200, 618),
                    (228, 655),
                    (236, 730),
                    (150, 742),
                    (122, 730),
                    (68, 560),
                    (66, 535),
                ]),
            ],
        ),
        (
            "nose",
            "Nos",
            "Nose",
            vec![
                traced(&[(230, 192), (268, 180), (268, 238), (236, 247)]),
                traced(&[(410, 152), (510, 137), (515, 150), (440, 165), (420, 170)]),
                traced(&[
                    (92, 540),
                    (100, 522),
                    (115, 515),
                    (260, 513),
                    (310, 507),
                    (420, 500),
                    (425, 580),
                    (310, 573),
                    (260, 568),
                    (115, 568),
                    (100, 560),
                ]),
            ],
        ),
        (
            "front_suspension",
            "Hjulupphängning fram",
            "Front suspension",
            vec![traced(&FRONT_ARMS), mirrored(&FRONT_ARMS)],
        ),
        (
            "front_wheels",
            "Framhjul",
            "Front wheels",
            vec![
                circle(345.0, 218.0, 81.0),
                rounded(266.0, 334.0, 422.0, 412.0),
                rounded(266.0, 667.0, 422.0, 747.0),
            ],
        ),
        (
            "monocoque",
            "Monokock och cockpit",
            "Monocoque and cockpit",
            vec![
                traced(&[
                    (510, 137),
                    (555, 103),
                    (598, 82),
                    (660, 90),
                    (760, 103),
                    (765, 112),
                    (695, 128),
                    (690, 152),
                    (642, 153),
                    (645, 200),
                    (560, 208),
                    (545, 170),
                    (515, 152),
                ]),
                traced(&[
                    (425, 500),
                    (570, 478),
                    (585, 455),
                    (640, 448),
                    (790, 448),
                    (820, 470),
                    (820, 612),
                    (790, 632),
                    (640, 632),
                    (585, 625),
                    (570, 602),
                    (425, 580),
                ]),
            ],
        ),
        (
            "sidepods",
            "Sidopontoner",
            "Sidepods",
            vec![
                traced(&[
                    (645, 155),
                    (760, 158),
                    (860, 132),
                    (990, 135),
                    (995, 190),
                    (1000, 235),
                    (900, 245),
                    (760, 250),
                    (700, 247),
                    (660, 240),
                    (645, 200),
                ]),
                traced(&SIDEPOD),
                mirrored(&SIDEPOD),
            ],
        ),
        (
            "floor",
            "Golv",
            "Floor",
            vec![
                traced(&[
                    (475, 205),
                    (560, 225),
                    (600, 245),
                    (700, 250),
                    (760, 252),
                    (900, 247),
                    (1000, 238),
                    (1065, 250),
                    (1085, 270),
                    (1075, 292),
                    (445, 292),
                    (455, 280),
                    (450, 230),
                ]),
                traced(&FLOOR),
                mirrored(&FLOOR),
            ],
        ),
        (
            "engine_cover",
            "Motorkåpa",
            "Engine cover",
            vec![
                traced(&[
                    (752, 55),
                    (765, 50),
                    (770, 28),
                    (825, 30),
                    (822, 52),
                    (900, 60),
                    (1000, 90),
                    (1100, 107),
                    (1118, 140),
                    (1060, 150),
                    (995, 135),
                    (860, 131),
                    (775, 138),
                    (755, 100),
                ]),
                traced(&[
                    (820, 470),
                    (900, 480),
                    (1000, 500),
                    (1090, 520),
                    (1110, 535),
                    (1110, 548),
                    (1090, 560),
                    (1000, 580),
                    (900, 600),
                    (820, 612),
                ]),
            ],
        ),
        (
            "rear_suspension",
            "Hjulupphängning bak",
            "Rear suspension",
            vec![traced(&REAR_ARMS), mirrored(&REAR_ARMS)],
        ),
        (
            "rear_wheels",
            "Bakhjul",
            "Rear wheels",
            vec![
                circle(1148.0, 218.0, 82.0),
                rounded(1070.0, 334.0, 1222.0, 420.0),
                rounded(1066.0, 652.0, 1222.0, 747.0),
            ],
        ),
        (
            "rear_wing",
            "Bakvinge",
            "Rear wing",
            vec![
                traced(&[
                    (1163, 130),
                    (1180, 82),
                    (1200, 75),
                    (1215, 55),
                    (1255, 55),
                    (1262, 68),
                    (1285, 62),
                    (1293, 55),
                    (1300, 85),
                    (1325, 88),
                    (1322, 185),
                    (1325, 215),
                    (1240, 222),
                    (1232, 180),
                    (1175, 133),
                ]),
                traced(&[
                    (1173, 422),
                    (1240, 418),
                    (1300, 407),
                    (1272, 435),
                    (1272, 645),
                    (1300, 657),
                    (1240, 662),
                    (1173, 655),
                ]),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::valid_region_id;

    #[test]
    fn every_region_is_well_formed_and_fully_filled() {
        let car = Car::new();
        assert_eq!(car.regions.len(), 11);
        for (i, region) in car.regions.iter().enumerate() {
            assert!(valid_region_id(region.id), "{}", region.id);
            assert!(
                car.regions[..i].iter().all(|r| r.id != region.id),
                "{} is defined twice",
                region.id
            );
            // Seen from the side and from above.
            assert!(region.largest(false).is_some(), "{}", region.id);
            for outline in &region.outlines {
                assert!(outline.area > 500.0, "{}", region.id);
                for (x, y) in &outline.points {
                    assert!(
                        (0.0..=W).contains(x) && (0.0..=H).contains(y),
                        "{}",
                        region.id
                    );
                }
                // The triangles cover exactly the outline: none missing, none outside.
                assert_eq!(outline.triangles.len(), outline.points.len() - 2);
                let filled: f32 = outline
                    .triangles
                    .iter()
                    .map(|[a, b, c]| {
                        let p = |i: &u32| outline.points[*i as usize];
                        cross(p(a), p(b), p(c)) / 2.0
                    })
                    .inspect(|area| assert!(*area >= -0.01, "{}", region.id))
                    .sum();
                assert!(
                    (filled - outline.area).abs() < outline.area * 0.001,
                    "{}: {filled} of {}",
                    region.id,
                    outline.area
                );
            }
        }
    }

    #[test]
    fn hits_follow_the_shapes_and_link_the_two_views() {
        let car = Car::new();
        let hit = |x: f32, y: f32| car.hit((x, y)).map(|r| r.id);
        // The same part from the side and from above.
        assert_eq!(hit(150.0, 250.0), Some("front_wing"));
        assert_eq!(hit(150.0, 420.0), Some("front_wing"));
        assert_eq!(hit(1260.0, 130.0), Some("rear_wing"));
        assert_eq!(hit(1220.0, 540.0), Some("rear_wing"));
        assert_eq!(hit(345.0, 218.0), Some("front_wheels"));
        assert_eq!(hit(345.0, 370.0), Some("front_wheels"));
        assert_eq!(hit(345.0, 710.0), Some("front_wheels"));
        assert_eq!(hit(1148.0, 218.0), Some("rear_wheels"));
        assert_eq!(hit(690.0, 540.0), Some("monocoque"));
        assert_eq!(hit(600.0, 120.0), Some("monocoque"));
        assert_eq!(hit(800.0, 200.0), Some("sidepods"));
        assert_eq!(hit(800.0, 425.0), Some("sidepods"));
        assert_eq!(hit(800.0, 655.0), Some("sidepods"));
        assert_eq!(hit(950.0, 100.0), Some("engine_cover"));
        assert_eq!(hit(950.0, 540.0), Some("engine_cover"));
        assert_eq!(hit(700.0, 275.0), Some("floor"));
        assert_eq!(hit(250.0, 540.0), Some("nose"));
        assert_eq!(hit(365.0, 460.0), Some("front_suspension"));
        assert_eq!(hit(365.0, 620.0), Some("front_suspension"));
        assert_eq!(hit(1140.0, 470.0), Some("rear_suspension"));
        // The nose lies on top of the front wing where they cross.
        assert_eq!(hit(140.0, 540.0), Some("nose"));

        // Nothing between the parts or outside the car.
        for (x, y) in [
            (20.0, 20.0),
            (700.0, 320.0),
            (460.0, 380.0),
            (250.0, 430.0),
            (1000.0, 30.0),
            (1350.0, 540.0),
            (445.0, 190.0),
        ] {
            assert_eq!(hit(x, y), None, "({x}, {y})");
        }
        // A wheel is round: its corner is not part of it.
        assert_eq!(hit(345.0 + 70.0, 218.0 - 70.0), None);
    }
}
