//! Decodifica a spritesheet uma vez, recorta e escala os quadros usados e descarta a folha.
//! Os quadros ficam prontos para `UpdateLayeredWindow`: BGRA com alpha pré-multiplicado.

use crate::pets;
use image::RgbaImage;
use image::imageops::{self, FilterType};
use std::collections::HashMap;
use std::path::Path;

pub const ICON_SIZE: u32 = 32;

pub struct Frame {
    pub bgra: Vec<u8>,
    pub duration_ms: u32,
}

pub struct Sprite {
    pub width: u32,
    pub height: u32,
    pub default: String,
    pub anims: HashMap<String, Vec<Frame>>,
    /// Ícone da bandeja, 32×32 BGRA com alpha direto (ícones não usam pré-multiplicação).
    pub icon: Vec<u8>,
}

impl Sprite {
    pub fn build(pet_dir: &Path, target_width: u32) -> Result<Sprite, String> {
        let manifest = pets::read_manifest(pet_dir)?;
        let sheet_path = pets::sheet_path(pet_dir, &manifest)?;
        let sheet = image::ImageReader::open(&sheet_path)
            .map_err(|e| format!("não abriu {}: {e}", sheet_path.display()))?
            .with_guessed_format()
            .map_err(|e| e.to_string())?
            .decode()
            .map_err(|e| format!("não decodificou a spritesheet: {e}"))?
            .into_rgba8();
        let layout = pets::resolve_layout(&manifest, sheet.width(), sheet.height())?;

        let (fw, fh) = (layout.frame_w, layout.frame_h);
        let width = target_width.max(16);
        let height = ((fh as f64) * (width as f64) / (fw as f64))
            .round()
            .max(1.0) as u32;
        let anims = layout
            .animations
            .iter()
            .map(|(name, anim)| {
                let frames = anim
                    .durations_ms
                    .iter()
                    .enumerate()
                    .map(|(i, &duration_ms)| {
                        let cell = imageops::crop_imm(&sheet, i as u32 * fw, anim.row * fh, fw, fh)
                            .to_image();
                        let scaled = imageops::resize(&cell, width, height, FilterType::CatmullRom);
                        Frame {
                            bgra: to_premultiplied_bgra(&scaled),
                            duration_ms,
                        }
                    })
                    .collect();
                (name.clone(), frames)
            })
            .collect();

        let first = &layout.animations[&layout.default];
        let icon_cell = imageops::crop_imm(&sheet, 0, first.row * fh, fw, fh).to_image();
        Ok(Sprite {
            width,
            height,
            default: layout.default,
            anims,
            icon: icon_bgra(&icon_cell),
        })
    }

    /// A animação pedida, ou a padrão do pet quando ele não tem aquela linha.
    pub fn frames<'a>(&'a self, name: &str) -> &'a [Frame] {
        self.anims
            .get(name)
            .or_else(|| self.anims.get(&self.default))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn has(&self, name: &str) -> bool {
        self.anims.contains_key(name)
    }
}

fn to_premultiplied_bgra(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(img.as_raw().len());
    for px in img.pixels() {
        let [r, g, b, a] = px.0;
        let premul = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
        out.extend_from_slice(&[premul(b), premul(g), premul(r), a]);
    }
    out
}

fn icon_bgra(cell: &RgbaImage) -> Vec<u8> {
    let scale = ICON_SIZE as f64 / cell.width().max(cell.height()) as f64;
    let w = ((cell.width() as f64 * scale).round() as u32).clamp(1, ICON_SIZE);
    let h = ((cell.height() as f64 * scale).round() as u32).clamp(1, ICON_SIZE);
    let scaled = imageops::resize(cell, w, h, FilterType::CatmullRom);
    let mut canvas = RgbaImage::new(ICON_SIZE, ICON_SIZE);
    imageops::overlay(
        &mut canvas,
        &scaled,
        ((ICON_SIZE - w) / 2) as i64,
        ((ICON_SIZE - h) / 2) as i64,
    );
    canvas
        .pixels()
        .flat_map(|p| [p.0[2], p.0[1], p.0[0], p.0[3]])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplies_and_swaps_to_bgra() {
        let img = RgbaImage::from_raw(2, 1, vec![255, 128, 0, 255, 200, 100, 50, 0]).unwrap();
        assert_eq!(
            to_premultiplied_bgra(&img),
            vec![0, 128, 255, 255, 0, 0, 0, 0]
        );
    }

    #[test]
    fn builds_a_real_pet_if_present() {
        let Some(pet) = pets::discover(&crate::config::default_pets_dir())
            .into_iter()
            .next()
        else {
            return;
        };
        let sprite = Sprite::build(&pet.dir, 160).unwrap();
        assert_eq!(sprite.width, 160);
        assert_eq!(sprite.height, 173);
        assert_eq!(sprite.frames("idle").len(), 6);
        assert_eq!(sprite.frames("idle")[0].bgra.len(), 160 * 173 * 4);
        assert_eq!(sprite.icon.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    }
}
