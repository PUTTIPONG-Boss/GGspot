//! Mini mode art: a spinning record made of blocks, and the cover as coloured ASCII.

use std::f32::consts::TAU;

use crate::cover::CELLS;

/// Blocks across the record.
pub const SIZE: usize = 48;
/// The record turns in visible steps, like a terminal animation.
pub const STEP_MS: u32 = 66;
const TURN_MS: u32 = 6000;

/// Radii as a fraction of the record's: the cover ends at `LABEL`, then grooves, then the rim.
const HOLE: f32 = 0.09;
const LABEL: f32 = 0.80;
const RIM: f32 = 0.94;

const HOLE_COLOR: [u8; 3] = [6, 8, 6];
const BLANK_LABEL: [u8; 3] = [22, 48, 28];
const GROOVE_DARK: [u8; 3] = [12, 16, 12];
const GROOVE_LIGHT: [u8; 3] = [30, 42, 30];
const RIM_COLOR: [u8; 3] = [57, 211, 83];

const RAMP: &[u8] = b" .:-=+*#%@";
const CELLS_F: f32 = CELLS as f32;
const COVER_LEN: usize = (CELLS * CELLS * 3) as usize;

fn cover_at(cover: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * CELLS as usize + x) * 3;
    [cover[i], cover[i + 1], cover[i + 2]]
}

/// Rotation for a step: tied to the playback position, so pausing stops the record.
pub fn angle(step: u32) -> f32 {
    (step.wrapping_mul(STEP_MS) % TURN_MS) as f32 / TURN_MS as f32 * TAU
}

/// RGBA for `SIZE` x `SIZE` blocks, transparent outside the record.
pub fn render(cover: Option<&[u8]>, angle: f32) -> Vec<u8> {
    let cover = cover.filter(|c| c.len() == COVER_LEN);
    let (sin, cos) = angle.sin_cos();
    let centre = SIZE as f32 / 2.0;
    // The circle inside the square cover fills the label.
    let to_cover = CELLS_F / 2.0 / (LABEL * centre);
    let mut rgba = Vec::with_capacity(SIZE * SIZE * 4);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 + 0.5 - centre, y as f32 + 0.5 - centre);
            let r = (dx * dx + dy * dy).sqrt() / centre;
            // Undo the clockwise turn to find which part of the record is here.
            let (u, v) = (dx * cos + dy * sin, dy * cos - dx * sin);
            let color = if r >= 1.0 {
                rgba.extend_from_slice(&[0; 4]);
                continue;
            } else if r < HOLE {
                HOLE_COLOR
            } else if r < LABEL {
                match cover {
                    Some(c) => {
                        let cell = |p: f32| ((p * to_cover + CELLS_F / 2.0) as usize).min(CELLS as usize - 1);
                        cover_at(c, cell(u), cell(v))
                    }
                    None => BLANK_LABEL,
                }
            } else if r < RIM {
                // Two bright wedges, so a record without a cover still looks like it turns.
                let wedge = v.atan2(u).sin().abs() > 0.92;
                if wedge || (r * centre) as u32 % 2 == 0 { GROOVE_LIGHT } else { GROOVE_DARK }
            } else {
                RIM_COLOR
            };
            rgba.extend_from_slice(&[color[0], color[1], color[2], 255]);
        }
    }
    rgba
}

fn luminance(c: [f32; 3]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

fn sample(cover: &[u8], x: f32, y: f32) -> [f32; 3] {
    let max = CELLS as usize - 1;
    let (x, y) = ((x - 0.5).clamp(0.0, max as f32), (y - 0.5).clamp(0.0, max as f32));
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(max), (y0 + 1).min(max));
    let (fx, fy) = (x.fract(), y.fract());
    let (a, b, c, d) = (cover_at(cover, x0, y0), cover_at(cover, x1, y0), cover_at(cover, x0, y1), cover_at(cover, x1, y1));
    std::array::from_fn(|i| {
        let top = a[i] as f32 * (1.0 - fx) + b[i] as f32 * fx;
        let bottom = c[i] as f32 * (1.0 - fx) + d[i] as f32 * fx;
        top * (1.0 - fy) + bottom * fy
    })
}

/// The cover as `rows` lines of `cols` coloured characters, cropped to fill the
/// area like a wallpaper. `cell_aspect` is a character's height over its width.
/// Contrast is stretched so dark covers still show up.
pub fn ascii(cover: &[u8], cols: usize, rows: usize, cell_aspect: f32) -> Vec<(char, [u8; 3])> {
    if cover.len() != COVER_LEN || cols == 0 || rows == 0 {
        return Vec::new();
    }
    let per_col = CELLS_F / (cols as f32).max(rows as f32 * cell_aspect);
    let per_row = per_col * cell_aspect;
    let samples: Vec<[f32; 3]> = (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (col, row)))
        .map(|(col, row)| {
            let x = CELLS_F / 2.0 + (col as f32 + 0.5 - cols as f32 / 2.0) * per_col;
            let y = CELLS_F / 2.0 + (row as f32 + 0.5 - rows as f32 / 2.0) * per_row;
            sample(cover, x, y)
        })
        .collect();
    let (low, high) = samples.iter().map(|c| luminance(*c)).fold((f32::MAX, f32::MIN), |(l, h), v| (l.min(v), h.max(v)));
    let range = high - low;
    samples
        .iter()
        .map(|&c| {
            let level = if range < 1.0 { 0.5 } else { (luminance(c) - low) / range };
            let ch = RAMP[(level * (RAMP.len() - 1) as f32).round() as usize] as char;
            // Keep the hue, but stay dimmer than the record and the text on top.
            let target = 70.0 + 110.0 * level;
            let peak = c[0].max(c[1]).max(c[2]).max(1.0);
            (ch, c.map(|v| (v * target / peak) as u8))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Left half `left`, right half `right`.
    fn split_cover(left: [u8; 3], right: [u8; 3]) -> Vec<u8> {
        (0..CELLS * CELLS).flat_map(|i| if i % CELLS < CELLS / 2 { left } else { right }).collect()
    }

    fn block(rgba: &[u8], x: usize, y: usize) -> [u8; 4] {
        let i = (y * SIZE + x) * 4;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    const RED: [u8; 3] = [200, 0, 0];
    const BLUE: [u8; 3] = [0, 0, 200];

    #[test]
    fn record_is_round_with_a_hole() {
        let rgba = render(None, 0.0);
        assert_eq!(rgba.len(), SIZE * SIZE * 4);
        assert_eq!(block(&rgba, 0, 0)[3], 0, "corner is see-through");
        assert_eq!(block(&rgba, SIZE / 2, SIZE / 2), [6, 8, 6, 255], "spindle hole");
    }

    #[test]
    fn half_a_turn_swaps_the_sides_of_the_cover() {
        let cover = split_cover(RED, BLUE);
        let (x, y) = (SIZE / 4, SIZE / 2);
        assert_eq!(block(&render(Some(&cover), 0.0), x, y), [200, 0, 0, 255]);
        assert_eq!(block(&render(Some(&cover), TAU / 2.0), x, y), [0, 0, 200, 255]);
    }

    #[test]
    fn a_quarter_turn_is_clockwise() {
        let cover = split_cover(RED, BLUE);
        // The red left side swings up to the top.
        let turned = render(Some(&cover), TAU / 4.0);
        assert_eq!(block(&turned, SIZE / 2, SIZE / 4), [200, 0, 0, 255]);
        assert_eq!(block(&turned, SIZE / 2, SIZE * 3 / 4), [0, 0, 200, 255]);
    }

    #[test]
    fn angle_follows_the_position_and_wraps() {
        assert_eq!(angle(0), 0.0);
        assert!(angle(1) > 0.0);
        assert!(angle(TURN_MS / STEP_MS + 1) < angle(TURN_MS / STEP_MS));
        assert!(angle(u32::MAX) < TAU);
    }

    #[test]
    fn bright_parts_get_dense_characters_and_keep_their_colour() {
        let cover = split_cover([10, 10, 10], [0, 220, 0]);
        let cells = ascii(&cover, 40, 20, 2.0);
        assert_eq!(cells.len(), 40 * 20);
        assert_eq!(cells[0].0, ' ');
        let (ch, rgb) = cells[39];
        assert_eq!(ch, '@');
        assert!(rgb[1] > 150 && rgb[0] == 0 && rgb[2] == 0, "{rgb:?}");
    }

    #[test]
    fn flat_black_cover_and_bad_input_do_not_panic() {
        let black = vec![0; COVER_LEN];
        assert!(ascii(&black, 10, 10, 2.0).iter().all(|(c, _)| *c == '+'), "a flat cover sits mid-ramp");
        assert!(ascii(&black, 0, 10, 2.0).is_empty());
        assert!(ascii(&[1, 2, 3], 10, 10, 2.0).is_empty());
        assert_eq!(render(Some(&[1, 2, 3]), 0.0).len(), SIZE * SIZE * 4);
    }
}
