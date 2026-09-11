use super::*;

/// Plot Valve's ordered percentages without inventing game-clock timestamps.
#[must_use]
pub fn draw_win_probability_graph(values: &[f64], match_id: i64) -> Option<Cursor<Vec<u8>>> {
    if values.len() < 2
        || values.len() > 4096
        || values
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=100.0).contains(v))
    {
        return None;
    }
    const WIDTH: i32 = 790;
    const HEIGHT: i32 = 360;
    let mut raster = Raster::new(WIDTH as usize, HEIGHT as usize, DISCORD_BG);
    let projection = AdvantageProjection {
        left: 56,
        right: 774,
        top: 42,
        bottom: 286,
        minimum: 0.0,
        maximum: 100.0,
    };
    raster.fill_rect(
        projection.left,
        projection.top,
        projection.right,
        projection.bottom,
        DISCORD_DARKER,
    );
    let title = format!("Radiant Win Probability - Match #{match_id}");
    raster.text(
        (WIDTH - Raster::text_width(&title, 1)) / 2,
        12,
        &title,
        DISCORD_WHITE,
        1,
    );
    for percentage in [0, 25, 50, 75, 100] {
        let y = projection.y(f64::from(percentage));
        raster.line((projection.left, y), (projection.right, y), DISCORD_GRID, 1);
        let label = format!("{percentage}%");
        raster.text(
            projection.left - Raster::text_width(&label, 1) - 8,
            y - 4,
            &label,
            DISCORD_GREY,
            1,
        );
    }
    draw_advantage_series(&mut raster, values, projection, DISCORD_GREEN, 2, false);
    for tick in 0..=4 {
        let index = tick * (values.len() - 1) / 4;
        let (x, _) = projection.point(index, values.len(), 0.0);
        let label = index.to_string();
        raster.text(
            (x - Raster::text_width(&label, 1) / 2)
                .min(projection.right - Raster::text_width(&label, 1)),
            298,
            &label,
            DISCORD_GREY,
            1,
        );
    }
    for (y, label) in [
        (320, "Ordered sample index"),
        (341, "Postgame history; sample timestamps are not available"),
    ] {
        raster.text(
            (WIDTH - Raster::text_width(label, 1)) / 2,
            y,
            label,
            DISCORD_GREY,
            1,
        );
    }
    Some(render(raster))
}
