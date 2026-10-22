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

/// Filtro de escala: `Fast` enquanto o slider de tamanho se move, `Final` quando ele para.
#[derive(Clone, Copy)]
pub enum Quality {
    Fast,
    Final,
}

/// Quadros recortados no tamanho original da folha. Guardados só enquanto o usuário
/// redimensiona, para não decodificar a spritesheet a cada passo do slider.
pub struct Cells {
    frame_w: u32,
    frame_h: u32,
    default: String,
    anims: Vec<(String, Vec<(RgbaImage, u32)>)>,
    icon: Vec<u8>,
}

impl Cells {
    pub fn load(pet_dir: &Path) -> Result<Cells, String> {
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
                        (cell, duration_ms)
                    })
                    .collect();
                (name.clone(), frames)
            })
            .collect();

        let first = &layout.animations[&layout.default];
        let icon_cell = imageops::crop_imm(&sheet, 0, first.row * fh, fw, fh).to_image();
        Ok(Cells {
            frame_w: fw,
            frame_h: fh,
            default: layout.default,
            anims,
            icon: icon_bgra(&icon_cell),
        })
    }

    pub fn scale(&self, target_width: u32, quality: Quality) -> Sprite {
        self.scale_where(target_width, quality, |_| true)
    }

    /// Só a animação `name` (ou a padrão, se o pet não tiver essa): é o que aparece na tela
    /// enquanto o slider se move, e custa uma fração de escalar todas.
    pub fn scale_only(&self, target_width: u32, name: &str, quality: Quality) -> Sprite {
        let name = if self.anims.iter().any(|(n, _)| n == name) {
            name.to_string()
        } else {
            self.default.clone()
        };
        let mut sprite = self.scale_where(target_width, quality, |n| n == name);
        sprite.default = name;
        sprite
    }

    fn scale_where(
        &self,
        target_width: u32,
        quality: Quality,
        keep: impl Fn(&str) -> bool,
    ) -> Sprite {
        let width = target_width.max(16);
        let height = ((self.frame_h as f64) * (width as f64) / (self.frame_w as f64))
            .round()
            .max(1.0) as u32;
        let filter = match quality {
            Quality::Fast => FilterType::Triangle,
            Quality::Final => FilterType::CatmullRom,
        };
        let anims = self
            .anims
            .iter()
            .filter(|(name, _)| keep(name))
            .map(|(name, cells)| {
                let frames = cells
                    .iter()
                    .map(|(cell, duration_ms)| Frame {
                        bgra: to_premultiplied_bgra(&imageops::resize(cell, width, height, filter)),
                        duration_ms: *duration_ms,
                    })
                    .collect();
                (name.clone(), frames)
            })
            .collect();
        Sprite {
            width,
            height,
            default: self.default.clone(),
            anims,
            icon: self.icon.clone(),
        }
    }
}

impl Sprite {
    pub fn build(pet_dir: &Path, target_width: u32) -> Result<Sprite, String> {
        Ok(Cells::load(pet_dir)?.scale(target_width, Quality::Final))
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
    fn scale_only_keeps_just_the_visible_animation() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/pets/tatu-canastra");
        let cells = Cells::load(&dir).unwrap();
        let full = cells.scale(120, Quality::Fast);
        let only = cells.scale_only(120, "running", Quality::Fast);
        assert_eq!(only.anims.len(), 1);
        assert_eq!(only.default, "running");
        assert_eq!(only.frames("running").len(), full.frames("running").len());
        // Pedir uma animação que o pet não tem cai na padrão, e `frames` continua achando quadros.
        let fallback = cells.scale_only(120, "nao-existe", Quality::Fast);
        assert_eq!(fallback.default, full.default);
        assert!(!fallback.frames("qualquer").is_empty());

        let t = std::time::Instant::now();
        for w in 100..120 {
            cells.scale_only(w, "running", Quality::Fast);
        }
        let partial = t.elapsed() / 20;
        let t = std::time::Instant::now();
        for w in 100..105 {
            cells.scale(w, Quality::Fast);
        }
        let all = t.elapsed() / 5;
        eprintln!("por passo do slider: só a animação {partial:?}, todas {all:?}");
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
