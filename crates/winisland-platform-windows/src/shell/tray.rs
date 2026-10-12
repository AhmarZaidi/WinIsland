use std::cell::RefCell;

use tray_icon::menu::{Menu, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winisland_platform::{PlatformError, TrayAction, TrayLabels, TrayTheme};

struct WindowsTray {
    menu: Menu,
    plugin_items: Vec<(u64, MenuItem)>,
    tray: TrayIcon,
    toggle: MenuItem,
    settings: MenuItem,
    guide: MenuItem,
    record: MenuItem,
    restart: MenuItem,
    exit: MenuItem,
    theme: TrayTheme,
}

thread_local! {
    static TRAY: RefCell<Option<WindowsTray>> = const { RefCell::new(None) };
}

fn icon(theme: TrayTheme) -> Result<Icon, PlatformError> {
    let bytes: &[u8] = match theme {
        TrayTheme::Light => include_bytes!("../../../../resources/icon-dark.png"),
        TrayTheme::Dark => include_bytes!("../../../../resources/icon.png"),
    };
    let rgba = image::load_from_memory(bytes)
        .map_err(PlatformError::backend)?
        .to_rgba8();
    let (width, height) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), width, height).map_err(PlatformError::backend)
}

pub(super) fn install(theme: TrayTheme, labels: TrayLabels) -> Result<(), PlatformError> {
    let menu = Menu::new();
    let toggle = MenuItem::new(&labels.toggle, true, None);
    let settings = MenuItem::new(&labels.settings, true, None);
    let guide = MenuItem::new(&labels.guide, true, None);
    let record = MenuItem::new(&labels.record, true, None);
    let restart = MenuItem::new(&labels.restart, true, None);
    let exit = MenuItem::new(&labels.exit, true, None);
    menu.append(&toggle).map_err(PlatformError::backend)?;
    menu.append(&settings).map_err(PlatformError::backend)?;
    menu.append(&guide).map_err(PlatformError::backend)?;
    menu.append(&record).map_err(PlatformError::backend)?;
    menu.append(&restart).map_err(PlatformError::backend)?;
    menu.append(&exit).map_err(PlatformError::backend)?;
    let tray = TrayIconBuilder::new()
        .with_tooltip(&labels.tooltip)
        .with_menu(Box::new(menu.clone()))
        .with_icon(icon(theme)?)
        .build()
        .map_err(PlatformError::backend)?;
    TRAY.with(|slot| {
        *slot.borrow_mut() = Some(WindowsTray {
            menu,
            plugin_items: Vec::new(),
            tray,
            toggle,
            settings,
            guide,
            record,
            restart,
            exit,
            theme,
        });
    });
    Ok(())
}

pub(super) fn update(theme: TrayTheme, labels: TrayLabels) -> Result<(), PlatformError> {
    TRAY.with(|slot| {
        let mut tray = slot.borrow_mut();
        let tray = tray.as_mut().ok_or(PlatformError::Unavailable("tray"))?;
        if tray.theme != theme {
            tray.tray
                .set_icon(Some(icon(theme)?))
                .map_err(PlatformError::backend)?;
            tray.theme = theme;
        }
        tray.toggle.set_text(&labels.toggle);
        tray.settings.set_text(&labels.settings);
        tray.guide.set_text(&labels.guide);
        tray.record.set_text(&labels.record);
        tray.restart.set_text(&labels.restart);
        tray.exit.set_text(&labels.exit);
        Ok(())
    })
}

pub(super) fn poll_events() -> Vec<TrayAction> {
    TRAY.with(|slot| {
        let tray = slot.borrow();
        let Some(tray) = tray.as_ref() else {
            return Vec::new();
        };
        let Ok(event) = tray_icon::menu::MenuEvent::receiver().try_recv() else {
            return Vec::new();
        };
        let action = if event.id == tray.toggle.id() {
            Some(TrayAction::ToggleVisibility)
        } else if event.id == tray.settings.id() {
            Some(TrayAction::OpenSettings)
        } else if event.id == tray.guide.id() {
            Some(TrayAction::ShowGuide)
        } else if event.id == tray.record.id() {
            Some(TrayAction::ToggleRecording)
        } else if event.id == tray.restart.id() {
            Some(TrayAction::Restart)
        } else if event.id == tray.exit.id() {
            Some(TrayAction::Exit)
        } else {
            tray.plugin_items
                .iter()
                .find_map(|(id, item)| (event.id == item.id()).then_some(TrayAction::Plugin(*id)))
        };
        action.into_iter().collect()
    })
}

pub(super) fn set_plugin_commands(
    commands: &[winisland_platform::PluginCommand],
) -> Result<(), PlatformError> {
    TRAY.with(|slot| {
        let mut tray = slot.borrow_mut();
        let Some(tray) = tray.as_mut() else {
            return Ok(());
        };
        for (_, item) in tray.plugin_items.drain(..) {
            tray.menu.remove(&item).map_err(PlatformError::backend)?;
        }
        for command in commands.iter().filter(|command| command.menu) {
            let item = MenuItem::new(&command.title, command.enabled, None);
            tray.menu.append(&item).map_err(PlatformError::backend)?;
            tray.plugin_items.push((command.id, item));
        }
        Ok(())
    })
}
