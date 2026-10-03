//! Shared application state, refreshed by a background poll of
//! `vexos-vpn status --json` (2 s while the window is visible, 10 s otherwise).

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch, Notify, RwLock};
use tracing::{debug, warn};
use vex_vpn::cli;
use vex_vpn::vexos::{Rates, Region, Sample, Status, VpnState};

const POLL_VISIBLE: Duration = Duration::from_secs(2);
const POLL_HIDDEN: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct AppState {
    /// Path of `vexos-vpn`; `None` when the backend is not installed.
    pub backend: Option<PathBuf>,
    pub status: Option<Status>,
    /// `vexos-vpn.service` is running or (re)starting.
    pub unit_active: bool,
    /// Last failure reading the status, if the most recent poll failed.
    pub poll_error: Option<String>,
    pub rates: Rates,
    /// `None` until first loaded; `Err` holds the CLI message (e.g. no
    /// cached server list yet).
    pub regions: Option<Result<Vec<Region>, String>>,
    /// Bumped whenever `regions` changes, so views can skip rebuilds.
    pub regions_version: u64,
}

impl AppState {
    /// A status is available and the user is signed out (or the backend
    /// reported missing credentials).
    pub fn needs_sign_in(&self) -> bool {
        self.status.as_ref().is_some_and(Status::needs_sign_in)
    }
}

/// Handles shared by the poll loop, the window and the tray.
#[derive(Clone)]
pub struct Ctx {
    pub state: Arc<RwLock<AppState>>,
    /// Fired after every poll.
    pub changed: broadcast::Sender<()>,
    poke: Arc<Notify>,
    regions_stale: Arc<AtomicBool>,
    visible: Arc<watch::Sender<bool>>,
    rt: tokio::runtime::Handle,
}

impl Ctx {
    pub fn new(rt: tokio::runtime::Handle) -> Self {
        Self {
            state: Arc::default(),
            changed: broadcast::channel(16).0,
            poke: Arc::new(Notify::new()),
            regions_stale: Arc::default(),
            visible: Arc::new(watch::channel(false).0),
            rt,
        }
    }

    /// Re-poll now (after an action) instead of waiting for the interval.
    pub fn poke(&self) {
        self.poke.notify_one();
    }

    /// Reload the region list on the next poll.
    pub fn reload_regions(&self) {
        self.regions_stale.store(true, Ordering::SeqCst);
        self.poke();
    }

    pub fn set_window_visible(&self, visible: bool) {
        self.visible.send_replace(visible);
    }

    pub async fn snapshot(&self) -> AppState {
        self.state.read().await.clone()
    }

    /// Run `fut` on the tokio runtime and await its result from any executor
    /// (used from GTK's main loop). Panics only if the task itself panicked.
    pub fn run<F>(&self, fut: F) -> impl Future<Output = F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let handle = self.rt.spawn(fut);
        async move { handle.await.expect("background task panicked") }
    }

    pub fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.rt.spawn(fut);
    }
}

// ---------------------------------------------------------------------------
// Poll loop
// ---------------------------------------------------------------------------

pub async fn poll_loop(ctx: Ctx) {
    let mut visible = ctx.visible.subscribe();
    let mut prev_sample: Option<(Sample, Instant)> = None;
    let mut prev_state: Option<VpnState> = None;
    let mut notified_error = String::new();

    loop {
        let backend = cli::find_backend();
        let mut new = AppState {
            backend: backend.clone(),
            ..Default::default()
        };

        if let Some(cli_path) = &backend {
            let (status, unit_active) = tokio::join!(
                cli::status(cli_path),
                crate::dbus::is_unit_active(crate::dbus::VPN_UNIT)
            );
            new.unit_active = unit_active.unwrap_or_else(|e| {
                debug!("vexos-vpn.service state unavailable: {e}");
                false
            });
            match status {
                Ok(status) => {
                    let now = Instant::now();
                    let sample = Sample {
                        interface: status.interface.clone(),
                        rx_bytes: status.rx_bytes,
                        tx_bytes: status.tx_bytes,
                    };
                    if let Some((prev, at)) = &prev_sample {
                        new.rates = Rates::between(prev, &sample, (now - *at).as_secs_f64());
                    }
                    prev_sample = Some((sample, now));
                    new.status = Some(status);
                }
                Err(e) => {
                    warn!("status poll failed: {e:#}");
                    new.poll_error = Some(format!("{e:#}"));
                }
            }
        }

        let state_now = new.status.as_ref().map(|s| s.state);
        let became_connected =
            state_now == Some(VpnState::Connected) && prev_state != Some(VpnState::Connected);

        // Region latency is measured during an automatic connect, so reload
        // the list on each new connection as well as on first use.
        {
            let s = ctx.state.read().await;
            new.regions = s.regions.clone();
            new.regions_version = s.regions_version;
        }
        if let Some(cli_path) = &backend {
            let stale = ctx.regions_stale.swap(false, Ordering::SeqCst);
            if new.regions.is_none() || became_connected || stale {
                new.regions = Some(cli::regions(cli_path).await.map_err(|e| format!("{e:#}")));
                new.regions_version += 1;
            }
        }

        if let (Some(prev), Some(status)) = (prev_state, &new.status) {
            notify_transition(prev, status, &mut notified_error);
        }
        if state_now == Some(VpnState::Connected) {
            notified_error.clear();
        }
        prev_state = state_now;

        *ctx.state.write().await = new;
        let _ = ctx.changed.send(());

        let interval = if *visible.borrow_and_update() {
            POLL_VISIBLE
        } else {
            POLL_HIDDEN
        };
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = ctx.poke.notified() => {}
            _ = visible.changed() => {}
        }
    }
}

/// Desktop notification on state changes. While connecting fails, the
/// service retries every 10 s; each distinct error is announced once.
fn notify_transition(prev: VpnState, status: &Status, notified_error: &mut String) {
    let (summary, body, icon) = match status.state {
        VpnState::Connected if prev != VpnState::Connected => (
            "VPN connected",
            format!(
                "{} via {}",
                status.region_name,
                vex_vpn::vexos::protocol_label(&status.protocol)
            ),
            "network-vpn-symbolic",
        ),
        VpnState::Disconnected if prev == VpnState::Connected => (
            "VPN disconnected",
            String::new(),
            "network-vpn-disabled-symbolic",
        ),
        VpnState::Error if status.last_error != *notified_error => {
            notified_error.clone_from(&status.last_error);
            (
                "VPN error",
                status.last_error.clone(),
                "network-vpn-no-route-symbolic",
            )
        }
        _ => return,
    };
    tokio::task::spawn_blocking(move || {
        if let Err(e) = notify_rust::Notification::new()
            .appname("vex-vpn")
            .summary(summary)
            .body(&body)
            .icon(icon)
            .show()
        {
            warn!("desktop notification failed: {e}");
        }
    });
}
