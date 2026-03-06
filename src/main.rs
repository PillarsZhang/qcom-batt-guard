use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use env_logger::Env;
use log::{debug, info, warn};
use std::fs;
#[cfg(feature = "udev")]
use std::os::fd::AsRawFd;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChargeState {
    Offline,
    Stop,
    Limit,
    Fast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MonitorMode {
    Poll,
    Udev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Snapshot {
    online: bool,
    soc: i64,
}

trait SnapshotSource {
    fn next_snapshot(&mut self, args: &Args) -> Result<Snapshot>;
}

struct PollMonitor {
    sleep: Duration,
}

#[cfg(feature = "udev")]
struct UdevMonitor {
    socket: udev::MonitorSocket,
}

#[derive(Parser, Debug)]
#[command(
    name = "qcom-batt-guard",
    about = "Guard Qualcomm battery SOC by controlling USB input_current_limit (ICL)"
)]
struct Args {
    /// Path to battery state-of-charge (SOC) sysfs node, usually an integer percentage.
    #[arg(long, default_value = "/sys/class/power_supply/qcom-battery/capacity")]
    soc_path: String,

    /// Path to USB online sysfs node. Expected values are usually 0 or 1.
    #[arg(
        long,
        default_value = "/sys/class/power_supply/qcom-smbcharger-usb/online"
    )]
    online_path: String,

    /// Path to USB input current limit sysfs node, in microamps (uA).
    #[arg(
        long,
        default_value = "/sys/class/power_supply/qcom-smbcharger-usb/input_current_limit"
    )]
    icl_path: String,

    /// Enter Stop state when SOC is greater than or equal to this percentage.
    #[arg(long, default_value_t = 60)]
    soc_stop: i64,

    /// Enter Fast state when SOC is less than or equal to this percentage.
    #[arg(long, default_value_t = 50)]
    soc_fast: i64,

    /// Middle threshold for Limit state.
    ///
    /// Stop exits to Limit when SOC falls to or below this value.
    /// Fast exits to Limit when SOC rises to or above this value.
    #[arg(long, default_value_t = 55)]
    soc_limit: i64,

    /// ICL value for Stop state, in microamps (uA).
    ///
    /// On this device, 0 is used to stop charging.
    #[arg(long, default_value_t = 0)]
    icl_stop_ua: i64,

    /// ICL value for Limit state, in microamps (uA).
    #[arg(long, default_value_t = 550_000)]
    icl_limit_ua: i64,

    /// ICL value for Fast state, in microamps (uA).
    #[arg(long, default_value_t = 3_000_000)]
    icl_fast_ua: i64,

    /// Main loop interval, in milliseconds.
    #[arg(long, default_value_t = 10_000)]
    interval_ms: u64,

    /// Monitor mode: poll sysfs periodically or listen for udev power_supply events.
    #[arg(long, value_enum, default_value_t = MonitorMode::Udev)]
    mode: MonitorMode,
}

fn read_i64(path: &str) -> Result<i64> {
    let s = fs::read_to_string(path).with_context(|| format!("read {}", path))?;
    let v = s
        .trim()
        .parse::<i64>()
        .with_context(|| format!("parse int from {}", path))?;
    Ok(v)
}

fn write_i64(path: &str, value: i64) -> Result<()> {
    fs::write(path, format!("{value}\n")).with_context(|| format!("write {} = {}", path, value))?;
    Ok(())
}

fn read_snapshot(args: &Args) -> Result<Snapshot> {
    let online_raw = match read_i64(&args.online_path) {
        Ok(v) => v,
        Err(e) => {
            warn!("failed to read online: {e:#}");
            0
        }
    };
    let soc = read_i64(&args.soc_path).context("read snapshot soc")?;
    Ok(Snapshot {
        online: online_raw == 1,
        soc,
    })
}

fn next_state(
    state: ChargeState,
    online: bool,
    soc: i64,
    soc_stop: i64,
    soc_limit: i64,
    soc_fast: i64,
) -> ChargeState {
    if !online {
        return ChargeState::Offline;
    }

    match state {
        ChargeState::Offline => {
            if soc >= soc_stop {
                ChargeState::Stop
            } else if soc <= soc_fast {
                ChargeState::Fast
            } else {
                ChargeState::Limit
            }
        }

        ChargeState::Stop => {
            if soc <= soc_limit {
                ChargeState::Limit
            } else {
                ChargeState::Stop
            }
        }

        ChargeState::Fast => {
            if soc >= soc_limit {
                ChargeState::Limit
            } else {
                ChargeState::Fast
            }
        }

        ChargeState::Limit => {
            if soc >= soc_stop {
                ChargeState::Stop
            } else if soc <= soc_fast {
                ChargeState::Fast
            } else {
                ChargeState::Limit
            }
        }
    }
}

fn target_icl_ua(state: ChargeState, a: &Args) -> Option<i64> {
    match state {
        ChargeState::Offline => None,
        ChargeState::Stop => Some(a.icl_stop_ua),
        ChargeState::Limit => Some(a.icl_limit_ua),
        ChargeState::Fast => Some(a.icl_fast_ua),
    }
}

fn control_step(state: &mut ChargeState, snapshot: Snapshot, args: &Args) {
    let new_state = next_state(
        *state,
        snapshot.online,
        snapshot.soc,
        args.soc_stop,
        args.soc_limit,
        args.soc_fast,
    );

    let old_state = *state;
    if new_state != old_state {
        *state = new_state;
    }

    let target = target_icl_ua(*state, args);

    if *state != old_state {
        info!(
            "state change: {:?} -> {:?} (online={} soc={}) target_icl_ua={:?}",
            old_state, *state, snapshot.online as i32, snapshot.soc, target
        );
    } else {
        debug!(
            "state not change: {:?} (online={} soc={}) target_icl_ua={:?}",
            *state, snapshot.online as i32, snapshot.soc, target
        );
    }

    let Some(icl_ua) = target else {
        return;
    };

    debug!("writing icl_ua={} to {}", icl_ua, args.icl_path);
    if let Err(e) = write_i64(&args.icl_path, icl_ua) {
        warn!("failed to write icl: {e:#}");
    }
}

impl SnapshotSource for PollMonitor {
    fn next_snapshot(&mut self, args: &Args) -> Result<Snapshot> {
        loop {
            thread::sleep(self.sleep);
            debug!("poll tick after sleep {:?}", self.sleep);

            match read_snapshot(args) {
                Ok(snapshot) => return Ok(snapshot),
                Err(e) => {
                    warn!("failed to read snapshot: {e:#}");
                }
            }
        }
    }
}

#[cfg(feature = "udev")]
impl SnapshotSource for UdevMonitor {
    fn next_snapshot(&mut self, args: &Args) -> Result<Snapshot> {
        loop {
            let mut poll_fd = libc::pollfd {
                fd: self.socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll_fd, 1, -1) };
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err).context("poll udev monitor");
            }

            let Some(event) = self.socket.iter().next() else {
                anyhow::bail!("udev monitor terminated unexpectedly");
            };

            debug!(
                "udev event seq={} type={:?} subsystem={} sysname={} devpath={}",
                event.sequence_number(),
                event.event_type(),
                event
                    .subsystem()
                    .map(|v| v.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "<none>".to_owned()),
                event.sysname().to_string_lossy(),
                event.devpath().to_string_lossy()
            );

            if event.event_type() != udev::EventType::Change {
                debug!("udev event ignored: not a change event");
                continue;
            }

            match read_snapshot(args) {
                Ok(snapshot) => return Ok(snapshot),
                Err(e) => {
                    warn!("failed to read snapshot after udev event: {e:#}");
                }
            }
        }
    }
}

fn build_snapshot_source(args: &Args) -> Result<Box<dyn SnapshotSource>> {
    match args.mode {
        MonitorMode::Poll => {
            let sleep = Duration::from_millis(args.interval_ms);
            info!("monitor mode: poll interval={:?}", sleep);
            Ok(Box::new(PollMonitor { sleep }))
        }
        MonitorMode::Udev => {
            info!("monitor mode: udev");
            #[cfg(feature = "udev")]
            {
                let socket = udev::MonitorBuilder::new()
                    .context("create udev monitor")?
                    .match_subsystem("power_supply")
                    .context("filter udev subsystem")?
                    .listen()
                    .context("listen udev monitor")?;
                return Ok(Box::new(UdevMonitor { socket }));
            }

            #[cfg(not(feature = "udev"))]
            {
                anyhow::bail!("udev mode requires building with --features udev");
            }
        }
    }
}

fn ensure_root() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        anyhow::bail!("must run as root (try: sudo ...)");
    }
    Ok(())
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    ensure_root()?;

    if !(args.soc_fast < args.soc_limit && args.soc_limit < args.soc_stop) {
        anyhow::bail!(
            "invalid thresholds: require soc_fast ({}) < soc_limit ({}) < soc_stop ({})",
            args.soc_fast,
            args.soc_limit,
            args.soc_stop
        );
    }

    info!(
        "thresholds: fast<= {} limit={} stop>= {}",
        args.soc_fast, args.soc_limit, args.soc_stop
    );

    let mut state = ChargeState::Offline;
    let mut source = build_snapshot_source(&args)?;
    info!("init state: {:?}", state);

    let initial_snapshot = read_snapshot(&args).context("read initial snapshot")?;
    control_step(&mut state, initial_snapshot, &args);
    let mut last_snapshot = Some(initial_snapshot);

    loop {
        let snapshot = source.next_snapshot(&args)?;
        debug!(
            "snapshot received online={} soc={}",
            snapshot.online, snapshot.soc
        );

        if last_snapshot != Some(snapshot) {
            debug!("snapshot changed");
            control_step(&mut state, snapshot, &args);
            last_snapshot = Some(snapshot);
        } else {
            debug!("snapshot unchanged");
        }
    }
}
