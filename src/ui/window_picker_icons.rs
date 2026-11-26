//! Lightweight XDG application icon resolver & Cairo surface cache for the Window Picker.
//!
//! Resolves `.desktop` `Icon=` entries and PNG theme icons without adding external crates
//! (reuses `png` and `pangocairo`). Falls back gracefully to a crisp frosted colored monogram
//! badge when an icon is missing or SVG-only.

use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use pangocairo::cairo::{self, ImageSurface};

/// Cached desktop entry index and decoded PNG icon surfaces.
#[derive(Default)]
pub struct AppIconCache {
    /// Lazily populated mapping of lowercase app_id / desktop id / StartupWMClass -> Icon name.
    desktop_icons: Option<HashMap<String, String>>,
    /// Cached icon file lookup by icon name (None = not found as PNG).
    icon_paths: HashMap<String, Option<PathBuf>>,
    /// Cached scaled ARGB32 Cairo surfaces keyed by `(app_id_lower, size_px)`.
    surfaces: HashMap<(String, i32), Option<ImageSurface>>,
}

impl AppIconCache {
    pub fn clear(&mut self) {
        self.surfaces.clear();
    }

    /// Returns a premultiplied ARGB32 Cairo surface of size `size_px × size_px` for `app_id`,
    /// or `None` if no PNG icon could be resolved (caller draws a frosted monogram fallback).
    pub fn get_icon_surface(&mut self, app_id: &str, size_px: i32) -> Option<ImageSurface> {
        let size_px = size_px.clamp(12, 128);
        let key_name = app_id.trim().to_ascii_lowercase();
        if key_name.is_empty() {
            return None;
        }

        let cache_key = (key_name.clone(), size_px);
        if let Some(cached) = self.surfaces.get(&cache_key) {
            return cached.clone();
        }

        let icon_name = self.resolve_icon_name(&key_name);
        let path = self.resolve_icon_path(&icon_name);
        let surface = path
            .as_deref()
            .and_then(|p| load_png_as_cairo_surface(p, size_px).ok());

        self.surfaces.insert(cache_key, surface.clone());
        surface
    }

    fn ensure_desktop_index(&mut self) {
        if self.desktop_icons.is_some() {
            return;
        }
        let mut map = HashMap::new();
        for dir in desktop_entry_dirs() {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                    continue;
                }
                let Ok(content) = fs::read_to_string(&path) else {
                    continue;
                };
                let Some((icon, wm_class)) = parse_desktop_entry(&content) else {
                    continue;
                };
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let stem_lower = stem.to_ascii_lowercase();
                    map.entry(stem_lower.clone())
                        .or_insert_with(|| icon.clone());
                    // Also index the last dot-segment for reverse-DNS app IDs (e.g.
                    // org.gnome.Nautilus -> nautilus).
                    if let Some(short) = stem_lower.rsplit('.').next() {
                        if short != stem_lower {
                            map.entry(short.to_owned()).or_insert_with(|| icon.clone());
                        }
                    }
                }
                if let Some(wm) = wm_class {
                    map.entry(wm.to_ascii_lowercase()).or_insert(icon);
                }
            }
        }
        self.desktop_icons = Some(map);
    }

    fn resolve_icon_name(&mut self, app_id_lower: &str) -> String {
        self.ensure_desktop_index();
        if let Some(map) = &self.desktop_icons {
            if let Some(icon) = map.get(app_id_lower) {
                return icon.clone();
            }
            if let Some(short) = app_id_lower.rsplit('.').next() {
                if let Some(icon) = map.get(short) {
                    return icon.clone();
                }
            }
        }
        app_id_lower.to_owned()
    }

    fn resolve_icon_path(&mut self, icon_name: &str) -> Option<PathBuf> {
        if let Some(cached) = self.icon_paths.get(icon_name) {
            return cached.clone();
        }

        let found = find_png_icon(icon_name);
        self.icon_paths.insert(icon_name.to_owned(), found.clone());
        found
    }
}

fn xdg_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home_data) = env::var("XDG_DATA_HOME") {
        if !home_data.is_empty() {
            dirs.push(PathBuf::from(home_data));
        }
    } else if let Ok(home) = env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share"));
    }

    if let Ok(data_dirs) = env::var("XDG_DATA_DIRS") {
        for dir in data_dirs.split(':').filter(|s| !s.is_empty()) {
            dirs.push(PathBuf::from(dir));
        }
    } else {
        dirs.push(PathBuf::from("/usr/local/share"));
        dirs.push(PathBuf::from("/usr/share"));
    }

    // NixOS profile & system share paths.
    for extra in ["/run/current-system/sw/share", "/etc/profiles/per-user"] {
        let p = PathBuf::from(extra);
        if p.exists() && !dirs.contains(&p) {
            dirs.push(p);
        }
    }

    dirs
}

fn desktop_entry_dirs() -> Vec<PathBuf> {
    xdg_data_dirs()
        .into_iter()
        .map(|d| d.join("applications"))
        .filter(|d| d.is_dir())
        .collect()
}

fn parse_desktop_entry(content: &str) -> Option<(String, Option<String>)> {
    let mut in_entry = false;
    let mut icon = None;
    let mut wm_class = None;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            if line == "[Desktop Entry]" {
                in_entry = true;
                continue;
            } else if in_entry {
                break;
            }
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            if k == "Icon" && !v.is_empty() {
                icon = Some(v.to_owned());
            } else if k == "StartupWMClass" && !v.is_empty() {
                wm_class = Some(v.to_owned());
            }
        }
    }

    icon.map(|i| (i, wm_class))
}

fn find_png_icon(icon_name: &str) -> Option<PathBuf> {
    let direct = Path::new(icon_name);
    if direct.is_absolute() && direct.is_file() {
        if direct
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        {
            return Some(direct.to_path_buf());
        }
        return None;
    }

    let png_name = if icon_name.ends_with(".png") {
        icon_name.to_owned()
    } else {
        format!("{icon_name}.png")
    };

    let themes = [
        "Papirus",
        "Papirus-Dark",
        "Adwaita",
        "breeze",
        "breeze-dark",
        "hicolor",
    ];
    let sizes = ["48x48", "64x64", "32x32", "128x128", "256x256", "96x96"];
    let categories = ["apps", "places", "categories", "mimetypes", "devices"];

    for data_dir in xdg_data_dirs() {
        let icons_dir = data_dir.join("icons");
        if icons_dir.is_dir() {
            for theme in themes {
                let theme_dir = icons_dir.join(theme);
                if !theme_dir.is_dir() {
                    continue;
                }
                for size in sizes {
                    for cat in categories {
                        let candidate = theme_dir.join(size).join(cat).join(&png_name);
                        if candidate.is_file() {
                            return Some(candidate);
                        }
                    }
                }
            }
        }

        let pixmap = data_dir.join("pixmaps").join(&png_name);
        if pixmap.is_file() {
            return Some(pixmap);
        }
    }

    None
}

fn load_png_as_cairo_surface(path: &Path, target_px: i32) -> anyhow::Result<ImageSurface> {
    let file = File::open(path)?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut buf)?;
    let w = info.width as i32;
    let h = info.height as i32;
    anyhow::ensure!(w > 0 && h > 0 && w <= 2048 && h <= 2048);

    // Convert decoded PNG pixels to Cairo's native-endian premultiplied ARGB32 (BGRA on LE).
    let mut src_surface = ImageSurface::create(cairo::Format::ARgb32, w, h)?;
    {
        let stride = src_surface.stride() as usize;
        let mut data = src_surface.data()?;
        let raw = &buf[..info.buffer_size()];
        let channels = match info.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Grayscale => 1,
            _ => anyhow::bail!("unsupported png color type"),
        };

        for y in 0..h as usize {
            let dst_row = &mut data[y * stride..y * stride + (w as usize) * 4];
            let src_row = &raw[y * (w as usize) * channels..(y + 1) * (w as usize) * channels];
            for x in 0..w as usize {
                let (r, g, b, a) = match channels {
                    4 => (
                        src_row[x * 4],
                        src_row[x * 4 + 1],
                        src_row[x * 4 + 2],
                        src_row[x * 4 + 3],
                    ),
                    3 => (src_row[x * 3], src_row[x * 3 + 1], src_row[x * 3 + 2], 255),
                    2 => {
                        let v = src_row[x * 2];
                        (v, v, v, src_row[x * 2 + 1])
                    }
                    _ => {
                        let v = src_row[x];
                        (v, v, v, 255)
                    }
                };
                let a_u16 = u16::from(a);
                let pr = ((u16::from(r) * a_u16 + 127) / 255) as u8;
                let pg = ((u16::from(g) * a_u16 + 127) / 255) as u8;
                let pb = ((u16::from(b) * a_u16 + 127) / 255) as u8;
                let pixel = u32::from_le_bytes([pb, pg, pr, a]);
                dst_row[x * 4..x * 4 + 4].copy_from_slice(&pixel.to_ne_bytes());
            }
        }
    }

    if w == target_px && h == target_px {
        return Ok(src_surface);
    }

    let scaled = ImageSurface::create(cairo::Format::ARgb32, target_px, target_px)?;
    let cr = cairo::Context::new(&scaled)?;
    cr.scale(
        f64::from(target_px) / f64::from(w),
        f64::from(target_px) / f64::from(h),
    );
    cr.set_source_surface(&src_surface, 0., 0.)?;
    cr.source().set_filter(cairo::Filter::Best);
    cr.paint()?;
    drop(cr);
    Ok(scaled)
}

/// Deterministic pleasant accent color for monogram fallback icons, derived from `app_id`.
pub fn monogram_color(app_id: &str) -> (f64, f64, f64) {
    let mut hash: u32 = 2166136261;
    for b in app_id.as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(16777619);
    }
    let hue = (hash % 360) as f64;
    hsl_to_rgb(hue, 0.56, 0.52)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    let c = (1. - (2. * l - 1.).abs()) * s;
    let x = c * (1. - ((h / 60.) % 2. - 1.).abs());
    let m = l - c / 2.;
    let (r, g, b) = match h as u32 {
        0..=59 => (c, x, 0.),
        60..=119 => (x, c, 0.),
        120..=179 => (0., c, x),
        180..=239 => (0., x, c),
        240..=299 => (x, 0., c),
        _ => (c, 0., x),
    };
    (r + m, g + m, b + m)
}
