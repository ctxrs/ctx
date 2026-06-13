use super::*;

#[cfg(target_os = "macos")]
pub(crate) static SETTINGS_BUTTON_APP: OnceLock<tauri::AppHandle> = OnceLock::new();
#[cfg(target_os = "macos")]
pub(crate) static SETTINGS_BUTTON_CLASS: Once = Once::new();

#[cfg(target_os = "macos")]
extern "C" fn settings_button_clicked(_this: &AnyObject, _cmd: Sel, _sender: *mut AnyObject) {
    if let Some(app) = SETTINGS_BUTTON_APP.get() {
        emit_settings_inplace(app, _this);
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn emit_settings_inplace(app: &tauri::AppHandle, target: &AnyObject) {
    const OPEN_SETTINGS_SCRIPT: &str = "(() => { let t = '/settings'; const p = window.location.pathname || '/'; \
        if (p.startsWith('/workspaces/')) { const ws = p.split('/')[2]; if (ws) { t = `/settings?ws=${encodeURIComponent(ws)}`; } } \
        window.dispatchEvent(new CustomEvent('ctx:open-settings', { detail: { target: t } })); })();";
    const WINDOW_LABEL_IVAR: &[u8] = b"ctxWindowLabel\0";
    let Some(class) = settings_button_target_class() else {
        return;
    };
    let Some(ivar_name) = CStr::from_bytes_with_nul(WINDOW_LABEL_IVAR).ok() else {
        return;
    };
    let ivar = class.instance_variable(ivar_name);
    if let Some(ivar) = ivar {
        let label_ptr = unsafe { *ivar.load::<*const std::ffi::c_char>(target) };
        if !label_ptr.is_null() {
            let label = unsafe { CStr::from_ptr(label_ptr) }
                .to_string_lossy()
                .into_owned();
            if app.get_webview_window(&label).is_some() {
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window.eval(OPEN_SETTINGS_SCRIPT);
                }
                return;
            }
        }
    }
    if app.get_webview_window("main").is_some() {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.eval(OPEN_SETTINGS_SCRIPT);
        }
        return;
    }
    let _ = app.emit("desktop_open_settings", ());
}

#[cfg(target_os = "macos")]
pub(crate) fn settings_button_target_class() -> Option<&'static AnyClass> {
    const CLASS_NAME: &[u8] = b"CtxSettingsButtonTarget\0";
    const WINDOW_LABEL_IVAR: &[u8] = b"ctxWindowLabel\0";
    SETTINGS_BUTTON_CLASS.call_once(|| {
        let Some(class_name) = CStr::from_bytes_with_nul(CLASS_NAME).ok() else {
            eprintln!("failed to parse settings button class name");
            return;
        };
        let Some(mut builder) = ClassBuilder::new(class_name, NSObject::class()) else {
            eprintln!("failed to register settings button class");
            return;
        };
        let Some(ivar_name) = CStr::from_bytes_with_nul(WINDOW_LABEL_IVAR).ok() else {
            eprintln!("failed to parse settings button ivar name");
            return;
        };
        builder.add_ivar::<*const std::ffi::c_char>(ivar_name);
        unsafe {
            let open_settings: extern "C" fn(&'static AnyObject, Sel, *mut AnyObject) =
                settings_button_clicked;
            builder.add_method(sel!(openSettings:), open_settings);
        }
        builder.register();
    });
    let class_name = CStr::from_bytes_with_nul(CLASS_NAME).ok()?;
    AnyClass::get(class_name)
}

#[cfg(target_os = "macos")]
pub(crate) fn install_macos_settings_button(
    app: &tauri::AppHandle,
    window: &tauri::WebviewWindow,
) -> Result<()> {
    SETTINGS_BUTTON_APP.get_or_init(|| app.clone());
    let window_label = window.label().to_string();
    let icon_path = app.path().resource_dir().ok().and_then(|dir| {
        dir.join("bundles/lucide-settings.svg")
            .to_str()
            .map(str::to_string)
    });
    window.with_webview(move |webview| unsafe {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let ns_window: &NSWindow = &*webview.ns_window().cast();
        ns_window.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
        ns_window.setTitleVisibility(NSWindowTitleVisibility::Visible);
        let image = icon_path
            .as_deref()
            .and_then(|path| load_lucide_settings_icon(path))
            .or_else(|| NSImage::imageNamed(NSImageNamePreferencesGeneral));
        let Some(image) = image else {
            return;
        };
        let Some(cls) = settings_button_target_class() else {
            return;
        };
        let target: Retained<AnyObject> = msg_send![cls, new];
        let target = &*Retained::into_raw(target);
        let Ok(label_cstr) = CString::new(window_label.as_str()) else {
            return;
        };
        let label_ptr = label_cstr.into_raw();
        let Some(ivar_name) = CStr::from_bytes_with_nul(b"ctxWindowLabel\0").ok() else {
            return;
        };
        let Some(ivar) = cls.instance_variable(ivar_name) else {
            return;
        };
        ivar.load_ptr::<*const std::ffi::c_char>(target)
            .write(label_ptr as *const std::ffi::c_char);
        let button = NSButton::buttonWithImage_target_action(
            &image,
            Some(target),
            Some(sel!(openSettings:)),
            mtm,
        );
        button.setBordered(false);
        let tint = NSColor::secondaryLabelColor();
        button.setContentTintColor(Some(&tint));
        let mut frame = button.frame();
        frame.size.height += 3.0;
        button.setFrame(frame);

        let mut spacer_frame = frame;
        spacer_frame.size.width = spacer_frame.size.width.max(22.0);
        let spacer = NSView::initWithFrame(NSView::alloc(mtm), spacer_frame);
        spacer.setAlphaValue(0.0);
        let leading_accessory = NSTitlebarAccessoryViewController::new(mtm);
        leading_accessory.setView(spacer.as_ref());
        leading_accessory.setLayoutAttribute(NSLayoutAttribute::Leading);
        leading_accessory.setAutomaticallyAdjustsSize(true);
        ns_window.addTitlebarAccessoryViewController(&leading_accessory);

        let accessory = NSTitlebarAccessoryViewController::new(mtm);
        accessory.setView(button.as_ref());
        accessory.setLayoutAttribute(NSLayoutAttribute::Trailing);
        accessory.setAutomaticallyAdjustsSize(true);
        ns_window.addTitlebarAccessoryViewController(&accessory);
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn load_lucide_settings_icon(icon_path: &str) -> Option<Retained<NSImage>> {
    let ns_path = NSString::from_str(icon_path);
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns_path)?;
    image.setTemplate(true);
    Some(image)
}

pub(crate) fn apply_workbench_titlebar<'a, R: tauri::Runtime, M: tauri::Manager<R>>(
    builder: tauri::WebviewWindowBuilder<'a, R, M>,
) -> tauri::WebviewWindowBuilder<'a, R, M> {
    #[cfg(target_os = "macos")]
    {
        return builder
            .title_bar_style(tauri::TitleBarStyle::Visible)
            .hidden_title(false);
    }
    #[cfg(target_os = "linux")]
    {
        builder.decorations(LINUX_WORKBENCH_WINDOW_DECORATIONS)
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "linux")))]
    {
        builder.decorations(false)
    }
}

pub(crate) fn ensure_workbench_titlebar(window: &tauri::WebviewWindow) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        window
            .set_decorations(LINUX_WORKBENCH_WINDOW_DECORATIONS)
            .context("setting Linux workbench window decorations")?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
const LINUX_WORKBENCH_WINDOW_DECORATIONS: bool = true;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn linux_workbench_windows_use_native_decorations() {
        assert!(
            super::LINUX_WORKBENCH_WINDOW_DECORATIONS,
            "Linux workbench windows should keep native titlebar controls"
        );
    }
}
