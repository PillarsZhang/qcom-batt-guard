use anyhow::{Context, Result};
use clap::Parser;
use env_logger::Env;
use log::{debug, info, warn};
use std::fs;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChargeState {
    Offline,
    Stop,
    Limit,
    Fast,
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
    usb_online_path: String,

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

fn next_state(
    state: ChargeState,
    usb_online: bool,
    soc: i64,
    soc_stop: i64,
    soc_limit: i64,
    soc_fast: i64,
) -> ChargeState {
    if !usb_online {
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
    let sleep = Duration::from_millis(args.interval_ms);
    info!("init state: {:?} sleep={:?}", state, sleep);

    loop {
        let online_raw = read_i64(&args.usb_online_path).unwrap_or(0);
        let usb_online = online_raw == 1;

        let soc = match read_i64(&args.soc_path) {
            Ok(v) => v,
            Err(e) => {
                warn!("failed to read soc: {e:#}");
                thread::sleep(sleep);
                continue;
            }
        };

        debug!("usb_online={} soc={}", online_raw, soc);

        let new_state = next_state(
            state,
            usb_online,
            soc,
            args.soc_stop,
            args.soc_limit,
            args.soc_fast,
        );

        let old_state = state;
        if new_state != old_state {
            state = new_state;
        }

        let target = target_icl_ua(state, &args);

        if state != old_state {
            info!(
                "state change: {:?} -> {:?} (usb_online={} soc={}) target_icl_ua={:?}",
                old_state, state, online_raw, soc, target
            );
        } else {
            debug!(
                "state not change: {:?} (usb_online={} soc={}) target_icl_ua={:?}",
                state, online_raw, soc, target
            );
        }

        let icl_ua = match target {
            Some(v) => v,
            None => {
                thread::sleep(sleep);
                continue;
            }
        };

        debug!("writing icl_ua={} to {}", icl_ua, args.icl_path);

        if let Err(e) = write_i64(&args.icl_path, icl_ua) {
            warn!("failed to write icl: {e:#}");
        }

        thread::sleep(sleep);
    }
}
