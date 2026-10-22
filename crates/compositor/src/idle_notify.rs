//! ext-idle-notify-v1: a client learns when the user has been idle for a while (a screen locker,
//! a chat app's "away"). Our own implementation: Smithay's needs the event loop's data to be
//! `State`. Only clients outside a sandbox see it.
//!
//! The idle time is the one `idle.rs` counts, so an idle inhibitor holds `idled` back, except for
//! `get_input_idle_notification`, which counts input alone.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use smithay::reexports::wayland_protocols::ext::idle_notify::v1::server::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use smithay::reexports::wayland_protocols::ext::idle_notify::v1::server::ext_idle_notifier_v1::{
    self, ExtIdleNotifierV1,
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::state::State;

pub struct NotificationData {
    timeout: Duration,
    /// From `get_input_idle_notification`: idle inhibitors do not count.
    input_only: bool,
    idled: AtomicBool,
}

/// The live notifications.
#[derive(Default)]
pub struct IdleNotifications {
    list: Vec<ExtIdleNotificationV1>,
}

impl IdleNotifications {
    pub fn new(dh: &DisplayHandle) -> Self {
        dh.create_global::<State, ExtIdleNotifierV1, _>(2, ());
        Self::default()
    }
}

impl State {
    /// After input and on every idle check: `idled` once a notification's time has passed,
    /// `resumed` when activity comes back.
    pub fn idle_notify(&mut self) {
        let (activity, input) = (self.last_activity, self.last_input);
        let list = &mut self.idle_notifications.list;
        list.retain(Resource::is_alive);
        for notification in list.iter() {
            let Some(data) = notification.data::<NotificationData>() else {
                continue;
            };
            let since = if data.input_only { input } else { activity };
            let idle = since.elapsed() >= data.timeout;
            if idle != data.idled.swap(idle, Ordering::Relaxed) {
                if idle {
                    notification.idled();
                } else {
                    notification.resumed();
                }
            }
        }
    }
}

impl GlobalDispatch<ExtIdleNotifierV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ExtIdleNotifierV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _global_data: &()) -> bool {
        crate::security::unsandboxed(&client)
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _notifier: &ExtIdleNotifierV1,
        request: ext_idle_notifier_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let (id, timeout, input_only) = match request {
            ext_idle_notifier_v1::Request::GetIdleNotification { id, timeout, .. } => {
                (id, timeout, false)
            }
            ext_idle_notifier_v1::Request::GetInputIdleNotification { id, timeout, .. } => {
                (id, timeout, true)
            }
            _ => return,
        };
        let notification = data_init.init(
            id,
            NotificationData {
                timeout: Duration::from_millis(timeout.into()),
                input_only,
                idled: AtomicBool::new(false),
            },
        );
        state.idle_notifications.list.retain(Resource::is_alive);
        state.idle_notifications.list.push(notification);
    }
}

impl Dispatch<ExtIdleNotificationV1, NotificationData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        notification: &ExtIdleNotificationV1,
        request: ext_idle_notification_v1::Request,
        _data: &NotificationData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_idle_notification_v1::Request::Destroy = request {
            state.idle_notifications.list.retain(|n| n != notification);
        }
    }
}
