use std::path::Path;
use std::time::{Duration, Instant};

use pollkit::Job;
use winisland_platform::{HostBackdropParams, TrayAction, WindowSize};

use crate::core::persistence::{get_config_path, load_config};
use crate::platform::{WindowRef, window};
use winisland_plugin_package::activate as zip_loader;
use winisland_plugin_package::marketplace::{self, MarketplacePlugin};
use winisland_render::RendererOptions;

use super::{App, BackdropCaptureState};

const BACKDROP_CAPTURE_RETRY: Duration = Duration::from_secs(10);

pub(super) fn update_host_backdrop(
    host_backdrop: &mut bool,
    id: winisland_platform::WindowId,
    params: HostBackdropParams,
) -> bool {
    if !*host_backdrop {
        return false;
    }
    match window().update_host_backdrop(id, params) {
        Ok(available) => available,
        Err(error) => {
            log::warn!("Host backdrop update failed: {error}");
            window().release_host_backdrop(id);
            *host_backdrop = false;
            crate::platform::update_capabilities(|caps| caps.host_backdrop = false);
            false
        }
    }
}

impl App {
    pub(super) fn handle_plugin_settings_request(&mut self) {
        let request = self
            .settings
            .as_mut()
            .and_then(crate::window::settings::SettingsApp::take_plugin_request);
        match request {
            Some(crate::window::settings::PluginSettingsRequest::HideIsland) => {
                self.visible = false;
                if let Some(window) = &self.window {
                    window.set_visible(false);
                }
                self.hide_host_backdrop();
            }
            Some(crate::window::settings::PluginSettingsRequest::Exit) => {
                self.close_settings();
                window().exit();
            }
            Some(crate::window::settings::PluginSettingsRequest::Install(path)) => {
                self.install_zip_drop(&path);
            }
            Some(crate::window::settings::PluginSettingsRequest::ShowGuide) => {
                self.open_guide();
            }
            Some(crate::window::settings::PluginSettingsRequest::LoadMarketplace) => {
                self.load_plugin_marketplace();
            }
            Some(crate::window::settings::PluginSettingsRequest::InstallMarketplace(plugin)) => {
                self.install_marketplace_plugin(*plugin);
            }
            Some(crate::window::settings::PluginSettingsRequest::SetEnabled { id, enabled }) => {
                let builtin = crate::plugin::is_builtin(&id);
                let result = if builtin {
                    self.plugin_host
                        .as_ref()
                        .ok_or_else(|| "Plugin host is unavailable".to_string())
                        .and_then(|host| {
                            host.set_builtin_enabled(&id, enabled)
                                .map_err(|error| error.to_string())
                        })
                } else {
                    self.plugin_mgr.set_plugin_enabled(&id, enabled)
                };
                if builtin && result.is_ok() {
                    self.refresh_v2_widgets();
                    self.update_plugin_services();
                }
                let plugin_inventory = result
                    .is_ok()
                    .then(|| self.plugin_mgr.installed_plugins_async());
                if let Some(settings) = self.settings.as_mut() {
                    match result {
                        Ok(()) => {
                            if let Some(scan) = plugin_inventory {
                                settings.set_plugin_inventory_scan(scan);
                            }
                            settings.set_plugin_status(
                                winisland_core::i18n::tr(if builtin {
                                    "plugin_state_updated"
                                } else {
                                    "plugin_state_restart"
                                }),
                                !builtin,
                            );
                        }
                        Err(error) => settings.set_plugin_status(
                            winisland_core::i18n::tr_args("plugin_state_failed", &[&error]),
                            false,
                        ),
                    }
                }
            }
            Some(crate::window::settings::PluginSettingsRequest::Uninstall { id }) => {
                if crate::plugin::is_builtin(&id) {
                    if let Some(settings) = self.settings.as_mut() {
                        settings.set_plugin_status(
                            winisland_core::i18n::tr("plugin_builtin_uninstall"),
                            false,
                        );
                    }
                    return;
                }
                if let Some(host) = &self.plugin_host
                    && let Err(error) = host.unload_if_loaded(&id)
                {
                    if let Some(settings) = self.settings.as_mut() {
                        settings.set_plugin_status(
                            winisland_core::i18n::tr_args(
                                "plugin_uninstall_failed",
                                &[&error.to_string()],
                            ),
                            false,
                        );
                    }
                    return;
                }
                let result = self.plugin_mgr.uninstall_plugin(&id);
                if result.is_ok() {
                    self.refresh_v2_widgets();
                }
                let plugin_inventory = result
                    .is_ok()
                    .then(|| self.plugin_mgr.installed_plugins_async());
                if let Some(settings) = self.settings.as_mut() {
                    match result {
                        Ok(()) => {
                            settings.set_plugin_widgets(self.widget_mgr.configurable_widgets());
                            if let Some(scan) = plugin_inventory {
                                settings.set_plugin_inventory_scan(scan);
                            }
                            settings.set_plugin_status(
                                winisland_core::i18n::tr("plugin_uninstalled"),
                                false,
                            );
                        }
                        Err(error) => settings.set_plugin_status(
                            winisland_core::i18n::tr_args("plugin_uninstall_failed", &[&error]),
                            false,
                        ),
                    }
                }
            }
            Some(crate::window::settings::PluginSettingsRequest::Restart) => {
                self.close_settings();
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).arg("--restart").spawn();
                }
                window().exit();
            }
            None => {}
        }
    }

    pub(super) fn invalidate_renderer(&mut self, reason: &str, now: Instant) {
        self.stop_recording();
        let renderer = self.renderer.take();
        if renderer.is_some() {
            log::warn!("Renderer invalidated: {reason}");
        }
        if let Some(settings) = self.settings.as_mut() {
            settings.invalidate_renderer_target();
        }
        crate::utils::backdrop::clear_blurred_cover_cache();
        crate::ui::expanded::music_view::clear_cover_cache();
        self.stop_backdrop_capture();
        if let Some(window_ref) = self.window {
            window().release_host_backdrop(window_ref.id());
        }
        self.host_backdrop = false;
        drop(renderer);
        self.renderer_retry_at = Some(now);
        self.next_frame_deadline = now;
    }

    pub(super) fn create_host_backdrop(&mut self, window_ref: &WindowRef) {
        self.host_backdrop = match window().start_host_backdrop(window_ref.id()) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("Host backdrop is unavailable: {error}");
                false
            }
        };
        if !self.host_backdrop {
            crate::platform::update_capabilities(|caps| caps.host_backdrop = false);
        }
    }

    pub(super) fn start_recording(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        match crate::utils::recorder::Recorder::start() {
            Ok((recorder, frames)) => {
                renderer.start_recording(frames, crate::utils::recorder::RECORDING_FPS);
                self.recorder = Some(recorder);
                log::info!("Recording island footage");
            }
            Err(error) => log::warn!("Recording could not start: {error}"),
        }
        self.refresh_tray_labels();
    }

    pub(super) fn stop_recording(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            return;
        };
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.stop_recording();
        }
        match recorder.finish() {
            Ok((directory, frames)) => {
                log::info!("Recorded {frames} frames to {}", directory.display());
                if let Err(error) = crate::platform::shell().reveal_path(&directory) {
                    log::warn!("Recording folder could not be shown: {error}");
                }
            }
            Err(error) => log::warn!("Recording could not be finished: {error}"),
        }
        self.refresh_tray_labels();
    }

    fn refresh_tray_labels(&self) {
        if self.tray_installed {
            let _ = crate::platform::shell().tray_update(
                crate::platform::tray_theme(self.is_light_theme),
                crate::platform::tray_labels(self.visible, self.recorder.is_some()),
            );
        }
    }

    pub(super) fn stop_backdrop_capture(&mut self) {
        stop_backdrop_capture(&mut self.backdrop_capture, self.window);
    }

    pub(super) fn hide_host_backdrop(&self) {
        if self.host_backdrop
            && let Some(window_ref) = self.window
        {
            window().hide_host_backdrop(window_ref.id());
        }
    }

    pub(super) fn recover_renderer(
        &mut self,
        window_ref: &WindowRef,
        now: Instant,
        retry_interval: Duration,
    ) {
        let Some(retry_at) = self.renderer_retry_at else {
            return;
        };
        if now < retry_at {
            self.next_frame_deadline = self.next_frame_deadline.min(retry_at);
            return;
        }

        match winisland_render::Renderer::new(
            match window().native_surface(window_ref.id()) {
                Some(surface) => surface,
                None => {
                    log::warn!("Renderer recovery failed: window surface unavailable");
                    self.renderer_retry_at = Some(now + retry_interval);
                    self.next_frame_deadline = now + retry_interval;
                    return;
                }
            },
            RendererOptions::new(self.geom.os_w, self.geom.os_h),
        ) {
            Ok(mut renderer) => {
                if let Some(settings) = self.settings.as_mut()
                    && let Err(error) = settings.recreate_renderer_target(&mut renderer)
                {
                    log::warn!("Settings renderer recovery failed: {error}");
                    self.renderer_retry_at = Some(now + retry_interval);
                    self.next_frame_deadline = now + retry_interval;
                    return;
                }
                if let Some(guide) = self.guide.as_mut()
                    && let Err(error) = guide.recreate_renderer_target(&mut renderer)
                {
                    log::warn!("Guide renderer recovery failed: {error}");
                }
                self.renderer = Some(renderer);
                self.create_host_backdrop(window_ref);
                self.renderer_retry_at = None;
                self.last_render_time = now;
                window_ref.request_redraw();
                log::info!("Renderer recovered");
            }
            Err(error) => {
                log::warn!("Renderer recovery failed: {error}");
                self.renderer_retry_at = Some(now + retry_interval);
                self.next_frame_deadline = now + retry_interval;
            }
        }
    }

    pub(super) fn set_aumid() {
        crate::platform::notify().set_app_identity();
    }

    pub(super) fn show_toast(title: &str, message: &str) {
        crate::platform::notify().show_toast(title, message);
    }

    pub(super) fn install_zip_drop(&mut self, path: &Path) {
        if self.pending_install.is_running() || self.pending_marketplace_download.is_running() {
            Self::show_toast("Plugin Info", "Another installation is already in progress");
            if let Some(settings) = self.settings.as_mut() {
                settings.set_plugin_status(
                    winisland_core::i18n::tr_args(
                        "plugin_install_failed",
                        &["another installation is already in progress"],
                    ),
                    false,
                );
            }
            return;
        }

        let plugin_dir = self.plugin_mgr.plugin_dir.clone();
        let zip_path = path.to_path_buf();
        self.pending_install = Job::spawn(move || {
            zip_loader::extract_plugin(
                &zip_path,
                &plugin_dir,
                winisland_plugin_api::abi::ABI_VERSION_2,
            )
            .map_err(|error| {
                if error.contains("Unsupported plugin ABI version 1") {
                    format!(
                        "该插件使用已废弃的 ABI v1，请升级或联系作者；建议先禁用该插件。 {error}"
                    )
                } else {
                    error
                }
            })
        });
        log::info!("Plugin extraction started in background thread");
    }

    fn load_plugin_marketplace(&mut self) {
        if self.pending_marketplace_catalog.is_running() {
            return;
        }
        if let Some(settings) = self.settings.as_mut() {
            settings.set_marketplace_loading();
        }
        let (job, done) = Job::pending();
        tokio::spawn(async move {
            let result = marketplace::load_catalog(winisland_plugin_api::abi::ABI_VERSION_2).await;
            done.send(result);
        });
        self.pending_marketplace_catalog = job;
    }

    fn install_marketplace_plugin(&mut self, plugin: MarketplacePlugin) {
        if plugin.revoked_reason.is_some()
            || !plugin.is_compatible(winisland_core::config::APP_VERSION)
        {
            if let Some(settings) = self.settings.as_mut() {
                settings.finish_marketplace_install();
                settings.set_plugin_status(
                    winisland_core::i18n::tr("plugin_marketplace_incompatible"),
                    false,
                );
            }
            return;
        }
        if self.pending_install.is_running() || self.pending_marketplace_download.is_running() {
            if let Some(settings) = self.settings.as_mut() {
                settings.finish_marketplace_install();
                settings.set_plugin_status(
                    winisland_core::i18n::tr_args(
                        "plugin_install_failed",
                        &["another installation is already in progress"],
                    ),
                    false,
                );
            }
            return;
        }
        if let Some(settings) = self.settings.as_mut() {
            settings.set_plugin_status(
                winisland_core::i18n::tr("plugin_marketplace_downloading"),
                false,
            );
        }
        let (job, done) = Job::pending();
        tokio::spawn(async move {
            let result =
                marketplace::download_plugin(&plugin, winisland_plugin_api::abi::ABI_VERSION_2)
                    .await;
            done.send(result);
        });
        self.pending_marketplace_download = job;
    }

    pub(super) fn open_settings(&mut self) {
        if let Some(settings) = &self.settings {
            settings.bring_to_front();
            return;
        }

        self.refresh_v2_widgets();
        let mut config = load_config();
        let plugin_widgets = self.widget_mgr.configurable_widgets();
        if winisland_core::config::normalize_active_plugin_widget_layout(
            &config.widget_layout,
            &mut config.plugin_widget_layout,
            &plugin_widgets,
        ) {
            crate::core::persistence::save_config(&config);
        }
        let plugin_settings_pages = self
            .plugin_host
            .as_ref()
            .and_then(|host| host.settings_pages_snapshot())
            .map(|(_, pages)| pages)
            .unwrap_or_default();
        let target_monitor = self
            .window
            .as_ref()
            .and_then(|window| Self::get_target_monitor(window, self.config.monitor_index));
        let mut settings = crate::window::settings::SettingsApp::new(
            config,
            Vec::new(),
            plugin_widgets,
            plugin_settings_pages,
        );
        settings.set_plugin_host(self.plugin_host.clone());
        let Some(renderer) = self.renderer.as_mut() else {
            log::error!("Cannot open settings without the shared D3D12 renderer");
            return;
        };
        settings.create_window(renderer, target_monitor);
        settings.set_plugin_inventory_scan(self.plugin_mgr.installed_plugins_async());
        if let Some(catalog) = self.marketplace_catalog.clone() {
            settings.set_marketplace_catalog(catalog);
        }
        self.settings = Some(settings);
        log::info!("Settings window opened in main process");
    }

    pub(super) fn open_guide(&mut self) {
        if let Some(guide) = &self.guide {
            guide.bring_to_front();
            return;
        }
        let target_monitor = self
            .window
            .as_ref()
            .and_then(|window| Self::get_target_monitor(window, self.config.monitor_index));
        let Some(renderer) = self.renderer.as_mut() else {
            log::error!("Cannot open guide without the shared D3D12 renderer");
            return;
        };
        let mut guide = crate::window::guide::GuideApp::new(self.config.settings_theme.clone());
        if let Err(error) = guide.create_window(renderer, target_monitor) {
            log::error!("Failed to open guide window: {error}");
            if let Some(target) = guide.close() {
                renderer.remove_target(target);
            }
            return;
        }
        self.guide = Some(guide);
        log::info!("Beginner guide opened");
    }

    pub(super) fn close_guide(&mut self) {
        let Some(mut guide) = self.guide.take() else {
            return;
        };
        if let Some(target) = guide.close()
            && let Some(renderer) = self.renderer.as_mut()
        {
            renderer.remove_target(target);
        }
        if !self.config.onboarding_completed {
            self.config.onboarding_completed = true;
            let mut config = load_config();
            config.onboarding_completed = true;
            crate::core::persistence::save_config(&config);
        }
        log::info!("Beginner guide closed");
    }

    pub(super) fn close_settings(&mut self) {
        if !self.tray_installed && !self.visible {
            self.visible = true;
            if let Some(window) = &self.window {
                window.set_visible(true);
                window.request_redraw();
            }
        }
        if let Some(mut settings) = self.settings.take() {
            if let Some(target) = settings.close()
                && let Some(renderer) = self.renderer.as_mut()
            {
                renderer.remove_target(target);
            }
            drop(settings);
            if !self.expanded {
                if let Err(error) = crate::platform::metrics().trim_working_set() {
                    log::warn!("Working set trim failed: {error}");
                }
                self.working_set_trim.restart(Instant::now());
            }
            log::info!("Settings window closed and resources released");
        }
    }

    pub(super) fn handle_tray_events(&mut self, window_ref: &WindowRef) {
        if self.tray_installed
            && let Some(action) = crate::platform::shell()
                .poll_tray_events()
                .into_iter()
                .next()
        {
            match action {
                TrayAction::Plugin(id) => {
                    if let Some(host) = &self.plugin_host
                        && let Some(command) = host
                            .runtime()
                            .extensions
                            .commands()
                            .into_iter()
                            .find(|command| command.resource == id)
                    {
                        let _ = host.runtime().extensions.invoke(&command.id, &[]);
                    }
                }
                TrayAction::ToggleVisibility => {
                    self.visible = !self.visible;
                    window_ref.set_visible(self.visible);
                    if !self.visible {
                        self.hide_host_backdrop();
                    } else {
                        window_ref.request_redraw();
                    }
                    let _ = crate::platform::shell().tray_update(
                        crate::platform::tray_theme(self.is_light_theme),
                        crate::platform::tray_labels(self.visible, self.recorder.is_some()),
                    );
                    log::info!("Tray: visibility toggled to {}", self.visible);
                }
                TrayAction::OpenSettings => {
                    log::info!("Tray: opening settings");
                    self.open_settings();
                }
                TrayAction::ShowGuide => {
                    log::info!("Tray: opening beginner guide");
                    self.open_guide();
                }
                TrayAction::ToggleRecording => {
                    if self.recorder.is_some() {
                        self.stop_recording();
                    } else {
                        self.start_recording();
                    }
                    window_ref.request_redraw();
                }
                TrayAction::Restart => {
                    log::info!("Tray: restarting application");
                    self.close_settings();
                    let _ = crate::platform::shell().restart(&["--restart".to_string()]);
                    window().exit();
                }
                TrayAction::Exit => {
                    log::info!("Tray: exiting application");
                    self.close_settings();
                    window().exit();
                }
            }
        }
    }

    pub(super) fn reload_config_if_changed(&mut self, window_ref: &WindowRef) {
        let now = Instant::now();
        if self.config_check.due(now) {
            let modified = std::fs::metadata(get_config_path())
                .and_then(|metadata| metadata.modified())
                .ok();
            if modified != self.last_config_modified {
                self.last_config_modified = modified;
                let current_config = load_config();
                if current_config != self.config {
                    let old_compact_scale = self.config.compact_scale;
                    let old_expanded_scale = self.config.expanded_scale;
                    let old_base_w = self.config.base_width;
                    let old_base_h = self.config.base_height;
                    let old_max_w = self.config.expanded_width;
                    let old_max_h = self.config.expanded_height;
                    let old_style = self.config.island_style.clone();
                    let old_mini_shape = self.config.mini_cover_shape.clone();
                    let old_expanded_shape = self.config.expanded_cover_shape.clone();
                    let old_font = self.config.custom_font_path.clone();
                    let old_smtc_enabled = self.config.smtc_enabled;
                    let old_system_controls_overlay = self.config.system_controls_overlay;
                    let old_position_x_offset = self.config.position_x_offset;
                    let old_position_y_offset = self.config.position_y_offset;
                    let old_notch_mode = self.config.notch_mode;
                    let old_monitor_index = self.config.monitor_index;
                    let page_order_changed = self.config.expanded_page_order
                        != current_config.expanded_page_order
                        || self.config.hidden_expanded_pages
                            != current_config.hidden_expanded_pages;

                    log::info!("Config changed, reloaded");
                    self.config = current_config;
                    window_ref.request_redraw();
                    if page_order_changed {
                        self.snap_to_current_page();
                    }
                    crate::ui::widget::resource_usage::set_configs(
                        &self.config.resource_metrics,
                        &self.config.compact_resource_metrics,
                    );
                    winisland_core::config::set_resource_widget_span(
                        self.config.resource_widget_columns,
                        self.config.resource_widget_rows,
                    );
                    if let Some(monitor) =
                        Self::get_target_monitor(window_ref, self.config.monitor_index)
                    {
                        self.migrate_legacy_dock_position(monitor.position(), monitor.size());
                    }
                    self.smtc.set_lyrics_mode(self.config.lyrics_mode.clone());
                    self.smtc
                        .set_lyrics_source(self.config.lyrics_source.clone());
                    self.smtc
                        .set_lyrics_local_dir(self.config.lyrics_local_dir.clone());
                    self.smtc.set_allowed_apps(self.config.smtc_apps.clone());
                    if old_system_controls_overlay != self.config.system_controls_overlay {
                        self.compact_overlay.set_system_controls_overlay_enabled(
                            self.config.system_controls_overlay,
                        );
                    }
                    if old_smtc_enabled != self.config.smtc_enabled {
                        self.smtc.set_enabled(self.config.smtc_enabled);
                        self.audio.set_target_app_id(self.audio_target_app_id());
                    }

                    if old_style != self.config.island_style {
                        crate::utils::backdrop::clear_blurred_cover_cache();
                    }

                    if old_mini_shape != self.config.mini_cover_shape
                        || old_expanded_shape != self.config.expanded_cover_shape
                    {
                        crate::ui::expanded::music_view::clear_cover_cache();
                    }

                    if old_font != self.config.custom_font_path {
                        winisland_render::text::FontManager::global()
                            .set_custom_font_path(self.config.custom_font_path.as_deref());
                    }

                    let window_size = self.required_window_size();
                    let surface_size_changed =
                        window_size.width != self.geom.os_w || window_size.height != self.geom.os_h;
                    let layout_size_changed = (old_compact_scale - self.config.compact_scale).abs()
                        > 0.001
                        || (old_expanded_scale - self.config.expanded_scale).abs() > 0.001
                        || (old_base_w - self.config.base_width).abs() > 0.1
                        || (old_base_h - self.config.base_height).abs() > 0.1
                        || (old_max_w - self.config.expanded_width).abs() > 0.1
                        || (old_max_h - self.config.expanded_height).abs() > 0.1;
                    let position_changed = old_position_x_offset != self.config.position_x_offset
                        || old_position_y_offset != self.config.position_y_offset
                        || old_monitor_index != self.config.monitor_index
                        || old_notch_mode != self.config.notch_mode;

                    if surface_size_changed {
                        self.geom.os_w = window_size.width;
                        self.geom.os_h = window_size.height;
                        let _ = window_ref
                            .request_inner_size(WindowSize::new(self.geom.os_w, self.geom.os_h));
                        if let Some(renderer) = self.renderer.as_mut() {
                            let target = renderer.main_target();
                            if let Err(error) =
                                renderer.resize(target, self.geom.os_w, self.geom.os_h)
                            {
                                log::error!("Renderer resize failed: {error}");
                            }
                        }
                    }

                    if (layout_size_changed || surface_size_changed || position_changed)
                        && let Some(monitor) =
                            Self::get_target_monitor(window_ref, self.config.monitor_index)
                    {
                        if let Some(settings) = self.settings.as_mut() {
                            settings.set_target_monitor(monitor.clone());
                        }
                        let mon_size = monitor.size();
                        let mon_pos = monitor.position();
                        self.update_display_frame_interval(&monitor);
                        if mon_size.width > 0 && mon_size.height > 0 {
                            self.geom.monitor_size = (mon_size.width, mon_size.height);
                            self.geom.monitor_pos = (mon_pos.x, mon_pos.y);
                            let (position_x, position_y) =
                                self.compute_window_position(mon_pos, mon_size);
                            self.set_configured_window_position(window_ref, position_x, position_y);
                            self.geom.position_restore_after = None;
                        }
                    }
                }
            }
        }

        if !self.monitor_check.due(now) {
            return;
        }
        if let Some(monitor) = Self::get_target_monitor(window_ref, self.config.monitor_index) {
            if let Some(settings) = self.settings.as_mut() {
                settings.set_target_monitor(monitor.clone());
            }
            let mon_size = monitor.size();
            let mon_pos = monitor.position();
            self.update_display_frame_interval(&monitor);
            let cur_mon_size = (mon_size.width, mon_size.height);
            let cur_mon_pos = (mon_pos.x, mon_pos.y);
            if (cur_mon_size != self.geom.monitor_size || cur_mon_pos != self.geom.monitor_pos)
                && cur_mon_size.0 > 0
                && cur_mon_size.1 > 0
            {
                self.geom.monitor_size = cur_mon_size;
                self.geom.monitor_pos = cur_mon_pos;
                let (position_x, position_y) = self.compute_window_position(mon_pos, mon_size);
                self.set_configured_window_position(window_ref, position_x, position_y);
                self.geom.position_restore_after = Some(now + Duration::from_millis(750));
            }
        }
    }
}

pub(super) struct BackdropCaptureInput {
    pub(super) window: Option<WindowRef>,
    pub(super) origin: (i32, i32),
    pub(super) frame_interval: Duration,
    pub(super) interest: winisland_render::Rect,
    pub(super) frost_sigma: f32,
    pub(super) active: bool,
    pub(super) animating: bool,
}

pub(super) fn update_backdrop_capture(
    state: &mut BackdropCaptureState,
    renderer: &mut winisland_render::Renderer,
    input: BackdropCaptureInput,
) -> bool {
    use winisland_platform::{BackdropCaptureRequest, Point, Rect};

    let Some(window_ref) = input.window else {
        return false;
    };
    if !input.active {
        stop_backdrop_capture(state, input.window);
        renderer.release_backdrop();
        return false;
    }
    match *state {
        BackdropCaptureState::Running => {}
        BackdropCaptureState::Failed(at) if at.elapsed() < BACKDROP_CAPTURE_RETRY => {
            return false;
        }
        _ => match window().start_backdrop_capture(window_ref.id(), renderer.adapter_luid()) {
            Ok(()) => *state = BackdropCaptureState::Running,
            Err(error) => {
                log::warn!("Liquid glass backdrop capture is unavailable: {error}");
                *state = BackdropCaptureState::Failed(Instant::now());
                return false;
            }
        },
    }
    let size = window_ref.inner_size();
    let share = match renderer.backdrop_share(size.width, size.height) {
        Ok(share) => share,
        Err(error) => {
            log::warn!("Liquid glass backdrop texture is unavailable: {error}");
            stop_backdrop_capture(state, input.window);
            *state = BackdropCaptureState::Failed(Instant::now());
            return false;
        }
    };
    let (x, y) = input.origin;
    let margin = (input.frost_sigma * 3.0).ceil();
    let interest = input.interest;
    let refresh_hz = (1_000_000_000 / input.frame_interval.as_nanos().max(1)) as u32;
    let integrated =
        crate::platform::display().gpu_profile() == winisland_platform::GpuProfile::Integrated;
    let rate_limit = match (integrated, input.animating) {
        (false, true) => 60,
        (true, false) => 20,
        _ => 30,
    };
    let frame = window().update_backdrop_capture(
        window_ref.id(),
        BackdropCaptureRequest {
            share,
            origin: Point { x, y },
            interest: Rect {
                left: x + (interest.left - margin).floor() as i32,
                top: y + (interest.top - margin).floor() as i32,
                right: x + (interest.right + margin).ceil() as i32,
                bottom: y + (interest.bottom + margin).ceil() as i32,
            },
            read_value: renderer.backdrop_read_value(),
            max_rate_hz: refresh_hz.clamp(1, rate_limit),
        },
    );
    renderer
        .set_backdrop_frame(frame, input.frost_sigma)
        .is_ok()
        && frame.is_some()
}

fn stop_backdrop_capture(state: &mut BackdropCaptureState, window_ref: Option<WindowRef>) {
    if *state != BackdropCaptureState::Running {
        return;
    }
    if let Some(window_ref) = window_ref {
        window().stop_backdrop_capture(window_ref.id());
    }
    *state = BackdropCaptureState::Stopped;
}
