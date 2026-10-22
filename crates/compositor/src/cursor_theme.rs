//! The cursor theme: a standard XCursor theme, `Adwaita` by default, the same one GNOME and
//! Ubuntu use. The theme is named by `XCURSOR_THEME` and drawn at `XCURSOR_SIZE` (24 by
//! default) times the output scale. It is looked up through the usual XCursor search path
//! (`XCURSOR_PATH`, otherwise the XDG icon folders, `~/.icons` and `/usr/share/icons`),
//! following `Inherits` in `index.theme`. Animated cursors (wait, progress) keep the frame
//! delays of the file. Without the theme the built-in arrow of `cursor.rs` stays.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use smithay::input::pointer::CursorIcon;
use tracing::{info, warn};

/// The theme used when `XCURSOR_THEME` is not set.
pub const DEFAULT_THEME: &str = "Adwaita";
/// The nominal size, in logical pixels, when `XCURSOR_SIZE` is not set.
pub const DEFAULT_SIZE: u32 = 24;

/// A cursor ready to upload: RGBA bytes (premultiplied, as stored in XCursor files), its size
/// and its hotspot (physical pixels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    pub width: u32,
    pub height: u32,
    pub hotspot: (i32, i32),
    pub rgba: Vec<u8>,
}

/// Identifies one frame of one cursor at one scale, for the texture cache of the backend.
pub type FrameKey = (String, u32, usize);

/// The frames of a cursor with the time each one stays on screen.
#[derive(Debug)]
struct Animation {
    /// The XCursor name that was resolved (aliases share it).
    name: String,
    frames: Vec<(Arc<CursorImage>, u64)>,
    /// The duration of the whole loop in milliseconds; 0 for a still cursor.
    total_ms: u64,
}

impl Animation {
    /// The frame to show `elapsed_ms` after the start of the loop.
    fn frame_at(&self, elapsed_ms: u64) -> Option<(usize, &Arc<CursorImage>)> {
        if self.total_ms == 0 {
            return self.frames.first().map(|(image, _)| (0, image));
        }
        let mut left = elapsed_ms % self.total_ms;
        for (index, (image, delay)) in self.frames.iter().enumerate() {
            if left < *delay {
                return Some((index, image));
            }
            left -= delay;
        }
        self.frames.first().map(|(image, _)| (0, image))
    }
}

/// The environment that decides which theme is loaded and where it is looked for.
#[derive(Debug, Clone, Default)]
pub struct ThemeEnv {
    pub theme: Option<String>,
    pub size: Option<String>,
    pub xcursor_path: Option<String>,
    pub home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_data_dirs: Option<String>,
}

impl ThemeEnv {
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            theme: var("XCURSOR_THEME"),
            size: var("XCURSOR_SIZE"),
            xcursor_path: var("XCURSOR_PATH"),
            home: var("HOME").map(PathBuf::from),
            xdg_data_home: var("XDG_DATA_HOME").map(PathBuf::from),
            xdg_data_dirs: var("XDG_DATA_DIRS"),
        }
    }

    fn theme_name(&self) -> &str {
        self.theme.as_deref().unwrap_or(DEFAULT_THEME)
    }

    fn nominal_size(&self) -> u32 {
        self.size
            .as_deref()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|s| *s > 0)
            .unwrap_or(DEFAULT_SIZE)
    }

    /// The folders that may hold theme folders, in order.
    fn search_paths(&self) -> Vec<PathBuf> {
        let expand = |entry: &str| -> Option<PathBuf> {
            match entry.strip_prefix('~') {
                Some(rest) => {
                    let home = self.home.as_ref()?;
                    Some(home.join(rest.trim_start_matches('/')))
                }
                None => Some(PathBuf::from(entry)),
            }
        };
        if let Some(path) = &self.xcursor_path {
            return path
                .split(':')
                .filter(|e| !e.is_empty())
                .filter_map(expand)
                .collect();
        }
        let mut paths = Vec::new();
        if let Some(data_home) = &self.xdg_data_home {
            paths.push(data_home.join("icons"));
        } else if let Some(home) = &self.home {
            paths.push(home.join(".local/share/icons"));
        }
        if let Some(home) = &self.home {
            paths.push(home.join(".icons"));
        }
        let data_dirs = self
            .xdg_data_dirs
            .as_deref()
            .unwrap_or("/usr/local/share:/usr/share");
        for dir in data_dirs.split(':').filter(|d| !d.is_empty()) {
            paths.push(Path::new(dir).join("icons"));
        }
        paths.push(PathBuf::from("/usr/share/pixmaps"));
        paths
    }
}

/// The `Inherits` list of a theme's `index.theme`.
fn inherits(index: &Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(index) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Inherits"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .flat_map(|list| {
            list.split([',', ';'])
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A theme and the themes it inherits, as the folders that hold their `cursors` directories,
/// in lookup order. The `default` theme closes the chain.
fn theme_chain(name: &str, search: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = vec![name.to_owned()];
    let mut next = 0;
    while let Some(current) = queue.get(next).cloned() {
        next += 1;
        if !seen.insert(current.clone()) {
            continue;
        }
        for base in search {
            let dir = base.join(&current);
            if !dir.is_dir() {
                continue;
            }
            queue.extend(inherits(&dir.join("index.theme")));
            dirs.push(dir);
        }
    }
    if !seen.contains("default") {
        for base in search {
            let dir = base.join("default");
            if dir.is_dir() {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// The XCursor names to try for a shape: the CSS name (what recent themes ship), then the
/// classic X11 names older themes use, and finally the plain arrow.
pub fn candidate_names(icon: CursorIcon) -> Vec<&'static str> {
    let mut names = vec![icon.name()];
    names.extend_from_slice(legacy_names(icon));
    for last in ["default", "left_ptr"] {
        if !names.contains(&last) {
            names.push(last);
        }
    }
    names
}

fn legacy_names(icon: CursorIcon) -> &'static [&'static str] {
    match icon {
        CursorIcon::Default | CursorIcon::ContextMenu => &["left_ptr"],
        CursorIcon::Help => &["question_arrow", "whats_this"],
        CursorIcon::Pointer => &["hand2", "hand1"],
        CursorIcon::Progress => &["left_ptr_watch", "half-busy"],
        CursorIcon::Wait => &["watch"],
        CursorIcon::Cell => &["plus"],
        CursorIcon::Crosshair => &["cross"],
        CursorIcon::Text => &["xterm", "ibeam"],
        CursorIcon::VerticalText => &["vertical-text"],
        CursorIcon::Alias => &["dnd-link"],
        CursorIcon::Copy => &["dnd-copy"],
        CursorIcon::Move => &["fleur", "dnd-move"],
        CursorIcon::NoDrop => &["dnd-none", "crossed_circle"],
        CursorIcon::NotAllowed => &["crossed_circle", "forbidden"],
        CursorIcon::Grab => &["openhand", "hand1"],
        CursorIcon::Grabbing => &["closedhand", "dnd-none"],
        CursorIcon::EResize => &["right_side"],
        CursorIcon::NResize => &["top_side"],
        CursorIcon::NeResize => &["top_right_corner"],
        CursorIcon::NwResize => &["top_left_corner"],
        CursorIcon::SResize => &["bottom_side"],
        CursorIcon::SeResize => &["bottom_right_corner"],
        CursorIcon::SwResize => &["bottom_left_corner"],
        CursorIcon::WResize => &["left_side"],
        CursorIcon::EwResize => &["sb_h_double_arrow", "h_double_arrow", "size_hor"],
        CursorIcon::NsResize => &["sb_v_double_arrow", "v_double_arrow", "size_ver"],
        CursorIcon::NeswResize => &["fd_double_arrow", "size_bdiag"],
        CursorIcon::NwseResize => &["bd_double_arrow", "size_fdiag"],
        CursorIcon::ColResize => &["sb_h_double_arrow", "split_h"],
        CursorIcon::RowResize => &["sb_v_double_arrow", "split_v"],
        CursorIcon::AllScroll => &["fleur"],
        _ => &[],
    }
}

/// The shapes of the theme, loaded from disk on demand.
#[derive(Debug)]
pub struct CursorTheme {
    /// The theme folders, in lookup order; empty when the theme is missing.
    dirs: Vec<PathBuf>,
    size: u32,
    /// The size of the environment, back when the user's choice is removed.
    env_size: u32,
    epoch: Instant,
    cache: HashMap<(&'static str, u32), Option<Rc<Animation>>>,
}

impl Default for CursorTheme {
    fn default() -> Self {
        Self::empty()
    }
}

impl CursorTheme {
    /// No theme: every lookup fails and the built-in arrow is drawn.
    pub fn empty() -> Self {
        Self {
            dirs: Vec::new(),
            size: DEFAULT_SIZE,
            env_size: DEFAULT_SIZE,
            epoch: Instant::now(),
            cache: HashMap::new(),
        }
    }

    /// The theme of the process environment (`XCURSOR_THEME`, `XCURSOR_SIZE`, ...).
    pub fn load() -> Self {
        Self::from_env(&ThemeEnv::from_process())
    }

    pub fn from_env(env: &ThemeEnv) -> Self {
        let name = env.theme_name();
        let dirs = theme_chain(name, &env.search_paths());
        if dirs.is_empty() {
            warn!("cursors: theme {name} not found, built-in arrow only");
        } else {
            info!("cursors: theme {name} at size {}", env.nominal_size());
        }
        Self {
            dirs,
            size: env.nominal_size(),
            env_size: env.nominal_size(),
            ..Self::empty()
        }
    }

    /// The user's pointer size (`cursorSize` of `theme.json`); `None`: the environment's.
    /// The shapes are loaded again at the new size.
    pub fn set_size(&mut self, size: Option<u32>) {
        let size = size.filter(|s| *s > 0).unwrap_or(self.env_size);
        if size != self.size {
            self.size = size;
            self.cache.clear();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    /// The frame of `icon` to show at `now` for an output at `scale`, with the key that
    /// identifies it. `None` when the theme has no usable cursor: the caller draws the arrow.
    pub fn image(
        &mut self,
        icon: CursorIcon,
        scale: f64,
        now: Instant,
    ) -> Option<(FrameKey, Arc<CursorImage>)> {
        if self.dirs.is_empty() {
            return None;
        }
        let scale_key = (scale * 100.0).round().max(1.0) as u32;
        let animation = match self.cache.get(&(icon.name(), scale_key)) {
            Some(animation) => animation.clone(),
            None => {
                let want = (f64::from(self.size) * scale).round().max(1.0) as u32;
                let animation = self.load_animation(icon, want).map(Rc::new);
                self.cache
                    .insert((icon.name(), scale_key), animation.clone());
                animation
            }
        }?;
        let elapsed =
            u64::try_from(now.saturating_duration_since(self.epoch).as_millis()).unwrap_or(0);
        let (index, image) = animation.frame_at(elapsed)?;
        // The size is part of the key: the frames already uploaded at another size stay apart.
        let key = scale_key.saturating_add(self.size.saturating_mul(1_000_000));
        Some(((animation.name.clone(), key, index), image.clone()))
    }

    /// Whether `icon` at `scale` is an animated cursor (it changes frame with time alone).
    /// Only cursors already shown (in the cache) count: a new one is loaded by the next frame.
    pub fn is_animated(&self, icon: CursorIcon, scale: f64) -> bool {
        let scale_key = (scale * 100.0).round().max(1.0) as u32;
        self.cache
            .get(&(icon.name(), scale_key))
            .and_then(Option::as_ref)
            .is_some_and(|animation| animation.total_ms > 0)
    }

    fn load_animation(&self, icon: CursorIcon, want: u32) -> Option<Animation> {
        for name in candidate_names(icon) {
            for dir in &self.dirs {
                let path = dir.join("cursors").join(name);
                if !path.is_file() {
                    continue;
                }
                if let Some(animation) = read_animation(&path, name, want) {
                    return Some(animation);
                }
            }
        }
        None
    }
}

/// The frames of the XCursor file at `path` whose nominal size is closest to `want`.
fn read_animation(path: &Path, name: &str, want: u32) -> Option<Animation> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(err) => {
            warn!("cursor {}: {err}", path.display());
            return None;
        }
    };
    let images = xcursor::parser::parse_xcursor(&data).or_else(|| {
        warn!("cursor {}: not an XCursor file", path.display());
        None
    })?;
    let size = images
        .iter()
        .map(|image| image.size)
        .min_by_key(|size| size.abs_diff(want))?;
    let chosen: Vec<_> = images.iter().filter(|image| image.size == size).collect();
    let animated = chosen.len() > 1;
    let frames: Vec<(Arc<CursorImage>, u64)> = chosen
        .iter()
        .map(|image| {
            let frame = CursorImage {
                width: image.width,
                height: image.height,
                hotspot: (
                    i32::try_from(image.xhot).unwrap_or(0),
                    i32::try_from(image.yhot).unwrap_or(0),
                ),
                rgba: image.pixels_rgba.clone(),
            };
            let delay = if animated {
                u64::from(image.delay).max(1)
            } else {
                0
            };
            (Arc::new(frame), delay)
        })
        .collect();
    let total_ms = frames.iter().map(|(_, delay)| delay).sum();
    Some(Animation {
        name: name.to_owned(),
        frames,
        total_ms,
    })
}

/// The cursor for the sides of a resize (MU-08): ↔ on the left and right edges, ↕ on the top
/// and bottom ones, the diagonals near the corners.
pub fn resize_icon(edges: crate::resize::Edges) -> CursorIcon {
    match (edges.left, edges.right, edges.top, edges.bottom) {
        (true, _, true, _) | (_, true, _, true) => CursorIcon::NwseResize,
        (true, _, _, true) | (_, true, true, _) => CursorIcon::NeswResize,
        (true, _, _, _) | (_, true, _, _) => CursorIcon::EwResize,
        _ => CursorIcon::NsResize,
    }
}

/// Like [`resize_icon`], one-way when an edge has reached a limit of the client: at the
/// minimum width the left edge shows ← only (it can only grow), at the maximum →, as macOS
/// does with its one-way resize cursors. WIN-RESIZE measured the two-way shapes (pairs of
/// separate arrowheads; Adwaita's `ew-resize`, `ns-resize`, `nwse-resize`, `nesw-resize`),
/// not the shapes at a limit: the one-way ones are provisional. The corners keep their
/// diagonals.
pub fn resize_icon_with_room(edges: crate::resize::Edges, room: crate::resize::Room) -> CursorIcon {
    let horizontal = (edges.left || edges.right) && !(edges.top || edges.bottom);
    let vertical = (edges.top || edges.bottom) && !(edges.left || edges.right);
    if horizontal && room.grow_w != room.shrink_w {
        // Growing moves the left edge leftwards and the right one rightwards.
        let leftwards = edges.left == room.grow_w;
        return if leftwards {
            CursorIcon::WResize
        } else {
            CursorIcon::EResize
        };
    }
    if vertical && room.grow_h != room.shrink_h {
        let upwards = edges.top == room.grow_h;
        return if upwards {
            CursorIcon::NResize
        } else {
            CursorIcon::SResize
        };
    }
    resize_icon(edges)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resize::Edges;
    use std::time::Duration;

    /// One chunk of a test XCursor file: nominal size, width, height, hotspot (x and y),
    /// delay in ms and the ARGB value of every pixel.
    type Chunk = (u32, u32, u32, u32, u32, u32);

    /// A minimal XCursor file with the given image chunks.
    fn xcursor_file(chunks: &[Chunk]) -> Vec<u8> {
        let mut out = Vec::new();
        let word = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
        out.extend_from_slice(b"Xcur");
        word(&mut out, 16);
        word(&mut out, 0x1_0000);
        word(&mut out, chunks.len() as u32);
        let mut position = 16 + 12 * chunks.len() as u32;
        for (size, w, h, ..) in chunks {
            word(&mut out, 0xfffd_0002);
            word(&mut out, *size);
            word(&mut out, position);
            position += 36 + w * h * 4;
        }
        for (size, w, h, hot, delay, argb) in chunks {
            for v in [36, 0xfffd_0002, *size, 1, *w, *h, *hot, *hot, *delay] {
                word(&mut out, v);
            }
            for _ in 0..w * h {
                word(&mut out, *argb);
            }
        }
        out
    }

    fn env_for(dir: &Path, theme: &str) -> ThemeEnv {
        ThemeEnv {
            theme: Some(theme.to_owned()),
            xcursor_path: Some(dir.display().to_string()),
            ..ThemeEnv::default()
        }
    }

    fn write_cursor(dir: &Path, theme: &str, name: &str, bytes: &[u8]) {
        let cursors = dir.join(theme).join("cursors");
        assert!(std::fs::create_dir_all(&cursors).is_ok());
        assert!(std::fs::write(cursors.join(name), bytes).is_ok());
    }

    #[test]
    fn resize_edges_map_to_the_measured_shapes() {
        let e = |left, right, top, bottom| Edges {
            left,
            right,
            top,
            bottom,
        };
        assert_eq!(
            resize_icon(e(true, false, false, false)),
            CursorIcon::EwResize
        );
        assert_eq!(
            resize_icon(e(false, false, false, true)),
            CursorIcon::NsResize
        );
        assert_eq!(
            resize_icon(e(true, false, true, false)),
            CursorIcon::NwseResize
        );
        assert_eq!(
            resize_icon(e(false, true, false, true)),
            CursorIcon::NwseResize
        );
        assert_eq!(
            resize_icon(e(false, true, true, false)),
            CursorIcon::NeswResize
        );
        assert_eq!(
            resize_icon(e(true, false, false, true)),
            CursorIcon::NeswResize
        );
    }

    #[test]
    fn at_a_limit_the_edge_cursor_is_one_way() {
        use crate::resize::Room;
        let e = |left, right, top, bottom| Edges {
            left,
            right,
            top,
            bottom,
        };
        let free = Room {
            grow_w: true,
            shrink_w: true,
            grow_h: true,
            shrink_h: true,
        };
        let at_min = Room {
            shrink_w: false,
            shrink_h: false,
            ..free
        };
        let at_max = Room {
            grow_w: false,
            grow_h: false,
            ..free
        };
        let left = e(true, false, false, false);
        let right = e(false, true, false, false);
        let top = e(false, false, true, false);
        let bottom = e(false, false, false, true);
        assert_eq!(resize_icon_with_room(left, free), CursorIcon::EwResize);
        assert_eq!(resize_icon_with_room(left, at_min), CursorIcon::WResize);
        assert_eq!(resize_icon_with_room(left, at_max), CursorIcon::EResize);
        assert_eq!(resize_icon_with_room(right, at_min), CursorIcon::EResize);
        assert_eq!(resize_icon_with_room(right, at_max), CursorIcon::WResize);
        assert_eq!(resize_icon_with_room(top, at_min), CursorIcon::NResize);
        assert_eq!(resize_icon_with_room(bottom, at_min), CursorIcon::SResize);
        assert_eq!(resize_icon_with_room(bottom, at_max), CursorIcon::NResize);
        // Corners keep the diagonal.
        let corner = e(true, false, true, false);
        assert_eq!(
            resize_icon_with_room(corner, at_min),
            CursorIcon::NwseResize
        );
    }

    /// Every resize shape resolves to a file of the Adwaita theme installed on this machine
    /// (skipped where it is not installed).
    #[test]
    fn the_resize_shapes_exist_in_adwaita() {
        let dirs = ["/usr/share/icons/Adwaita/cursors"];
        let Some(dir) = dirs
            .iter()
            .map(std::path::Path::new)
            .find(|dir| dir.is_dir())
        else {
            return;
        };
        for icon in [
            CursorIcon::EwResize,
            CursorIcon::NsResize,
            CursorIcon::NwseResize,
            CursorIcon::NeswResize,
            CursorIcon::EResize,
            CursorIcon::WResize,
            CursorIcon::NResize,
            CursorIcon::SResize,
        ] {
            let found = candidate_names(icon)
                .first()
                .is_some_and(|name| dir.join(name).exists());
            assert!(
                found,
                "{icon:?}: {:?} not in Adwaita",
                candidate_names(icon)
            );
        }
    }

    #[test]
    fn shapes_map_to_the_names_adwaita_ships() {
        let first = |icon| candidate_names(icon).first().copied();
        assert_eq!(first(CursorIcon::Default), Some("default"));
        assert_eq!(first(CursorIcon::Text), Some("text"));
        assert_eq!(first(CursorIcon::Pointer), Some("pointer"));
        assert_eq!(first(CursorIcon::Grab), Some("grab"));
        assert_eq!(first(CursorIcon::Grabbing), Some("grabbing"));
        assert_eq!(first(CursorIcon::Wait), Some("wait"));
        assert_eq!(first(CursorIcon::Progress), Some("progress"));
        assert_eq!(first(CursorIcon::NotAllowed), Some("not-allowed"));
        assert_eq!(first(CursorIcon::Crosshair), Some("crosshair"));
        assert_eq!(first(CursorIcon::EwResize), Some("ew-resize"));
        assert_eq!(first(CursorIcon::NsResize), Some("ns-resize"));
        assert_eq!(first(CursorIcon::NwseResize), Some("nwse-resize"));
        assert_eq!(first(CursorIcon::NeswResize), Some("nesw-resize"));
        // Classic names follow, and every list ends with the plain arrow.
        assert!(candidate_names(CursorIcon::Text).contains(&"xterm"));
        assert!(candidate_names(CursorIcon::Pointer).contains(&"hand2"));
        assert_eq!(candidate_names(CursorIcon::Wait).last(), Some(&"left_ptr"));
    }

    #[test]
    fn a_missing_theme_falls_back_to_the_arrow() {
        let Ok(tmp) = tempfile::tempdir() else {
            return;
        };
        let now = Instant::now();
        let mut theme = CursorTheme::from_env(&env_for(tmp.path(), "Nope"));
        assert!(theme.is_empty());
        assert!(theme.image(CursorIcon::Default, 1.0, now).is_none());
        let mut empty = CursorTheme::empty();
        assert!(empty.image(CursorIcon::Text, 2.0, now).is_none());
        let (w, h, pixels) = crate::cursor::arrow();
        assert!(w > 0 && h > 0 && !pixels.is_empty());
    }

    #[test]
    fn the_size_comes_from_xcursor_size() {
        let mut env = ThemeEnv::default();
        assert_eq!(env.nominal_size(), DEFAULT_SIZE);
        assert_eq!(env.theme_name(), "Adwaita");
        env.size = Some("32".to_owned());
        assert_eq!(env.nominal_size(), 32);
        env.size = Some("junk".to_owned());
        assert_eq!(env.nominal_size(), DEFAULT_SIZE);
    }

    #[test]
    fn a_theme_loads_from_xcursor_path_with_inheritance_and_animation() {
        let Ok(tmp) = tempfile::tempdir() else {
            return;
        };
        let dir = tmp.path();
        // Two sizes of a still arrow, and a two-frame animation, in the theme "Test".
        write_cursor(
            dir,
            "Test",
            "default",
            &xcursor_file(&[(24, 2, 2, 1, 0, 0xff10_2030), (48, 4, 4, 2, 0, 0xff40_5060)]),
        );
        write_cursor(
            dir,
            "Test",
            "wait",
            &xcursor_file(&[
                (24, 2, 2, 0, 100, 0xff00_0001),
                (24, 2, 2, 0, 50, 0xff00_0002),
            ]),
        );
        // "text" only exists in the parent theme.
        write_cursor(
            dir,
            "Parent",
            "text",
            &xcursor_file(&[(24, 3, 3, 1, 0, 0xffff_ffff)]),
        );
        assert!(
            std::fs::write(
                dir.join("Test/index.theme"),
                "[Icon Theme]\nName=Test\nInherits=Parent\n"
            )
            .is_ok()
        );

        let mut theme = CursorTheme::from_env(&env_for(dir, "Test"));
        assert!(!theme.is_empty());
        let t0 = theme.epoch;

        let Some((key, image)) = theme.image(CursorIcon::Default, 1.0, t0) else {
            panic!("default");
        };
        assert_eq!(key.0, "default");
        assert_eq!((image.width, image.height, image.hotspot), (2, 2, (1, 1)));
        assert_eq!(image.rgba.len(), 2 * 2 * 4);
        // At 2x the 48 px image is the closest.
        let Some((_, big)) = theme.image(CursorIcon::Default, 2.0, t0) else {
            panic!("default 2x");
        };
        assert_eq!((big.width, big.hotspot), (4, (2, 2)));

        // Inherited from the parent theme.
        assert!(theme.image(CursorIcon::Text, 1.0, t0).is_some());

        // Wait is animated by the delays of the file: 100 ms, then 50 ms, then it loops.
        let at = |ms| t0 + Duration::from_millis(ms);
        let frame = |theme: &mut CursorTheme, ms| {
            theme
                .image(CursorIcon::Wait, 1.0, at(ms))
                .map(|(key, _)| key.2)
        };
        assert_eq!(frame(&mut theme, 0), Some(0));
        assert_eq!(frame(&mut theme, 99), Some(0));
        assert_eq!(frame(&mut theme, 120), Some(1));
        assert_eq!(frame(&mut theme, 160), Some(0));

        // A shape the theme lacks resolves to its default arrow.
        let Some((key, _)) = theme.image(CursorIcon::Crosshair, 1.0, t0) else {
            panic!("crosshair");
        };
        assert_eq!(key.0, "default");
    }
}
