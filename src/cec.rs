//! HDMI-CEC control, for displays that can be switched on and off over the
//! HDMI cable itself.
//!
//! This exists because the TV on this desk answers no DDC/CI at all — not
//! power, not brightness, not even input select — while speaking CEC perfectly
//! well. That split is normal for televisions: they implement CEC, and the
//! MCCS command set that computer monitors use is simply absent.
//!
//! Everything here is best-effort by design. A profile's displays must switch
//! whether or not an adapter is plugged in, so every function returns its
//! failure as a message for the caller to report rather than propagating an
//! error that would abort a topology change that already succeeded.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use libcec::{
    enums::DeviceType, enums::LogicalAddress, enums::PowerStatus, Connection, ConnectionBuilder,
};

/// CEC OSD names are capped at 13 characters by the spec, so this is not
/// "monitor-switcher". It shows up on the TV's own device list.
const OSD_NAME: &str = "monitor-sw";

/// How long to wait for the TV to finish an in-progress power transition before
/// issuing a new command. Asking a TV to wake while it is still going to sleep
/// is ignored, which is the failure mode this avoids.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(12);

/// How long to wait for the TV to report itself on after being told to wake.
const POWER_ON_TIMEOUT: Duration = Duration::from_secs(15);

const POLL_INTERVAL: Duration = Duration::from_millis(400);

pub struct Cec {
    connection: Connection,
}

impl Cec {
    /// Open the first adapter found. `hdmi_port` is the numbered input on the
    /// TV that the adapter's signal arrives at — it determines the physical
    /// address we claim, and therefore whether taking over the input works.
    pub fn open(hdmi_port: u8) -> Result<Self> {
        let connection = ConnectionBuilder::new(OSD_NAME)
            .device_type(DeviceType::PlaybackDevice)
            .hdmi_port(hdmi_port)
            // Don't grab the TV's input merely by connecting; that is a
            // separate, explicit step.
            .activate_source(false)
            .open_first()
            .context("no CEC adapter found (is the Pulse-Eight adapter plugged in?)")?;
        Ok(Self { connection })
    }

    pub fn power_status(&self) -> PowerStatus {
        self.connection.power_status(LogicalAddress::Tv)
    }

    /// Wake the TV and take over its input.
    ///
    /// Returns a description of what happened, for the caller to print.
    pub fn wake(&self, claim_input: bool) -> Result<String> {
        // If the TV is still shutting down, waiting is the whole trick: a wake
        // sent during the on-to-standby transition is silently dropped.
        self.settle()?;

        if self.power_status() == PowerStatus::On {
            if claim_input {
                self.claim_input()?;
                return Ok("TV already on, input claimed".into());
            }
            return Ok("TV already on".into());
        }

        self.connection
            .power_on(LogicalAddress::Tv)
            .context("sending CEC power-on")?;

        let woke = self.wait_for(POWER_ON_TIMEOUT, |s| s == PowerStatus::On);

        if claim_input {
            self.claim_input()?;
        }

        Ok(if woke {
            if claim_input {
                "TV woken, input claimed".into()
            } else {
                "TV woken".into()
            }
        } else {
            // The command was accepted; the TV just hasn't confirmed yet. Many
            // sets report late, so this is a note rather than a failure.
            format!(
                "TV did not confirm power-on within {}s (status: {:?})",
                POWER_ON_TIMEOUT.as_secs(),
                self.power_status()
            )
        })
    }

    /// Put the TV into standby.
    pub fn sleep(&self) -> Result<String> {
        if matches!(
            self.power_status(),
            PowerStatus::Standby | PowerStatus::InTransitionOnToStandby
        ) {
            return Ok("TV already in standby".into());
        }
        self.connection
            .standby(LogicalAddress::Tv)
            .context("sending CEC standby")?;
        Ok("TV sent to standby".into())
    }

    /// Announce ourselves as the active source so the TV shows this PC rather
    /// than whatever else is plugged into it.
    fn claim_input(&self) -> Result<()> {
        self.connection
            .set_active_source(DeviceType::PlaybackDevice)
            .context("claiming the TV's input as active source")
    }

    /// Wait out any in-progress power transition.
    fn settle(&self) -> Result<()> {
        self.wait_for(SETTLE_TIMEOUT, |s| {
            !matches!(
                s,
                PowerStatus::InTransitionOnToStandby | PowerStatus::InTransitionStandbyToOn
            )
        });
        Ok(())
    }

    /// Poll power status until the predicate holds. Returns whether it did
    /// before the timeout.
    fn wait_for(&self, timeout: Duration, done: impl Fn(PowerStatus) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if done(self.power_status()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}
