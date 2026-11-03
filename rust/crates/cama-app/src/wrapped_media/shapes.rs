//! Antialiased vector primitives for story cards.

use super::*;

fn coverage_span(left: f32, right: f32, x: i32) -> f32 {
    (right.min(x as f32 + 1.0) - left.max(x as f32)).clamp(0.0, 1.0)
}

impl Canvas {
    pub(super) fn clear(&mut self, color: Rgba) {
        for pixel in self.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color.0);
        }
    }

    pub(super) fn disc(&mut self, center: (f32, f32), radius: f32, color: Rgba) {
        if radius > 0.0 {
            self.ring(center, radius, radius + 1.0, color);
        }
    }

    /// A band `band` pixels wide just inside `radius`.
    pub(super) fn ring(&mut self, center: (f32, f32), radius: f32, band: f32, color: Rgba) {
        let (cx, cy) = center;
        let inner = radius - band;
        let inner_solid = (inner + 1.0).max(0.0).powi(2);
        let outer_solid = (radius - 1.0).max(0.0).powi(2);
        let top = (cy - radius - 1.0).floor().max(0.0) as i32;
        let bottom = (cy + radius + 1.0).ceil().min(self.height as f32) as i32;
        for y in top..bottom {
            let dy = y as f32 + 0.5 - cy;
            let reach_squared = (radius + 1.0).powi(2) - dy * dy;
            if reach_squared <= 0.0 {
                continue;
            }
            let reach = reach_squared.sqrt();
            let left = (cx - reach).floor().max(0.0) as i32;
            let right = (cx + reach).ceil().min(self.width as f32) as i32;
            for x in left..right {
                let dx = x as f32 + 0.5 - cx;
                let squared = dx * dx + dy * dy;
                if squared >= inner_solid && squared <= outer_solid {
                    self.set(x, y, color);
                    continue;
                }
                let distance = squared.sqrt();
                let edge = (radius - distance + 0.5).clamp(0.0, 1.0)
                    * (distance - inner + 0.5).clamp(0.0, 1.0);
                if edge > 0.0 {
                    self.set(x, y, color.scaled_alpha(edge));
                }
            }
        }
    }

    /// Rounded rectangle given as `(left, top, right, bottom)`.
    pub(super) fn rounded_rect(&mut self, rect: (f32, f32, f32, f32), radius: f32, color: Rgba) {
        let (left, top, right, bottom) = rect;
        if right <= left || bottom <= top {
            return;
        }
        let radius = radius.min((right - left) / 2.0).min((bottom - top) / 2.0);
        for y in (top.floor().max(0.0) as i32)..(bottom.ceil().min(self.height as f32) as i32) {
            let row = y as f32 + 0.5;
            let vertical = (row - top + 0.5)
                .clamp(0.0, 1.0)
                .min((bottom - row + 0.5).clamp(0.0, 1.0));
            let corner = if row < top + radius {
                top + radius - row
            } else if row > bottom - radius {
                row - (bottom - radius)
            } else {
                0.0
            };
            let inset = if corner > 0.0 {
                radius - (radius * radius - corner.min(radius).powi(2)).sqrt()
            } else {
                0.0
            };
            let (span_left, span_right) = (left + inset, right - inset);
            for x in (span_left.floor().max(0.0) as i32)
                ..(span_right.ceil().min(self.width as f32) as i32)
            {
                let coverage = coverage_span(span_left, span_right, x) * vertical;
                if coverage > 0.0 {
                    self.set(x, y, color.scaled_alpha(coverage));
                }
            }
        }
    }

    /// Even-odd polygon fill with four sub-scanlines per pixel row.
    pub(super) fn polygon(&mut self, points: &[(f32, f32)], color: Rgba) {
        if points.len() < 3 {
            return;
        }
        let (mut top, mut bottom) = (f32::MAX, f32::MIN);
        for (_, y) in points {
            top = top.min(*y);
            bottom = bottom.max(*y);
        }
        const SAMPLES: i32 = 4;
        let width = self.width;
        let mut row = vec![0.0_f32; width];
        let mut crossings = Vec::with_capacity(8);
        for y in (top.floor().max(0.0) as i32)..(bottom.ceil().min(self.height as f32) as i32) {
            let (mut first, mut last) = (width, 0);
            for sample in 0..SAMPLES {
                let scan = y as f32 + (sample as f32 + 0.5) / SAMPLES as f32;
                crossings.clear();
                for index in 0..points.len() {
                    let (x0, y0) = points[index];
                    let (x1, y1) = points[(index + 1) % points.len()];
                    if (y0 <= scan) != (y1 <= scan) {
                        crossings.push(x0 + (scan - y0) / (y1 - y0) * (x1 - x0));
                    }
                }
                crossings.sort_by(f32::total_cmp);
                for pair in crossings.chunks_exact(2) {
                    let from = pair[0].floor().max(0.0) as usize;
                    let to = (pair[1].ceil().max(0.0) as usize).min(width);
                    first = first.min(from);
                    last = last.max(to);
                    for (x, value) in row.iter_mut().enumerate().take(to).skip(from) {
                        *value += coverage_span(pair[0], pair[1], x as i32) / SAMPLES as f32;
                    }
                }
            }
            for (x, value) in row.iter_mut().enumerate().take(last).skip(first) {
                if *value > 0.0 {
                    self.set(x as i32, y, color.scaled_alpha(*value));
                }
                *value = 0.0;
            }
        }
    }

    /// Opaque round-capped polyline.
    pub(super) fn stroke(&mut self, points: &[(f32, f32)], width: f32, color: Rgba) {
        let half = width / 2.0;
        for pair in points.windows(2) {
            let (x0, y0) = pair[0];
            let (x1, y1) = pair[1];
            let length = (x1 - x0).hypot(y1 - y0);
            if length > 0.0 {
                let (nx, ny) = (-(y1 - y0) / length * half, (x1 - x0) / length * half);
                self.polygon(
                    &[
                        (x0 + nx, y0 + ny),
                        (x1 + nx, y1 + ny),
                        (x1 - nx, y1 - ny),
                        (x0 - nx, y0 - ny),
                    ],
                    color,
                );
            }
        }
        for point in points {
            self.disc(*point, half, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLANK: Rgba = Rgba::rgb(0, 0, 0);
    const MARK: Rgba = Rgba::rgb(255, 255, 255);

    fn pixel(canvas: &Canvas, x: usize, y: usize) -> u8 {
        canvas.pixels[(y * canvas.width + x) * 4]
    }

    #[test]
    fn shapes_are_solid_inside_empty_outside_and_soft_on_the_edge() {
        let mut canvas = Canvas::new(40, 40, BLANK);
        canvas.disc((20.0, 20.0), 10.0, MARK);
        assert_eq!(pixel(&canvas, 20, 20), 255);
        assert_eq!(pixel(&canvas, 2, 2), 0);
        assert!(
            canvas
                .pixels
                .chunks_exact(4)
                .any(|p| p[0] > 0 && p[0] < 255),
            "the edge is antialiased"
        );
    }

    #[test]
    fn ring_leaves_its_center_open() {
        let mut canvas = Canvas::new(40, 40, BLANK);
        canvas.ring((20.0, 20.0), 15.0, 4.0, MARK);
        assert_eq!(pixel(&canvas, 20, 20), 0);
        assert_eq!(pixel(&canvas, 20, 7), 255);
    }

    #[test]
    fn rounded_rect_rounds_corners_only() {
        let mut canvas = Canvas::new(40, 40, BLANK);
        canvas.rounded_rect((5.0, 5.0, 35.0, 35.0), 10.0, MARK);
        assert_eq!(pixel(&canvas, 5, 5), 0, "corner is cut");
        assert_eq!(pixel(&canvas, 20, 5), 255, "edge is filled");
        assert_eq!(pixel(&canvas, 20, 20), 255, "center is filled");
    }

    #[test]
    fn polygon_and_stroke_fill_expected_regions_and_clip_to_the_canvas() {
        let mut canvas = Canvas::new(40, 40, BLANK);
        canvas.polygon(
            &[(-10.0, -10.0), (20.0, -10.0), (20.0, 20.0), (-10.0, 20.0)],
            MARK,
        );
        assert_eq!(pixel(&canvas, 10, 10), 255);
        assert_eq!(pixel(&canvas, 30, 30), 0);
        let mut line = Canvas::new(40, 40, BLANK);
        line.stroke(&[(5.0, 30.0), (35.0, 30.0)], 6.0, MARK);
        assert_eq!(pixel(&line, 20, 30), 255);
        assert_eq!(pixel(&line, 20, 20), 0);
    }
}
