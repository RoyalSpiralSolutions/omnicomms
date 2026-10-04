#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{collections::HashMap, fs, io::Read, path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use tauri::{
    menu::{Menu, MenuItemBuilder, SubmenuBuilder},
    window::WindowBuilder,
    AppHandle, Emitter, EventTarget, Manager, PhysicalPosition, PhysicalSize, Rect, State, Webview,
    WebviewBuilder, WebviewUrl, Window, WindowEvent,
};
#[cfg(target_os = "macos")]
use tauri::TitleBarStyle;
use uuid::Uuid;

// Tall enough to hold the traffic lights, which the overlay title bar style
// draws on top of this strip rather than above it.
const TAB_BAR_HEIGHT: f64 = 38.0;
// WKWebView's default UA string trips WhatsApp/Discord/etc.'s browser-version
// gates ("please update Safari") even on a current WebKit. Reporting as a
// recent desktop Safari avoids that wall; this is the standard workaround.
const DESKTOP_SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3 Safari/605.1.15";
const TABBAR_LABEL: &str = "tabbar";
const MAIN_WINDOW_LABEL: &str = "main";

#[derive(Clone, Serialize, Deserialize)]
struct TabRecord {
    id: String,
    name: String,
    url: String,
    data_store_id: Uuid,
    /// File name inside the icons directory. `None` until one is downloaded or
    /// chosen; `default` keeps older configs loadable.
    #[serde(default)]
    icon: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct PersistedState {
    tabs: Vec<TabRecord>,
    active_id: Option<String>,
}

#[derive(Serialize, Clone)]
struct TabDto {
    id: String,
    name: String,
    url: String,
}

#[derive(Serialize, Clone)]
struct StateDto {
    tabs: Vec<TabDto>,
    #[serde(rename = "activeId")]
    active_id: Option<String>,
}

struct AppState {
    tabs: Vec<TabRecord>,
    active_id: Option<String>,
    webviews: HashMap<String, Webview>,
    config_path: PathBuf,
    icons_dir: PathBuf,
}

impl AppState {
    fn to_dto(&self) -> StateDto {
        StateDto {
            tabs: self
                .tabs
                .iter()
                .map(|t| TabDto {
                    id: t.id.clone(),
                    name: t.name.clone(),
                    url: t.url.clone(),
                })
                .collect(),
            active_id: self.active_id.clone(),
        }
    }

    fn save(&self) {
        let persisted = PersistedState {
            tabs: self.tabs.clone(),
            active_id: self.active_id.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&persisted) {
            if let Some(dir) = self.config_path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::write(&self.config_path, json);
        }
    }
}

fn default_tabs() -> Vec<TabRecord> {
    [
        ("WhatsApp", "https://web.whatsapp.com"),
        ("Discord", "https://discord.com/app"),
        ("Telegram", "https://web.telegram.org"),
        ("Messages", "https://messages.google.com/web"),
        ("Instagram", "https://www.instagram.com/direct/inbox/"),
    ]
    .into_iter()
    .map(|(name, url)| TabRecord {
        id: Uuid::new_v4().to_string(),
        name: name.to_string(),
        url: url.to_string(),
        data_store_id: Uuid::new_v4(),
        icon: None,
    })
    .collect()
}

fn load_or_seed(config_path: &PathBuf) -> PersistedState {
    if let Ok(raw) = fs::read_to_string(config_path) {
        if let Ok(parsed) = serde_json::from_str::<PersistedState>(&raw) {
            if !parsed.tabs.is_empty() {
                return parsed;
            }
        }
    }
    let tabs = default_tabs();
    let active_id = tabs.first().map(|t| t.id.clone());
    let seeded = PersistedState { tabs, active_id };
    if let Ok(json) = serde_json::to_string_pretty(&seeded) {
        if let Some(dir) = config_path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(config_path, json);
    }
    seeded
}

// The unstable multiwebview add_child/set_bounds API doesn't reliably honor
// the Logical/Physical distinction on macOS — values round-trip as if they
// were already physical pixels, which on a Retina display silently halves
// any Logical offset you pass in. Working entirely in physical pixels here
// sidesteps that ambiguity.
fn bar_height_physical(window: &Window) -> u32 {
    let scale = window.scale_factor().unwrap_or(1.0);
    (TAB_BAR_HEIGHT * scale).round() as u32
}

/// Bounds for the content area below the fixed-height tab strip, in physical pixels.
fn content_bounds(window: &Window) -> Rect {
    let physical = window
        .inner_size()
        .unwrap_or(tauri::PhysicalSize::new(1200, 800));
    let bar_h = bar_height_physical(window);
    Rect {
        position: PhysicalPosition::new(0i32, bar_h as i32).into(),
        size: PhysicalSize::new(physical.width, physical.height.saturating_sub(bar_h)).into(),
    }
}

fn tabbar_bounds(window: &Window) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let physical = window
        .inner_size()
        .unwrap_or(tauri::PhysicalSize::new(1200, 800));
    (
        PhysicalPosition::new(0, 0),
        PhysicalSize::new(physical.width, bar_height_physical(window)),
    )
}

fn webview_label(tab_id: &str) -> String {
    format!("tab-{tab_id}")
}

// Favicons are fetched here rather than in the tab strip's page because several
// services (WhatsApp, Instagram, Discord) refuse image requests originating from
// the webview's `tauri://` origin even though the same URL serves fine over
// plain HTTP. Requests go only to the service the user already configured.
const ICON_MAX_BYTES: usize = 512 * 1024;
// Login pages can be large (Instagram's is ~640KB) and the icon link lives in
// the <head>, so the page budget has to be well above the icon budget.
const PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;

fn http_get(url: &str, max_bytes: usize) -> Result<(String, Vec<u8>), String> {
    let resp = ureq::builder()
        .timeout(std::time::Duration::from_secs(8))
        .redirects(5)
        .build()
        .get(url)
        .set("User-Agent", DESKTOP_SAFARI_UA)
        .call()
        .map_err(|e| e.to_string())?;

    let content_type = resp
        .header("content-type")
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .trim()
        .to_string();

    let mut body = Vec::new();
    resp.into_reader()
        .take((max_bytes + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() > max_bytes {
        return Err("response too large".into());
    }
    Ok((content_type, body))
}

/// Pulls `href`s out of any `<link rel="...icon...">` tags, best first.
fn icon_hrefs_from_html(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = html.to_lowercase();
    let mut cursor = 0;

    while let Some(start) = lower[cursor..].find("<link").map(|i| i + cursor) {
        let Some(len) = lower[start..].find('>') else { break };
        let tag = &html[start..start + len];
        let tag_lower = &lower[start..start + len];
        cursor = start + len;

        if !tag_lower.contains("rel=") || !tag_lower.contains("icon") {
            continue;
        }
        // Skip mask-icons; they're monochrome silhouettes, not real favicons.
        if tag_lower.contains("mask-icon") {
            continue;
        }
        if let Some(href) = attr_value(tag, "href") {
            out.push(href);
        }
    }
    out
}

fn attr_value(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_lowercase();
    let at = lower.find(&format!("{attr}="))? + attr.len() + 1;
    let rest = &tag[at..];
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let end = rest[1..].find(quote)? + 1;
        Some(rest[1..end].to_string())
    } else {
        let end = rest.find([' ', '>', '\t', '\n']).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

fn ext_for(content_type: &str) -> &'static str {
    match content_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "ico",
    }
}

fn content_type_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "image/x-icon",
    }
}

/// Downloads the site's favicon and returns its bytes plus the chosen extension.
fn download_icon(page_url: &str) -> Result<(Vec<u8>, &'static str), String> {
    let base = tauri::Url::parse(page_url).map_err(|e| e.to_string())?;

    let mut candidates: Vec<String> = Vec::new();
    // Prefer whatever the page itself declares, then the conventional paths.
    if let Ok((_, html)) = http_get(page_url, PAGE_MAX_BYTES) {
        let html = String::from_utf8_lossy(&html);
        for href in icon_hrefs_from_html(&html) {
            if let Ok(abs) = base.join(&href) {
                candidates.push(abs.to_string());
            }
        }
    }
    for path in ["/favicon.ico", "/favicon.png", "/apple-touch-icon.png"] {
        if let Ok(abs) = base.join(path) {
            candidates.push(abs.to_string());
        }
    }

    for candidate in candidates {
        let Ok((content_type, bytes)) = http_get(&candidate, ICON_MAX_BYTES) else { continue };
        if bytes.is_empty() || !content_type.starts_with("image/") {
            continue;
        }
        return Ok((bytes, ext_for(&content_type)));
    }
    Err("no icon found".into())
}

fn data_uri_from_file(path: &PathBuf) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("ico")
        .to_lowercase();
    Ok(format!(
        "data:{};base64,{}",
        content_type_for(&ext),
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
    ))
}

/// Writes icon bytes into the icons directory, replacing any previous file for
/// this tab, and records the new file name.
fn store_icon(
    state: &State<'_, Mutex<AppState>>,
    id: &str,
    bytes: &[u8],
    ext: &str,
) -> Result<String, String> {
    let mut guard = state.lock().unwrap();
    let icons_dir = guard.icons_dir.clone();
    fs::create_dir_all(&icons_dir).map_err(|e| e.to_string())?;

    let tab = guard
        .tabs
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| "tab not found".to_string())?;
    let previous = tab.icon.clone();

    let file_name = format!("{id}.{ext}");
    let path = icons_dir.join(&file_name);
    fs::write(&path, bytes).map_err(|e| e.to_string())?;

    if let Some(prev) = previous {
        if prev != file_name {
            let _ = fs::remove_file(icons_dir.join(prev));
        }
    }

    let tab = guard.tabs.iter_mut().find(|t| t.id == id).unwrap();
    tab.icon = Some(file_name);
    guard.save();

    data_uri_from_file(&path)
}

/// Returns the cached icon for a tab, or `None` if nothing is stored yet.
#[tauri::command]
fn tab_icon(id: String, state: State<'_, Mutex<AppState>>) -> Option<String> {
    let guard = state.lock().unwrap();
    let tab = guard.tabs.iter().find(|t| t.id == id)?;
    let path = guard.icons_dir.join(tab.icon.as_ref()?);
    data_uri_from_file(&path).ok()
}

/// Downloads the site's icon into the icons directory and returns it.
#[tauri::command]
async fn refresh_tab_icon(
    id: String,
    app: AppHandle,
) -> Result<String, String> {
    let url = {
        let state = app.state::<Mutex<AppState>>();
        let guard = state.lock().unwrap();
        guard
            .tabs
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.url.clone())
            .ok_or_else(|| "tab not found".to_string())?
    };

    let (bytes, ext) = tauri::async_runtime::spawn_blocking(move || download_icon(&url))
        .await
        .map_err(|e| e.to_string())??;

    let state = app.state::<Mutex<AppState>>();
    store_icon(&state, &id, &bytes, ext)
}

/// Replaces a tab's icon with an image the user picked, passed in as a data URI.
#[tauri::command]
fn set_tab_icon(
    id: String,
    data_uri: String,
    state: State<'_, Mutex<AppState>>,
) -> Result<String, String> {
    let (header, payload) = data_uri
        .split_once(",")
        .ok_or_else(|| "not a data URI".to_string())?;
    if !header.starts_with("data:image/") {
        return Err("file is not an image".into());
    }
    let content_type = header
        .trim_start_matches("data:")
        .split(';')
        .next()
        .unwrap_or("image/png");

    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, payload)
            .map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Err("empty image".into());
    }
    if bytes.len() > ICON_MAX_BYTES {
        return Err("image too large".into());
    }

    store_icon(&state, &id, &bytes, ext_for(content_type))
}

fn spawn_tab_webview(window: &Window, record: &TabRecord) -> Result<Webview, String> {
    let parsed = tauri::Url::parse(&record.url).map_err(|e| e.to_string())?;
    let bounds = content_bounds(window);
    window
        .add_child(
            WebviewBuilder::new(webview_label(&record.id), WebviewUrl::External(parsed))
                .data_store_identifier(*record.data_store_id.as_bytes())
                .user_agent(DESKTOP_SAFARI_UA),
            bounds.position,
            bounds.size,
        )
        .map_err(|e| e.to_string())
}

/// Shows `id`'s webview (creating it on first visit) and hides whatever was active before.
fn activate_tab(window: &Window, state: &State<'_, Mutex<AppState>>, id: &str) -> Result<(), String> {
    let mut guard = state.lock().unwrap();

    if let Some(prev) = guard.active_id.clone() {
        if prev != id {
            if let Some(wv) = guard.webviews.get(&prev) {
                let _ = wv.hide();
            }
        }
    }

    if !guard.webviews.contains_key(id) {
        let record = guard
            .tabs
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or_else(|| "tab not found".to_string())?;
        drop(guard);
        let webview = spawn_tab_webview(window, &record)?;
        guard = state.lock().unwrap();
        guard.webviews.insert(id.to_string(), webview);
    }

    if let Some(wv) = guard.webviews.get(id) {
        let bounds = content_bounds(window);
        let _ = wv.set_bounds(bounds);
        let _ = wv.show();
        let _ = wv.set_focus();
    }

    guard.active_id = Some(id.to_string());
    guard.save();
    Ok(())
}

fn relayout(window: &Window, state: &State<'_, Mutex<AppState>>) {
    let (pos, size) = tabbar_bounds(window);
    if let Some(bar) = window.get_webview(TABBAR_LABEL) {
        let _ = bar.set_bounds(Rect {
            position: pos.into(),
            size: size.into(),
        });
    }
    let guard = state.lock().unwrap();
    if let Some(active) = &guard.active_id {
        if let Some(wv) = guard.webviews.get(active) {
            let _ = wv.set_bounds(content_bounds(window));
        }
    }
}

#[tauri::command]
fn get_state(state: State<'_, Mutex<AppState>>) -> StateDto {
    state.lock().unwrap().to_dto()
}

#[tauri::command]
fn add_tab(
    name: String,
    url: String,
    window: Window,
    state: State<'_, Mutex<AppState>>,
) -> Result<StateDto, String> {
    let name = name.trim().to_string();
    let raw_url = url.trim().to_string();
    if name.is_empty() || raw_url.is_empty() {
        return Err("name and url are required".to_string());
    }
    let normalized_url = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
        raw_url
    } else {
        format!("https://{raw_url}")
    };

    let record = TabRecord {
        id: Uuid::new_v4().to_string(),
        name,
        url: normalized_url,
        data_store_id: Uuid::new_v4(),
        icon: None,
    };
    let id = record.id.clone();

    {
        let mut guard = state.lock().unwrap();
        guard.tabs.push(record);
        guard.save();
    }

    activate_tab(&window, &state, &id)?;
    Ok(state.lock().unwrap().to_dto())
}

#[tauri::command]
fn remove_tab(
    id: String,
    window: Window,
    state: State<'_, Mutex<AppState>>,
) -> Result<StateDto, String> {
    let next_active = {
        let mut guard = state.lock().unwrap();
        let idx = guard
            .tabs
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| "tab not found".to_string())?;
        let removed = guard.tabs.remove(idx);
        if let Some(icon) = removed.icon {
            let _ = fs::remove_file(guard.icons_dir.join(icon));
        }
        if let Some(wv) = guard.webviews.remove(&id) {
            let _ = wv.close();
        }

        let was_active = guard.active_id.as_deref() == Some(id.as_str());
        let next = if was_active {
            let len = guard.tabs.len();
            if len == 0 {
                None
            } else {
                Some(guard.tabs[idx.min(len - 1)].id.clone())
            }
        } else {
            guard.active_id.clone()
        };
        guard.active_id = next.clone();
        guard.save();
        next
    };

    if let Some(next_id) = next_active {
        activate_tab(&window, &state, &next_id)?;
    }

    Ok(state.lock().unwrap().to_dto())
}

#[tauri::command]
fn rename_tab(id: String, name: String, state: State<'_, Mutex<AppState>>) -> Result<StateDto, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("name is required".to_string());
    }
    let mut guard = state.lock().unwrap();
    let tab = guard
        .tabs
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or_else(|| "tab not found".to_string())?;
    tab.name = name;
    guard.save();
    Ok(guard.to_dto())
}

/// Reorders tabs to match `ids`. Rejects any list that isn't a permutation of
/// the current tabs, so a stale frontend can't drop or duplicate one.
#[tauri::command]
fn reorder_tabs(ids: Vec<String>, state: State<'_, Mutex<AppState>>) -> Result<StateDto, String> {
    let mut guard = state.lock().unwrap();

    if ids.len() != guard.tabs.len() {
        return Err("tab list out of sync".to_string());
    }
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != ids.len() || !ids.iter().all(|id| guard.tabs.iter().any(|t| &t.id == id)) {
        return Err("tab list out of sync".to_string());
    }

    let mut reordered = Vec::with_capacity(ids.len());
    for id in &ids {
        let tab = guard
            .tabs
            .iter()
            .find(|t| &t.id == id)
            .cloned()
            .ok_or_else(|| "tab not found".to_string())?;
        reordered.push(tab);
    }
    guard.tabs = reordered;
    guard.save();
    Ok(guard.to_dto())
}

#[tauri::command]
fn switch_tab(
    id: String,
    window: Window,
    state: State<'_, Mutex<AppState>>,
) -> Result<StateDto, String> {
    activate_tab(&window, &state, &id)?;
    Ok(state.lock().unwrap().to_dto())
}

/// Moves `offset` positions from the active tab, wrapping around the ends.
///
/// Tab changes that originate here (menu/keyboard) have to tell the tab strip
/// to re-render — unlike a click, where the strip already knows because it
/// issued the command itself.
fn cycle_tab(window: &Window, state: &State<'_, Mutex<AppState>>, offset: i32) {
    let target = {
        let guard = state.lock().unwrap();
        let count = guard.tabs.len();
        if count < 2 {
            return;
        }
        let Some(active) = guard.active_id.as_ref() else { return };
        let Some(current) = guard.tabs.iter().position(|t| &t.id == active) else {
            return;
        };
        let next = (current as i32 + offset).rem_euclid(count as i32) as usize;
        guard.tabs[next].id.clone()
    };
    if activate_tab(window, state, &target).is_err() {
        return;
    }
    let dto = state.lock().unwrap().to_dto();
    let _ = window.emit_to(EventTarget::webview(TABBAR_LABEL), "tabs-changed", dto);
}

/// Reloads whichever tab is currently showing.
#[tauri::command]
fn reload_active_tab(state: State<'_, Mutex<AppState>>) -> Result<(), String> {
    let guard = state.lock().unwrap();
    let active = guard
        .active_id
        .as_ref()
        .ok_or_else(|| "no active tab".to_string())?;
    let webview = guard
        .webviews
        .get(active)
        .ok_or_else(|| "active tab not loaded".to_string())?;
    webview.reload().map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_state,
            add_tab,
            remove_tab,
            switch_tab,
            rename_tab,
            reorder_tabs,
            tab_icon,
            refresh_tab_icon,
            set_tab_icon,
            reload_active_tab
        ])
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let config_path = data_dir.join("tabs.json");
            let icons_dir = data_dir.join("icons");
            let _ = fs::create_dir_all(&icons_dir);
            let persisted = load_or_seed(&config_path);

            let mut builder = WindowBuilder::new(app, MAIN_WINDOW_LABEL)
                .title("OmniComms")
                .inner_size(1200.0, 800.0)
                .min_inner_size(700.0, 480.0)
                .resizable(true)
                .maximizable(true);

            // Put the tab strip in the title bar: Overlay keeps the traffic
            // lights floating over our own content instead of reserving a
            // separate bar above it.
            #[cfg(target_os = "macos")]
            {
                builder = builder
                    .title_bar_style(TitleBarStyle::Overlay)
                    .hidden_title(true);
            }

            let window = builder.build()?;

            let (bar_pos, bar_size) = tabbar_bounds(&window);
            window.add_child(
                WebviewBuilder::new(TABBAR_LABEL, WebviewUrl::App(Default::default())),
                bar_pos,
                bar_size,
            )?;

            app.manage(Mutex::new(AppState {
                tabs: persisted.tabs,
                active_id: None,
                webviews: HashMap::new(),
                config_path,
                icons_dir,
            }));

            if let Some(active_id) = persisted.active_id {
                let state = app.state::<Mutex<AppState>>();
                activate_tab(&window, &state, &active_id)?;
            }

            // Tab cycling lives in the app menu rather than a JS key handler:
            // keystrokes go to whichever webview has focus, so a listener in the
            // tab strip would be dead whenever you're actually using a service.
            // Menu accelerators are handled before the webview sees the key.
            let next_item = MenuItemBuilder::with_id("tab_next", "Next Tab")
                .accelerator("Control+Tab")
                .build(app)?;
            let prev_item = MenuItemBuilder::with_id("tab_prev", "Previous Tab")
                .accelerator("Control+Shift+Tab")
                .build(app)?;
            let reload_item = MenuItemBuilder::with_id("tab_reload", "Reload Tab")
                .accelerator("CmdOrCtrl+R")
                .build(app)?;
            let tabs_menu = SubmenuBuilder::new(app, "Tabs")
                .item(&next_item)
                .item(&prev_item)
                .separator()
                .item(&reload_item)
                .build()?;

            let menu = Menu::default(app.handle())?;
            menu.append(&tabs_menu)?;
            app.set_menu(menu)?;

            app.on_menu_event(move |app, event| {
                let Some(win) = app.get_window(MAIN_WINDOW_LABEL) else { return };
                let state = app.state::<Mutex<AppState>>();
                match event.id().as_ref() {
                    "tab_next" => cycle_tab(&win, &state, 1),
                    "tab_prev" => cycle_tab(&win, &state, -1),
                    "tab_reload" => {
                        let guard = state.lock().unwrap();
                        if let Some(active) = guard.active_id.as_ref() {
                            if let Some(wv) = guard.webviews.get(active) {
                                let _ = wv.reload();
                            }
                        }
                    }
                    _ => {}
                }
            });

            let app_handle: AppHandle = app.handle().clone();
            window.on_window_event(move |event| {
                if let WindowEvent::Resized(_) = event {
                    if let Some(win) = app_handle.get_window(MAIN_WINDOW_LABEL) {
                        let state = app_handle.state::<Mutex<AppState>>();
                        relayout(&win, &state);
                    }
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
