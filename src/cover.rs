//! Album covers as a small grid of colour blocks, the terminal look.

use std::path::PathBuf;

use image::imageops::FilterType;

use crate::model::data_dir;

/// Blocks per side of a cover.
pub const CELLS: u32 = 40;
/// Bytes of a decoded cover: RGB per block.
const RGB_LEN: usize = (CELLS * CELLS * 3) as usize;

/// Shrink an encoded image to `CELLS` x `CELLS` RGB bytes.
pub fn to_cells(encoded: &[u8]) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(encoded).map_err(|e| e.to_string())?;
    Ok(img.resize_exact(CELLS, CELLS, FilterType::Triangle).to_rgb8().into_raw())
}

fn cached_path(track_uri: &str) -> PathBuf {
    let id = track_uri.rsplit(':').next().unwrap_or(track_uri);
    data_dir().join("covers").join(format!("{id}.rgb"))
}

pub fn load_cached(track_uri: &str) -> Option<Vec<u8>> {
    std::fs::read(cached_path(track_uri)).ok().filter(|b| b.len() == RGB_LEN)
}

pub fn save_cached(track_uri: &str, cells: &[u8]) {
    let path = cached_path(track_uri);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, cells);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_becomes_one_rgb_triple_per_block() {
        let source = image::RgbImage::from_pixel(300, 300, image::Rgb([200, 40, 20]));
        let mut jpeg = Vec::new();
        source.write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        let cells = to_cells(&jpeg).unwrap();
        assert_eq!(cells.len(), RGB_LEN);
        assert!(cells[0] > 180 && cells[1] < 70 && cells[2] < 50, "colour kept: {:?}", &cells[..3]);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(to_cells(b"not an image").is_err());
    }
}
