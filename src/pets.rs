//! Descoberta e leitura de pets no formato do Codex (`<pasta>/pet.json` + spritesheet).
//! O layout padrão é porte de `codex-pet-sprite-defaults.ts` do Orca, que espelha
//! `codex-rs/tui/src/pets/model.rs`.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST_FILE: &str = "pet.json";
const DEFAULT_SPRITESHEET: &str = "spritesheet.webp";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

pub const CODEX_FRAME_W: u32 = 192;
pub const CODEX_FRAME_H: u32 = 208;
const CODEX_COLUMNS: u32 = 8;
const CODEX_ROWS: u32 = 9;
const DEFAULT_FPS: f64 = 8.0;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub spritesheet_path: Option<String>,
    pub frame: Option<FrameSize>,
    pub fps: Option<f64>,
    pub default_animation: Option<String>,
    #[serde(default, deserialize_with = "animations")]
    pub animations: Option<Animations>,
    #[serde(default)]
    pub interactions: Option<Interactions>,
}

#[derive(Debug)]
pub enum Animations {
    /// Layout explícito do Orca: `row` + `frames`, junto com `frame`.
    Explicit(BTreeMap<String, AnimSpec>),
    /// Animações extras do Codex por cima da grade padrão: reusam uma linha nomeada
    /// (`sourceRow`) com mais quadros que o estado normal usa.
    Extras(BTreeMap<String, ExtraSpec>),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtraSpec {
    pub source_row: String,
    pub frame_count: u32,
    pub timing_ms: Option<Vec<f64>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Interactions {
    pub click: Option<ClickSpec>,
}

/// Só o modo `cycle` existe hoje: cada clique toca a próxima da lista.
#[derive(Debug, Default, Deserialize)]
pub struct ClickSpec {
    #[serde(default)]
    pub animations: Vec<String>,
}

fn animations<'de, D>(d: D) -> Result<Option<Animations>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(raw) = Option::<BTreeMap<String, serde_json::Value>>::deserialize(d)? else {
        return Ok(None);
    };
    if raw.values().any(|v| v.get("sourceRow").is_some()) {
        // Extras são enfeite: a que não faz sentido sai sozinha, sem derrubar o pet.
        let extras = raw
            .into_iter()
            .filter_map(|(name, v)| Some((name, serde_json::from_value(v).ok()?)))
            .collect();
        return Ok(Some(Animations::Extras(extras)));
    }
    let parsed = serde_json::Value::Object(raw.into_iter().collect());
    serde_json::from_value(parsed)
        .map(|specs| Some(Animations::Explicit(specs)))
        .map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub struct FrameSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnimSpec {
    pub row: u32,
    pub frames: u32,
    pub frame_durations_ms: Option<Vec<f64>>,
}

#[derive(Debug, Clone)]
pub struct PetInfo {
    /// Nome da pasta; é o que vai para o config.
    pub id: String,
    pub label: String,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Anim {
    pub row: u32,
    pub durations_ms: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub frame_w: u32,
    pub frame_h: u32,
    pub default: String,
    pub animations: BTreeMap<String, Anim>,
    /// Animações que o clique toca, em ciclo; só nomes que existem em `animations`.
    pub click: Vec<String>,
}

pub fn discover(dir: &Path) -> Vec<PetInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut pets: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().join(MANIFEST_FILE).is_file())
        .map(|e| {
            // Data de inclusão = criação da pasta; atualizar um pet troca os arquivos, não a pasta.
            let added = e.metadata().and_then(|m| m.created()).ok();
            let dir = e.path();
            let id = e.file_name().to_string_lossy().into_owned();
            let manifest = read_manifest(&dir).unwrap_or_default();
            let label = manifest
                .display_name
                .or(manifest.id)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| id.clone());
            (added, PetInfo { id, label, dir })
        })
        .collect();
    pets.sort_by_key(|(added, p)| (*added, p.label.to_lowercase()));
    pets.into_iter().map(|(_, p)| p).collect()
}

/// Vigia a pasta dos pets numa thread (bloqueada no kernel, sem polling) e posta `message`
/// com um `Box<Vec<String>>`: as pastas de pet que mudaram. Vazio = não dá para saber quais
/// (o buffer de mudanças estourou), então trate como se todas tivessem mudado.
pub fn spawn_watch(dir: PathBuf, hwnd: isize, message: u32) {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
        FILE_NOTIFY_CHANGE_SIZE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

    let filter = FILE_NOTIFY_CHANGE_FILE_NAME
        | FILE_NOTIFY_CHANGE_DIR_NAME
        | FILE_NOTIFY_CHANGE_LAST_WRITE
        | FILE_NOTIFY_CHANGE_SIZE;
    crate::system::watch_dir(dir, true, filter, move |entries| {
        // `None` (buffer estourado) deixa `changed` vazio.
        let mut changed: Vec<String> = Vec::new();
        for (_, name) in entries.into_iter().flatten() {
            // Só a primeira parte do caminho: a pasta do pet.
            let top = name.split('\\').next().unwrap_or_default().to_string();
            if !top.is_empty() && !changed.contains(&top) {
                changed.push(top);
            }
        }
        let ptr = Box::into_raw(Box::new(changed));
        // SAFETY: a janela retoma a posse do Box; se o post falhar, liberamos aqui.
        if unsafe { PostMessageW(hwnd as _, message, 0, ptr as isize) } == 0 {
            drop(unsafe { Box::from_raw(ptr) });
        }
    });
}

pub fn read_manifest(dir: &Path) -> Result<Manifest, String> {
    let path = dir.join(MANIFEST_FILE);
    let meta = std::fs::metadata(&path).map_err(|_| "pet.json não encontrado".to_string())?;
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err("pet.json grande demais".into());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("pet.json ilegível: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("pet.json inválido: {e}"))
}

/// Caminho da spritesheet, sempre dentro da pasta do pet.
pub fn sheet_path(dir: &Path, manifest: &Manifest) -> Result<PathBuf, String> {
    let rel = manifest
        .spritesheet_path
        .as_deref()
        .unwrap_or(DEFAULT_SPRITESHEET);
    // Why: o manifest vem de terceiros (npx codex-pets); não deixa apontar para fora da pasta.
    if rel.is_empty()
        || rel.contains('\0')
        || rel.contains("..")
        || rel.contains(':')
        || rel.starts_with('/')
        || rel.starts_with('\\')
    {
        return Err(format!("spritesheetPath inválido: {rel}"));
    }
    Ok(dir.join(rel))
}

fn uniform(frames: u32, fps: f64) -> Vec<u32> {
    vec![(1000.0 / fps).round().max(1.0) as u32; frames as usize]
}

/// Quadros intermediários seguram `frame_ms`, o último segura `final_ms` (o `app_state_animation` do Codex).
fn app_state(frames: u32, frame_ms: u32, final_ms: u32) -> Vec<u32> {
    (0..frames)
        .map(|i| if i == frames - 1 { final_ms } else { frame_ms })
        .collect()
}

pub fn codex_animations(fps: Option<f64>) -> BTreeMap<String, Anim> {
    let table: [(&str, u32, Vec<u32>); 9] = [
        ("idle", 0, vec![1680, 660, 660, 840, 840, 1920]),
        ("running-right", 1, app_state(8, 120, 220)),
        ("running-left", 2, app_state(8, 120, 220)),
        ("waving", 3, app_state(4, 140, 280)),
        ("jumping", 4, app_state(5, 140, 280)),
        ("failed", 5, app_state(8, 140, 240)),
        ("waiting", 6, app_state(6, 150, 260)),
        ("running", 7, app_state(6, 120, 220)),
        ("review", 8, app_state(6, 150, 280)),
    ];
    table
        .into_iter()
        .map(|(name, row, timed)| {
            // Why: fps explícito no manifest vence o ritmo irregular do Codex, como no Orca.
            let durations_ms = match fps {
                Some(fps) => uniform(timed.len() as u32, fps),
                None => timed,
            };
            (name.to_string(), Anim { row, durations_ms })
        })
        .collect()
}

pub fn resolve_layout(m: &Manifest, sheet_w: u32, sheet_h: u32) -> Result<Layout, String> {
    if let Some(fps) = m.fps
        && !(fps > 0.0 && fps <= 60.0)
    {
        return Err(format!("fps fora do intervalo: {fps}"));
    }
    let is_codex_grid = sheet_w == CODEX_FRAME_W * CODEX_COLUMNS
        && sheet_h.is_multiple_of(CODEX_FRAME_H)
        && sheet_h / CODEX_FRAME_H >= CODEX_ROWS;

    let explicit = match &m.animations {
        Some(Animations::Explicit(specs)) => Some(specs),
        _ => None,
    };
    let (frame, mut animations) = match (&m.frame, explicit) {
        (Some(frame), Some(specs)) => (*frame, from_specs(specs, m.fps)?),
        (Some(frame), None) if frame.width == CODEX_FRAME_W && frame.height == CODEX_FRAME_H => {
            (*frame, codex_animations(m.fps))
        }
        (Some(frame), None) => {
            let columns = sheet_w / frame.width.max(1);
            let anim = Anim {
                row: 0,
                durations_ms: uniform(columns.max(1), m.fps.unwrap_or(DEFAULT_FPS)),
            };
            (*frame, BTreeMap::from([("idle".to_string(), anim)]))
        }
        (None, Some(_)) => return Err("animations declarado sem frame".into()),
        (None, None) if is_codex_grid => (
            FrameSize {
                width: CODEX_FRAME_W,
                height: CODEX_FRAME_H,
            },
            codex_animations(m.fps),
        ),
        // Imagem solta: um quadro só, parado.
        (None, None) => (
            FrameSize {
                width: sheet_w,
                height: sheet_h,
            },
            BTreeMap::from([(
                "idle".to_string(),
                Anim {
                    row: 0,
                    durations_ms: vec![1000],
                },
            )]),
        ),
    };

    if let Some(Animations::Extras(extras)) = &m.animations {
        let columns = sheet_w.checked_div(frame.width).unwrap_or(0);
        for (name, extra) in extras {
            // Linha desconhecida ou quadros fora da folha perdem só a animação extra, não o pet.
            let Some(row) = animations.get(&extra.source_row).map(|a| a.row) else {
                continue;
            };
            if extra.frame_count == 0 || extra.frame_count > columns {
                continue;
            }
            let timing = extra.timing_ms.as_deref().unwrap_or(&[]);
            let fallback = 1000.0 / m.fps.unwrap_or(DEFAULT_FPS);
            let durations_ms = (0..extra.frame_count as usize)
                .map(|i| {
                    let ms = timing.get(i).or(timing.last()).copied().unwrap_or(fallback);
                    ms.round().max(1.0) as u32
                })
                .collect();
            animations
                .entry(name.clone())
                .or_insert(Anim { row, durations_ms });
        }
    }
    let click = m
        .interactions
        .as_ref()
        .and_then(|i| i.click.as_ref())
        .map(|c| c.animations.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter(|name| animations.contains_key(*name))
        .cloned()
        .collect();

    if frame.width == 0
        || frame.height == 0
        || !sheet_w.is_multiple_of(frame.width)
        || !sheet_h.is_multiple_of(frame.height)
    {
        return Err(format!(
            "spritesheet {sheet_w}×{sheet_h} não é múltiplo exato do quadro {}×{}",
            frame.width, frame.height
        ));
    }
    let columns = sheet_w / frame.width;
    let rows = sheet_h / frame.height;
    for (name, anim) in &animations {
        if anim.row >= rows {
            return Err(format!(
                "animação \"{name}\" usa a linha {} mas a folha tem {rows}",
                anim.row
            ));
        }
        if anim.durations_ms.len() as u32 > columns {
            return Err(format!(
                "animação \"{name}\" tem {} quadros mas a folha só tem {columns} colunas",
                anim.durations_ms.len()
            ));
        }
    }

    let default = match &m.default_animation {
        Some(name) if animations.contains_key(name) => name.clone(),
        Some(name) => {
            return Err(format!(
                "defaultAnimation \"{name}\" não está em animations"
            ));
        }
        None if animations.contains_key("idle") => "idle".to_string(),
        None => animations
            .keys()
            .next()
            .cloned()
            .ok_or("nenhuma animação")?,
    };

    Ok(Layout {
        frame_w: frame.width,
        frame_h: frame.height,
        default,
        animations,
        click,
    })
}

fn from_specs(
    specs: &BTreeMap<String, AnimSpec>,
    fps: Option<f64>,
) -> Result<BTreeMap<String, Anim>, String> {
    specs
        .iter()
        .map(|(name, spec)| {
            if spec.frames == 0 {
                return Err(format!("animação \"{name}\" sem quadros"));
            }
            let durations_ms = match &spec.frame_durations_ms {
                Some(d) if d.len() as u32 != spec.frames => {
                    return Err(format!(
                        "animação \"{name}\" declara {} durações para {} quadros",
                        d.len(),
                        spec.frames
                    ));
                }
                // Why: teto de 60 s por quadro, para um manifest ruim não congelar o pet.
                Some(d) => d
                    .iter()
                    .map(|ms| ms.clamp(1.0, 60_000.0).round() as u32)
                    .collect(),
                None => uniform(spec.frames, fps.unwrap_or(DEFAULT_FPS)),
            };
            Ok((
                name.clone(),
                Anim {
                    row: spec.row,
                    durations_ms,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(json: &str) -> Manifest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn codex_manifest_gets_codex_layout_on_9_and_11_row_sheets() {
        let m =
            manifest(r#"{"id":"goku","displayName":"Goku","spritesheetPath":"spritesheet.webp"}"#);
        for h in [1872, 2288] {
            let layout = resolve_layout(&m, 1536, h).unwrap();
            assert_eq!((layout.frame_w, layout.frame_h), (192, 208));
            assert_eq!(layout.default, "idle");
            assert_eq!(layout.animations.len(), 9);
            assert_eq!(
                layout.animations["idle"].durations_ms,
                vec![1680, 660, 660, 840, 840, 1920]
            );
            assert_eq!(layout.animations["review"].row, 8);
            assert_eq!(layout.animations["running"].durations_ms.last(), Some(&220));
        }
    }

    #[test]
    fn codex_click_animations_extend_codex_layout() {
        let m = manifest(
            r#"{"animations":{
                  "form-1":{"sourceRow":"waiting","frameCount":8,"timingMs":[190,170]},
                  "form-x":{"sourceRow":"nao-existe","frameCount":8},
                  "form-0":{"sourceRow":"waiting","frameCount":0},
                  "form-9":{"sourceRow":"waiting","frameCount":9},
                  "quebrada":{"row":3}},
                "interactions":{"click":{"animations":["form-1","form-x","form-0","form-9"],"mode":"cycle"}}}"#,
        );
        let layout = resolve_layout(&m, 1536, 1872).unwrap();
        assert_eq!(layout.animations.len(), 10);
        assert_eq!(layout.animations["running"].row, 7);
        // Mesma linha do waiting, com os 8 quadros; timing curto repete o último.
        assert_eq!(layout.animations["form-1"].row, 6);
        assert_eq!(
            layout.animations["form-1"].durations_ms,
            vec![190, 170, 170, 170, 170, 170, 170, 170]
        );
        assert_eq!(layout.click, vec!["form-1"]);

        let m = manifest(r#"{"interactions":null}"#);
        assert!(resolve_layout(&m, 1536, 1872).unwrap().click.is_empty());
    }

    #[test]
    fn explicit_fps_makes_codex_pacing_uniform() {
        let m = manifest(r#"{"fps":10}"#);
        let layout = resolve_layout(&m, 1536, 1872).unwrap();
        assert!(
            layout.animations["idle"]
                .durations_ms
                .iter()
                .all(|&d| d == 100)
        );
    }

    #[test]
    fn non_grid_image_is_a_single_static_frame() {
        let layout = resolve_layout(&Manifest::default(), 300, 300).unwrap();
        assert_eq!((layout.frame_w, layout.frame_h), (300, 300));
        assert_eq!(layout.animations["idle"].durations_ms.len(), 1);
    }

    #[test]
    fn explicit_layout_is_validated() {
        let ok = manifest(
            r#"{"frame":{"width":64,"height":64},"animations":{"idle":{"row":0,"frames":4}}}"#,
        );
        assert_eq!(
            resolve_layout(&ok, 256, 128).unwrap().animations["idle"].durations_ms,
            vec![125; 4]
        );
        assert!(
            resolve_layout(&ok, 250, 128).is_err(),
            "folha não múltipla do quadro"
        );

        let row = manifest(
            r#"{"frame":{"width":64,"height":64},"animations":{"idle":{"row":5,"frames":4}}}"#,
        );
        assert!(resolve_layout(&row, 256, 128).is_err());

        let durations = manifest(
            r#"{"frame":{"width":64,"height":64},"animations":{"idle":{"row":0,"frames":2,"frameDurationsMs":[100]}}}"#,
        );
        assert!(resolve_layout(&durations, 256, 128).is_err());

        let default = manifest(
            r#"{"frame":{"width":64,"height":64},"defaultAnimation":"x","animations":{"idle":{"row":0,"frames":1}}}"#,
        );
        assert!(resolve_layout(&default, 256, 128).is_err());
    }

    #[test]
    fn spritesheet_path_cannot_escape_the_pet_folder() {
        let dir = Path::new("C:/pets/goku");
        for bad in ["../x.webp", "/x.webp", "\\x.webp", "C:/x.webp", "a\0b"] {
            let m = Manifest {
                spritesheet_path: Some(bad.into()),
                ..Default::default()
            };
            assert!(sheet_path(dir, &m).is_err(), "{bad}");
        }
        let m = Manifest {
            spritesheet_path: Some("sheets/a.png".into()),
            ..Default::default()
        };
        assert_eq!(sheet_path(dir, &m).unwrap(), dir.join("sheets/a.png"));
    }

    #[test]
    fn real_codex_pets_load_if_present() {
        for pet in discover(&crate::config::default_pets_dir()) {
            let m = read_manifest(&pet.dir).unwrap();
            assert!(sheet_path(&pet.dir, &m).unwrap().is_file(), "{}", pet.id);
        }
    }
}
