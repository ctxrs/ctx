#[cfg(target_os = "macos")]
use anyhow::Context;
use ctx_desktop_ipc::{DesktopNotificationPermission, DesktopShowSystemNotificationReq};

use super::automation::{
    DesktopClearDeliveredNotificationsReq, DesktopDeliveredNotificationEntry,
    DesktopDeliveredNotificationSnapshot, NOTIFICATION_IDENTIFIER_PREFIX,
};
#[cfg(target_os = "macos")]
use super::deep_links::notification_deep_link_from_payload_value;
use super::deep_links::open_notification_target;

#[cfg(target_os = "macos")]
const NOTIFICATION_DEEP_LINK_USER_INFO_KEY: &str = "deep_link";

#[cfg(target_os = "macos")]
use std::ptr::NonNull;
#[cfg(target_os = "macos")]
use std::sync::{mpsc, OnceLock};
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use block2::{DynBlock, RcBlock};
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
#[cfg(target_os = "macos")]
use objc2::{define_class, msg_send, AnyThread};
#[cfg(all(target_os = "macos", feature = "automation"))]
use objc2_foundation::NSArray;
#[cfg(target_os = "macos")]
use objc2_foundation::{NSDictionary, NSError, NSObject, NSObjectProtocol, NSString};
#[cfg(target_os = "macos")]
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationDefaultActionIdentifier, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSettings, UNNotificationSound,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

#[cfg(target_os = "macos")]
static MACOS_NOTIFICATION_APP: OnceLock<tauri::AppHandle> = OnceLock::new();

#[cfg(target_os = "macos")]
static MACOS_NOTIFICATION_DELEGATE: OnceLock<Retained<MacosNotificationDelegate>> = OnceLock::new();

#[cfg(target_os = "macos")]
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AnyThread]
    struct MacosNotificationDelegate;

    unsafe impl NSObjectProtocol for MacosNotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for MacosNotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present_notification(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive_notification_response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            if macos_response_is_default_action(response) {
                let notification = response.notification();
                let request = notification.request();
                let content = request.content();
                let user_info = content.userInfo();
                if let Some(deep_link) = macos_notification_deep_link(&user_info) {
                    if let Some(app) = MACOS_NOTIFICATION_APP.get().cloned() {
                        open_notification_target(app, &deep_link);
                    }
                }
            }
            completion_handler.call(());
        }
    }
);

#[cfg(target_os = "macos")]
impl MacosNotificationDelegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn install_macos_notification_delegate(app: tauri::AppHandle) {
    let _ = MACOS_NOTIFICATION_APP.set(app);
    let delegate = MACOS_NOTIFICATION_DELEGATE.get_or_init(MacosNotificationDelegate::new);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn install_macos_notification_delegate(_app: tauri::AppHandle) {}

#[cfg(target_os = "macos")]
fn macos_response_is_default_action(response: &UNNotificationResponse) -> bool {
    let action_identifier = response.actionIdentifier();
    unsafe { &*action_identifier == UNNotificationDefaultActionIdentifier }
}

#[cfg(target_os = "macos")]
fn macos_notification_deep_link(user_info: &NSDictionary) -> Option<String> {
    let key = NSString::from_str(NOTIFICATION_DEEP_LINK_USER_INFO_KEY);
    let value = user_info.objectForKey(&**key)?;
    let value = value.downcast::<NSString>().ok()?;
    notification_deep_link_from_payload_value(Some(&value.to_string()))
}

#[cfg(target_os = "macos")]
fn map_macos_permission(status: UNAuthorizationStatus) -> DesktopNotificationPermission {
    if status == UNAuthorizationStatus::Authorized
        || status == UNAuthorizationStatus::Provisional
        || status == UNAuthorizationStatus::Ephemeral
    {
        DesktopNotificationPermission::Granted
    } else if status == UNAuthorizationStatus::Denied {
        DesktopNotificationPermission::Denied
    } else {
        DesktopNotificationPermission::Default
    }
}

#[cfg(target_os = "macos")]
fn macos_notification_permission() -> DesktopNotificationPermission {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let (tx, rx) = mpsc::sync_channel(1);
    let completion = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        let permission = unsafe { map_macos_permission(settings.as_ref().authorizationStatus()) };
        let _ = tx.send(permission);
    });
    center.getNotificationSettingsWithCompletionHandler(&completion);
    rx.recv_timeout(Duration::from_secs(2))
        .unwrap_or(DesktopNotificationPermission::Default)
}

#[cfg(target_os = "macos")]
fn macos_request_notification_permission() -> DesktopNotificationPermission {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let (tx, rx) = mpsc::sync_channel(1);
    let completion = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
        let permission = if granted.as_bool() {
            DesktopNotificationPermission::Granted
        } else {
            DesktopNotificationPermission::Denied
        };
        let _ = tx.send(permission);
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge,
        &completion,
    );
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| macos_notification_permission())
}

#[cfg(target_os = "macos")]
fn schedule_macos_notification(request: &UNNotificationRequest) -> anyhow::Result<()> {
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let (tx, rx) = mpsc::sync_channel(1);
    let completion = RcBlock::new(move |err: *mut NSError| {
        let result = match NonNull::new(err) {
            Some(err) => {
                let err = unsafe { err.as_ref() };
                Err(format!("failed to schedule macOS notification: {err}"))
            }
            None => Ok(()),
        };
        let _ = tx.send(result);
    });
    center.addNotificationRequest_withCompletionHandler(request, Some(&completion));
    rx.recv_timeout(Duration::from_secs(2))
        .context("timed out scheduling macOS notification")?
        .map_err(anyhow::Error::msg)
}

#[cfg(any(all(target_os = "macos", feature = "automation"), test))]
fn normalize_delivered_notification_identifiers(
    identifiers: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut normalized = Vec::new();
    for identifier in identifiers {
        let trimmed = identifier.trim();
        if trimmed.is_empty() {
            anyhow::bail!("delivered notification identifier must not be empty");
        }
        if !trimmed.starts_with(NOTIFICATION_IDENTIFIER_PREFIX) {
            anyhow::bail!("delivered notification identifier is not owned by ctx: {trimmed}");
        }
        normalized.push(trimmed.to_string());
    }
    if normalized.is_empty() {
        anyhow::bail!("at least one delivered notification identifier is required");
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

#[cfg(all(target_os = "macos", feature = "automation"))]
fn macos_delivered_notification_entry(
    notification: &UNNotification,
) -> DesktopDeliveredNotificationEntry {
    let request = notification.request();
    let identifier = request.identifier().to_string();
    let content = request.content();
    let title = content.title().to_string();
    let body = content.body().to_string();
    let user_info = content.userInfo();
    DesktopDeliveredNotificationEntry {
        body: if body.trim().is_empty() {
            None
        } else {
            Some(body)
        },
        deep_link: macos_notification_deep_link(&user_info),
        identifier,
        title,
    }
}

#[cfg(all(target_os = "macos", feature = "automation"))]
fn macos_delivered_notification_entries() -> anyhow::Result<Vec<DesktopDeliveredNotificationEntry>>
{
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let (tx, rx) = mpsc::sync_channel(1);
    let completion = RcBlock::new(move |notifications: NonNull<NSArray<UNNotification>>| {
        let notifications = unsafe { notifications.as_ref() };
        let entries = notifications
            .to_vec()
            .iter()
            .map(|notification| macos_delivered_notification_entry(notification))
            .collect::<Vec<_>>();
        let _ = tx.send(entries);
    });
    center.getDeliveredNotificationsWithCompletionHandler(&completion);
    rx.recv_timeout(Duration::from_secs(2))
        .context("timed out reading delivered macOS notifications")
}

#[cfg(all(target_os = "macos", feature = "automation"))]
fn macos_clear_delivered_notifications(
    req: DesktopClearDeliveredNotificationsReq,
) -> anyhow::Result<()> {
    let identifiers = normalize_delivered_notification_identifiers(&req.identifiers)?;
    let ns_identifiers = identifiers
        .iter()
        .map(|identifier| NSString::from_str(identifier))
        .collect::<Vec<_>>();
    let identifier_array = NSArray::from_retained_slice(&ns_identifiers);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.removeDeliveredNotificationsWithIdentifiers(&identifier_array);
    Ok(())
}

#[cfg(target_os = "macos")]
fn show_macos_notification(
    app: &tauri::AppHandle,
    req: DesktopShowSystemNotificationReq,
    deep_link: String,
) -> anyhow::Result<()> {
    install_macos_notification_delegate(app.clone());

    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&req.title));
    if let Some(body) = req.body.as_deref() {
        content.setBody(&NSString::from_str(body));
    }
    let sound = UNNotificationSound::defaultSound();
    content.setSound(Some(&sound));

    let deep_link_key = NSString::from_str(NOTIFICATION_DEEP_LINK_USER_INFO_KEY);
    let deep_link_value = NSString::from_str(&deep_link);
    let user_info: Retained<NSDictionary<NSString, NSString>> =
        NSDictionary::from_slices(&[&*deep_link_key], &[&*deep_link_value]);
    unsafe { content.setUserInfo(user_info.cast_unchecked::<AnyObject, AnyObject>()) };

    let identifier = NSString::from_str(&format!(
        "{}{}",
        NOTIFICATION_IDENTIFIER_PREFIX,
        uuid::Uuid::new_v4()
    ));
    let request =
        UNNotificationRequest::requestWithIdentifier_content_trigger(&identifier, &content, None);
    schedule_macos_notification(&request)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn show_linux_notification(
    app: &tauri::AppHandle,
    req: DesktopShowSystemNotificationReq,
    deep_link: String,
) -> anyhow::Result<()> {
    let mut notification = notify_rust::Notification::new();
    notification.summary(&req.title);
    if let Some(body) = req.body.as_deref() {
        notification.body(body);
    }
    notification.action("default", "Open");
    let handle = notification.show()?;
    let app = app.clone();
    std::thread::spawn(move || {
        handle.wait_for_action(|action| {
            if action == "default" {
                open_notification_target(app, &deep_link);
            }
        });
    });
    Ok(())
}

#[cfg(target_os = "windows")]
fn show_windows_notification(
    req: DesktopShowSystemNotificationReq,
    deep_link: String,
) -> anyhow::Result<()> {
    let _ = deep_link;
    let mut notification = notify_rust::Notification::new();
    notification.summary(&req.title);
    if let Some(body) = req.body.as_deref() {
        notification.body(body);
    }
    notification.show()?;
    Ok(())
}

pub(crate) fn notification_permission() -> DesktopNotificationPermission {
    #[cfg(target_os = "macos")]
    {
        return macos_notification_permission();
    }

    #[cfg(not(target_os = "macos"))]
    {
        DesktopNotificationPermission::Granted
    }
}

pub(crate) fn request_notification_permission() -> DesktopNotificationPermission {
    #[cfg(target_os = "macos")]
    {
        return macos_request_notification_permission();
    }

    #[cfg(not(target_os = "macos"))]
    {
        DesktopNotificationPermission::Granted
    }
}

pub(crate) fn show_system_notification(
    app: &tauri::AppHandle,
    req: DesktopShowSystemNotificationReq,
    deep_link: String,
) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        return show_macos_notification(app, req, deep_link);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return show_linux_notification(app, req, deep_link);
    }

    #[cfg(target_os = "windows")]
    {
        return show_windows_notification(req, deep_link);
    }

    #[allow(unreachable_code)]
    Ok(())
}

pub(crate) fn desktop_get_delivered_notification_automation_snapshot(
) -> Result<DesktopDeliveredNotificationSnapshot, String> {
    #[cfg(all(feature = "automation", target_os = "macos"))]
    {
        return macos_delivered_notification_entries()
            .map(|delivered| DesktopDeliveredNotificationSnapshot { delivered })
            .map_err(super::super::to_err);
    }

    #[cfg(all(feature = "automation", not(target_os = "macos")))]
    {
        Err("desktop_get_delivered_notification_automation_snapshot is macOS-only".to_string())
    }

    #[cfg(not(feature = "automation"))]
    {
        Err("desktop_get_delivered_notification_automation_snapshot is automation-only".to_string())
    }
}

pub(crate) fn desktop_clear_delivered_notification_automation_snapshot(
    req: DesktopClearDeliveredNotificationsReq,
) -> Result<(), String> {
    #[cfg(all(feature = "automation", target_os = "macos"))]
    {
        return macos_clear_delivered_notifications(req).map_err(super::super::to_err);
    }

    #[cfg(all(feature = "automation", not(target_os = "macos")))]
    {
        let DesktopClearDeliveredNotificationsReq { identifiers } = req;
        let _ = identifiers;
        Err("desktop_clear_delivered_notification_automation_snapshot is macOS-only".to_string())
    }

    #[cfg(not(feature = "automation"))]
    {
        let DesktopClearDeliveredNotificationsReq { identifiers } = req;
        let _ = identifiers;
        Err(
            "desktop_clear_delivered_notification_automation_snapshot is automation-only"
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_notification_clear_requires_ctx_owned_identifiers() {
        let req = DesktopClearDeliveredNotificationsReq {
            identifiers: vec!["ctx-task-notification-from-req".to_string()],
        };
        assert_eq!(
            normalize_delivered_notification_identifiers(&req.identifiers)
                .expect("req identifiers"),
            vec!["ctx-task-notification-from-req".to_string()]
        );

        assert_eq!(
            normalize_delivered_notification_identifiers(&[
                " ctx-task-notification-b ".to_string(),
                "ctx-task-notification-a".to_string(),
                "ctx-task-notification-a".to_string(),
            ])
            .expect("identifiers"),
            vec![
                "ctx-task-notification-a".to_string(),
                "ctx-task-notification-b".to_string(),
            ]
        );

        assert!(normalize_delivered_notification_identifiers(&[]).is_err());
        assert!(normalize_delivered_notification_identifiers(&[" ".to_string()]).is_err());
        assert!(
            normalize_delivered_notification_identifiers(&["other-notification".to_string(),])
                .is_err()
        );
    }
}
