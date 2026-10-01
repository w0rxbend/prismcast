//! Framework-free scene layout shared by native rendering and preview tools.
//! Crop -> source-axis flip -> cardinal rotation -> canvas-axis scale/bounds.
use prismcast_core::{Anchor, BoundsKind, Crop, Error, Result, SceneItem, Source, SourceKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
impl RenderRect {
    /// Left/top inclusive, right/bottom exclusive, matching rendered pixels.
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x as f64
            && y >= self.y as f64
            && x < self.x as f64 + self.width as f64
            && y < self.y as f64 + self.height as f64
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardinalRotation {
    None,
    Clockwise,
    HalfTurn,
    Counterclockwise,
}
impl CardinalRotation {
    pub fn swaps_axes(self) -> bool {
        matches!(self, Self::Clockwise | Self::Counterclockwise)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemLayout {
    pub rect: RenderRect,
    /// Actual native crop edges, clamped left/top first to retain one pixel.
    pub crop: Crop,
    /// Cropped intrinsic dimensions after cardinal rotation, before scaling.
    pub rotated_source_size: SourceSize,
    pub rotation: CardinalRotation,
    pub flip_x: bool,
    pub flip_y: bool,
    pub rotation_quantized: bool,
    pub scale_clamped: bool,
}
fn invalid(message: &str) -> Error {
    Error::InvalidInput(message.into())
}

pub fn anchor_fractions(anchor: Anchor) -> (f32, f32) {
    match anchor {
        Anchor::TopLeft => (0.0, 0.0),
        Anchor::Top => (0.5, 0.0),
        Anchor::TopRight => (1.0, 0.0),
        Anchor::Left => (0.0, 0.5),
        Anchor::Center => (0.5, 0.5),
        Anchor::Right => (1.0, 0.5),
        Anchor::BottomLeft => (0.0, 1.0),
        Anchor::Bottom => (0.5, 1.0),
        Anchor::BottomRight => (1.0, 1.0),
    }
}

/// Resolve the known test-pattern dimensions without depending on native APIs.
/// Validates dimensions only; the source backend validates the full settings.
pub fn test_pattern_source_size(source: &Source) -> Result<SourceSize> {
    if source.kind != SourceKind::TestPattern {
        return Err(invalid(
            "source dimensions are unavailable for this source kind",
        ));
    }
    if !source.settings.is_null() && !source.settings.is_object() {
        return Err(invalid("test-pattern settings must be an object or null"));
    }
    let dimension = |name: &str, default: u32| -> Result<u32> {
        match source.settings.get(name) {
            None => Ok(default),
            Some(value) => value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| (1..=8192).contains(n))
                .ok_or_else(|| invalid("test-pattern dimensions must be integers in 1..8192")),
        }
    };
    Ok(SourceSize {
        width: dimension("width", 1920)?,
        height: dimension("height", 1080)?,
    })
}

/// Derive the exact native rectangle, validating before any graph mutation.
/// Position is bounded to +/-1M pixels, extents to8192, finite scales clamp to64.
/// Fractional geometry stays f32 until final half-away-from-zero rounding.
pub fn layout_item(item: &SceneItem, source: SourceSize) -> Result<ItemLayout> {
    if !(1..=8192).contains(&source.width) || !(1..=8192).contains(&source.height) {
        return Err(invalid("source dimensions must be in 1..8192"));
    }
    let transform = item.transform;
    if !transform.position.x.is_finite()
        || !transform.position.y.is_finite()
        || transform.position.x.abs() > 1_000_000.0
        || transform.position.y.abs() > 1_000_000.0
        || !transform.scale.x.is_finite()
        || !transform.scale.y.is_finite()
        || !transform.rotation.is_finite()
        || !item.opacity.is_finite()
        || !(0.0..=1.0).contains(&item.opacity)
    {
        return Err(invalid(
            "nonfinite or excessive position/scale/rotation/opacity",
        ));
    }
    // Avoid summing attacker-controlled u32 edges (overflow); prefer left/top.
    let left = item.crop.left.min(source.width - 1);
    let right = item.crop.right.min(source.width - left - 1);
    let top = item.crop.top.min(source.height - 1);
    let bottom = item.crop.bottom.min(source.height - top - 1);
    let crop = Crop {
        left,
        right,
        top,
        bottom,
    };
    let angle = transform.rotation.rem_euclid(360.0);
    let step = (angle / 90.0).round() as u32 % 4;
    let rotation = match step {
        0 => CardinalRotation::None,
        1 => CardinalRotation::Clockwise,
        2 => CardinalRotation::HalfTurn,
        _ => CardinalRotation::Counterclockwise,
    };
    let mut width = (source.width - left - right) as f32;
    let mut height = (source.height - top - bottom) as f32;
    if rotation.swaps_axes() {
        std::mem::swap(&mut width, &mut height);
    }
    let scale_x = transform.scale.x.abs().min(64.0);
    let scale_y = transform.scale.y.abs().min(64.0);
    let (ax, ay) = anchor_fractions(transform.anchor);
    let (draw_width, draw_height, x, y) = match item.bounds.kind {
        BoundsKind::None => {
            let dw = width * scale_x;
            let dh = height * scale_y;
            (
                dw,
                dh,
                transform.position.x - ax * dw,
                transform.position.y - ay * dh,
            )
        }
        kind => {
            let bw = item.bounds.size.x;
            let bh = item.bounds.size.y;
            if !bw.is_finite()
                || !bh.is_finite()
                || bw <= 0.0
                || bh <= 0.0
                || bw > 8192.0
                || bh > 8192.0
            {
                return Err(invalid(
                    "bounds must have finite positive dimensions <=8192",
                ));
            }
            let (dw, dh) = match kind {
                BoundsKind::Stretch => (bw, bh),
                BoundsKind::FitInner => {
                    let scale = (bw / width).min(bh / height);
                    (width * scale, height * scale)
                }
                BoundsKind::FitOuter => {
                    let scale = (bw / width).max(bh / height);
                    (width * scale, height * scale)
                }
                BoundsKind::None => return Err(invalid("unreachable bounds branch")),
            };
            let (align_x, align_y) = anchor_fractions(item.bounds.alignment);
            (
                dw,
                dh,
                transform.position.x - ax * bw + align_x * (bw - dw),
                transform.position.y - ay * bh + align_y * (bh - dh),
            )
        }
    };
    if !draw_width.is_finite()
        || !draw_height.is_finite()
        || draw_width > 8192.0
        || draw_height > 8192.0
    {
        return Err(invalid("rendered dimensions exceed8192"));
    }
    Ok(ItemLayout {
        rect: RenderRect {
            x: x.round() as i32,
            y: y.round() as i32,
            width: draw_width.round().max(1.0) as u32,
            height: draw_height.round().max(1.0) as u32,
        },
        crop,
        rotated_source_size: SourceSize {
            width: width as u32,
            height: height as u32,
        },
        rotation,
        flip_x: transform.scale.x < 0.0,
        flip_y: transform.scale.y < 0.0,
        rotation_quantized: angle % 90.0 != 0.0,
        scale_clamped: transform.scale.x.abs() > 64.0 || transform.scale.y.abs() > 64.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prismcast_core::{Bounds, Vec2};
    fn item() -> SceneItem {
        SceneItem::new(prismcast_core::SourceId::new(), 0)
    }
    const ANCHORS: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];
    #[test]
    fn all_anchors_match_exact_native_rect_and_hit_edges() {
        let expected = [
            (12, 8),
            (9, 8),
            (6, 8),
            (12, 6),
            (9, 6),
            (6, 6),
            (12, 4),
            (9, 4),
            (6, 4),
        ];
        for (anchor, (x, y)) in ANCHORS.into_iter().zip(expected) {
            let mut item = item();
            item.transform.position = Vec2::new(12.0, 8.0);
            item.transform.anchor = anchor;
            let layout = layout_item(
                &item,
                SourceSize {
                    width: 6,
                    height: 4,
                },
            )
            .unwrap();
            assert_eq!(
                layout.rect,
                RenderRect {
                    x,
                    y,
                    width: 6,
                    height: 4
                }
            );
            assert!(layout.rect.contains(x as f64, y as f64));
            assert!(!layout.rect.contains((x + 6) as f64, y as f64));
            assert!(!layout.rect.contains(x as f64, (y + 4) as f64));
            assert!(!layout.rect.contains(f64::NAN, y as f64));
        }
    }
    #[test]
    fn huge_crop_edges_leave_exactly_one_pixel_without_overflow() {
        let mut item = item();
        item.crop = Crop {
            left: u32::MAX,
            right: u32::MAX,
            top: u32::MAX,
            bottom: u32::MAX,
        };
        let layout = layout_item(
            &item,
            SourceSize {
                width: 6,
                height: 4,
            },
        )
        .unwrap();
        assert_eq!(
            layout.crop,
            Crop {
                left: 5,
                right: 0,
                top: 3,
                bottom: 0
            }
        );
        assert_eq!(
            layout.rotated_source_size,
            SourceSize {
                width: 1,
                height: 1
            }
        );
        assert_eq!((layout.rect.width, layout.rect.height), (1, 1));
    }
    #[test]
    fn orientation_precedes_canvas_axis_scaling_and_handles_zero_signs() {
        let mut item = item();
        item.transform.rotation = 90.0;
        item.transform.scale = Vec2::new(-2.0, 3.0);
        let layout = layout_item(
            &item,
            SourceSize {
                width: 6,
                height: 4,
            },
        )
        .unwrap();
        assert_eq!((layout.rect.width, layout.rect.height), (8, 18));
        assert_eq!(
            layout.rotated_source_size,
            SourceSize {
                width: 4,
                height: 6
            }
        );
        assert!(layout.flip_x);
        assert!(!layout.flip_y);
        assert!(!layout.rotation_quantized);
        item.transform.scale = Vec2::new(0.0, -0.0);
        let layout = layout_item(
            &item,
            SourceSize {
                width: 6,
                height: 4,
            },
        )
        .unwrap();
        assert_eq!((layout.rect.width, layout.rect.height), (1, 1));
        assert!(!layout.flip_x);
        assert!(!layout.flip_y);
        item.transform.anchor = Anchor::Center;
        let collapsed = layout_item(
            &item,
            SourceSize {
                width: 32,
                height: 16,
            },
        )
        .unwrap();
        assert_eq!(
            collapsed.rect,
            RenderRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            }
        );
        item.transform.scale = Vec2::new(0.01, 0.01);
        let subpixel = layout_item(
            &item,
            SourceSize {
                width: 32,
                height: 16,
            },
        )
        .unwrap();
        assert_eq!(
            subpixel.rect,
            RenderRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            }
        );
        item.transform.rotation = -45.0;
        assert_eq!(
            layout_item(
                &item,
                SourceSize {
                    width: 6,
                    height: 4
                }
            )
            .unwrap()
            .rotation,
            CardinalRotation::None
        );
        item.transform.rotation = 45.0;
        let layout = layout_item(
            &item,
            SourceSize {
                width: 6,
                height: 4,
            },
        )
        .unwrap();
        assert_eq!(layout.rotation, CardinalRotation::Clockwise);
        assert!(layout.rotation_quantized);
    }
    #[test]
    fn bounds_modes_and_alignment_apply_inside_anchor_rectangle() {
        for (kind, expected) in [
            (
                BoundsKind::Stretch,
                RenderRect {
                    x: 90,
                    y: 90,
                    width: 20,
                    height: 20,
                },
            ),
            (
                BoundsKind::FitInner,
                RenderRect {
                    x: 90,
                    y: 95,
                    width: 20,
                    height: 10,
                },
            ),
            (
                BoundsKind::FitOuter,
                RenderRect {
                    x: 80,
                    y: 90,
                    width: 40,
                    height: 20,
                },
            ),
        ] {
            let mut item = item();
            item.transform.position = Vec2::new(100.0, 100.0);
            item.transform.anchor = Anchor::Center;
            item.bounds = Bounds {
                kind,
                size: Vec2::new(20.0, 20.0),
                alignment: Anchor::Center,
            };
            assert_eq!(
                layout_item(
                    &item,
                    SourceSize {
                        width: 8,
                        height: 4
                    }
                )
                .unwrap()
                .rect,
                expected
            );
        }
        for (alignment, expected_y) in ANCHORS.into_iter().zip([0, 0, 0, 5, 5, 5, 10, 10, 10]) {
            let mut item = item();
            item.bounds = Bounds {
                kind: BoundsKind::FitInner,
                size: Vec2::new(20.0, 20.0),
                alignment,
            };
            assert_eq!(
                layout_item(
                    &item,
                    SourceSize {
                        width: 8,
                        height: 4
                    }
                )
                .unwrap()
                .rect
                .y,
                expected_y
            );
        }
    }
    #[test]
    fn fractional_negative_placement_rounds_once_and_rejects_excessive_geometry() {
        let mut item = item();
        item.transform.position = Vec2::new(-1.5, 2.5);
        let size = SourceSize {
            width: 6,
            height: 4,
        };
        assert_eq!(
            (
                layout_item(&item, size).unwrap().rect.x,
                layout_item(&item, size).unwrap().rect.y
            ),
            (-2, 3)
        );
        item.transform.rotation = f32::NAN;
        assert!(layout_item(&item, size).is_err());
        item.transform.rotation = 0.0;
        item.transform.scale.x = f32::INFINITY;
        assert!(layout_item(&item, size).is_err());
        item.transform.scale.x = 100.0;
        let layout = layout_item(
            &item,
            SourceSize {
                width: 1,
                height: 1,
            },
        )
        .unwrap();
        assert_eq!(layout.rect.width, 64);
        assert!(layout.scale_clamped);
        assert!(layout_item(
            &item,
            SourceSize {
                width: 8192,
                height: 1
            }
        )
        .is_err());
        item.bounds = Bounds {
            kind: BoundsKind::Stretch,
            size: Vec2::new(0.0, 20.0),
            alignment: Anchor::Center,
        };
        assert!(layout_item(&item, size).is_err());
    }
    #[test]
    fn opaque_source_dimension_defaults_are_available_without_native_bindings() {
        let source = Source::new(SourceKind::TestPattern, "test");
        assert_eq!(
            test_pattern_source_size(&source).unwrap(),
            SourceSize {
                width: 1920,
                height: 1080
            }
        );
        assert!(test_pattern_source_size(&Source::new(SourceKind::Color, "color")).is_err());
    }
}
