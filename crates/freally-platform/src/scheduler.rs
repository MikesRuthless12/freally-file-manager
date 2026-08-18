//! FFM-M17 — user-level OS scheduler installation.
//!
//! Phase 14d rendered scheduler stanzas for the user to paste in by
//! hand; this module *installs* them. Three backends, all per-user
//! and none requiring elevation:
//!
//! - **Windows** — a Task Scheduler task under the `\Freally\` folder,
//!   created from generated Task Scheduler XML via
//!   `schtasks /Create /XML`. XML (rather than the `/SC` flag set)
//!   because the missed-run policy is `<StartWhenAvailable>`, which
//!   the flag form cannot express.
//! - **macOS** — a `~/Library/LaunchAgents/dev.freally.job.<id>.plist`
//!   loaded with `launchctl load -w`.
//! - **Linux** — a `freally-<id>.service` + `freally-<id>.timer` pair
//!   under `~/.config/systemd/user/`, enabled with
//!   `systemctl --user enable --now`.
//!
//! ## Missed-run policy
//!
//! [`MissedRunPolicy::RunWhenAvailable`] maps to `<StartWhenAvailable>`
//! on Windows and `Persistent=true` on the systemd timer. launchd has
//! no equivalent knob because the behaviour is unconditional: a
//! `StartCalendarInterval` job whose window passed while the machine
//! was asleep runs once at wake. macOS therefore reports
//! `RunWhenAvailable` semantics regardless of the requested policy,
//! and [`policy_is_honored`] says so rather than letting the UI claim
//! a guarantee the platform does not make.
//!
//! ## Threat model
//!
//! A scheduled task is a command line the OS runs unattended, so every
//! field that reaches a stanza is validated before it is written:
//!
//! - The **id** is `[a-z0-9-]{1,48}` ([`validate_id`]) — it becomes a
//!   filename and a task-path component, so no separators, no dots, no
//!   Unicode.
//! - The **label** is human text; control characters are rejected so it
//!   cannot forge extra lines in a systemd unit or plist.
//! - **Program + args** are XML-escaped for the Windows/launchd stanzas
//!   and shell-quoted for the systemd `ExecStart=`. A control character
//!   anywhere in them is rejected outright rather than escaped — there
//!   is no legitimate scheduled command containing a newline.
//!
//! Rendering is separated from installation so the exact bytes that
//! reach disk are unit-testable on every host, not just the one whose
//! scheduler is present.

use std::path::PathBuf;

/// When the scheduled job fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleTrigger {
    /// Every hour, at `minute` past.
    Hourly {
        /// Minutes past the hour, 0-59.
        minute: u32,
    },
    /// Every day at `hour`:`minute` local time.
    Daily {
        /// Hour of the day, 0-23.
        hour: u32,
        /// Minutes past the hour, 0-59.
        minute: u32,
    },
    /// Weekly on `weekday` at `hour`:`minute` local time.
    Weekly {
        /// Day of the week, 0 = Sunday … 6 = Saturday.
        weekday: u32,
        /// Hour of the day, 0-23.
        hour: u32,
        /// Minutes past the hour, 0-59.
        minute: u32,
    },
}

impl ScheduleTrigger {
    fn validate(&self) -> Result<(), SchedulerError> {
        let (h, m, wd) = match *self {
            Self::Hourly { minute } => (0, minute, 0),
            Self::Daily { hour, minute } => (hour, minute, 0),
            Self::Weekly {
                weekday,
                hour,
                minute,
            } => (hour, minute, weekday),
        };
        if h > 23 || m > 59 || wd > 6 {
            return Err(SchedulerError::InvalidTrigger);
        }
        Ok(())
    }
}

/// What the scheduler should do about a run whose window elapsed while
/// the machine was off or asleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissedRunPolicy {
    /// Wait for the next scheduled window.
    Skip,
    /// Run once, as soon as the machine is available again.
    RunWhenAvailable,
}

/// Whether the host scheduler can actually honor `policy`.
///
/// launchd ignores the distinction — a missed `StartCalendarInterval`
/// always runs at wake — so on macOS only
/// [`MissedRunPolicy::RunWhenAvailable`] is truthful. Callers surface
/// this instead of showing a control that silently does nothing.
pub const fn policy_is_honored(policy: MissedRunPolicy) -> bool {
    if cfg!(target_os = "macos") {
        matches!(policy, MissedRunPolicy::RunWhenAvailable)
    } else {
        true
    }
}

/// One installable scheduled run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledJob {
    /// Stable slug — filename + task-path component. See [`validate_id`].
    pub id: String,
    /// Human label shown in the OS scheduler UI.
    pub label: String,
    /// Absolute path to the executable to run.
    pub program: PathBuf,
    /// Arguments passed to `program`, unquoted.
    pub args: Vec<String>,
    /// When the job fires.
    pub trigger: ScheduleTrigger,
    /// What to do about a firing missed while the machine was off.
    pub missed_run: MissedRunPolicy,
}

/// Why installing, removing, or querying a schedule failed.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    /// The id is not `[a-z0-9-]{1,48}`. See [`validate_id`].
    #[error("schedule id must be 1-48 chars of [a-z0-9-]")]
    InvalidId,
    /// The human label carries a control character.
    #[error("schedule label must not contain control characters")]
    InvalidLabel,
    /// Hour, minute, or weekday is out of range.
    #[error("schedule trigger is out of range")]
    InvalidTrigger,
    /// The program path is relative, empty, or an argument carries a
    /// control character.
    #[error("scheduled command must be an absolute path free of control characters")]
    InvalidCommand,
    /// This target has no per-user scheduler to install into.
    #[error("no user scheduler is available on this platform")]
    Unsupported,
    /// `$HOME` is unset or empty, so the per-user stanza has nowhere
    /// to live.
    #[error("could not locate the user's home directory")]
    NoHomeDir,
    /// Reading or writing a stanza failed.
    #[error("scheduler I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// The host scheduler's CLI exited non-zero.
    #[error("{command} failed (exit {code}): {stderr}")]
    CommandFailed {
        /// The program that was invoked (`schtasks` / `launchctl` /
        /// `systemctl`).
        command: String,
        /// Its exit code, or `-1` when the process was signalled.
        code: i32,
        /// Whatever it wrote to stderr, trimmed.
        stderr: String,
    },
}

/// Accept only `[a-z0-9-]{1,48}`.
///
/// The id becomes a filename (`freally-<id>.timer`) and a Task
/// Scheduler path component (`\Freally\<id>`), so anything that could
/// traverse, shell-quote, or collide case-insensitively is refused
/// here rather than escaped at each of the three render sites.
pub fn validate_id(id: &str) -> Result<(), SchedulerError> {
    if id.is_empty() || id.len() > 48 {
        return Err(SchedulerError::InvalidId);
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(SchedulerError::InvalidId);
    }
    Ok(())
}

fn validate_job(job: &ScheduledJob) -> Result<(), SchedulerError> {
    validate_id(&job.id)?;
    if job.label.chars().any(char::is_control) {
        return Err(SchedulerError::InvalidLabel);
    }
    job.trigger.validate()?;
    let program = job.program.to_string_lossy();
    if program.is_empty() || !job.program.is_absolute() || program.chars().any(char::is_control) {
        return Err(SchedulerError::InvalidCommand);
    }
    if job
        .args
        .iter()
        .any(|a| a.chars().any(char::is_control) || a.contains('\u{FFFD}'))
    {
        return Err(SchedulerError::InvalidCommand);
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Next-run preview
// ---------------------------------------------------------------------

/// Seconds-since-epoch of the first firing strictly after
/// `now_local_secs`.
///
/// Both the input and the result are *local* wall-clock seconds — the
/// caller adds its UTC offset going in and subtracts it coming out.
/// Keeping the arithmetic offset-free makes it a pure function with no
/// timezone database, so the whole trigger matrix is unit-testable on
/// any host.
///
/// DST is deliberately not modelled: the OS schedulers themselves fire
/// on local wall-clock, so a "03:00 daily" job runs at 03:00 before and
/// after a transition. Predicting the one duplicated or skipped hour a
/// year would make this preview disagree with the scheduler that
/// actually runs the job.
pub fn next_run_after(trigger: ScheduleTrigger, now_local_secs: i64) -> i64 {
    const DAY: i64 = 86_400;
    match trigger {
        ScheduleTrigger::Hourly { minute } => {
            let hour_start = now_local_secs - now_local_secs.rem_euclid(3_600);
            let mut cand = hour_start + i64::from(minute) * 60;
            if cand <= now_local_secs {
                cand += 3_600;
            }
            cand
        }
        ScheduleTrigger::Daily { hour, minute } => {
            let day_start = now_local_secs - now_local_secs.rem_euclid(DAY);
            let mut cand = day_start + i64::from(hour) * 3_600 + i64::from(minute) * 60;
            if cand <= now_local_secs {
                cand += DAY;
            }
            cand
        }
        ScheduleTrigger::Weekly {
            weekday,
            hour,
            minute,
        } => {
            let days = now_local_secs.div_euclid(DAY);
            // 1970-01-01 was a Thursday, i.e. weekday 4 with Sunday=0.
            let today_wd = (days + 4).rem_euclid(7);
            let delta = (i64::from(weekday) - today_wd).rem_euclid(7);
            let mut cand = (days + delta) * DAY + i64::from(hour) * 3_600 + i64::from(minute) * 60;
            if cand <= now_local_secs {
                cand += 7 * DAY;
            }
            cand
        }
    }
}

// ---------------------------------------------------------------------
// Escaping
// ---------------------------------------------------------------------

/// Escape for XML `#PCDATA`. Covers all five XML 1.0 predefined
/// entities, so a path containing `<`, `&`, or a quote produces a
/// well-formed document rather than one that breaks parsing — or, worse,
/// injects an element that overrides the surrounding stanza.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// Single-quote for a systemd `ExecStart=` word, with the
/// POSIX-canonical `'\''` escape for embedded single quotes.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// See [`xml_escape`].
///
/// Crate-visible alias so `autostart` shares this rather than keeping
/// its own copy, which had already drifted: its Run-key quoting doubled
/// quotes but not trailing backslashes.
pub(crate) fn xml_escape_pub(s: &str) -> String {
    xml_escape(s)
}

/// See [`win_quote`].
pub(crate) fn win_quote_pub(s: &str) -> String {
    win_quote(s)
}

/// Quote one `cmd.exe` argument for the Task Scheduler `<Arguments>`
/// element (which is a raw command tail, not an argv array). Wraps in
/// double quotes and doubles any embedded ones.
fn win_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in s.chars() {
        match ch {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                // A run of backslashes before a quote must be doubled,
                // then the quote itself doubled for cmd.
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push_str("\"\"");
            }
            other => {
                backslashes = 0;
                out.push(other);
            }
        }
    }
    // Trailing backslashes would otherwise escape the closing quote and
    // let this argument absorb the next one. `D:\` is an entirely
    // ordinary destination, so this is reachable without malice.
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------
// Rendering — pure, so the exact on-disk bytes are testable anywhere
// ---------------------------------------------------------------------

/// Task Scheduler XML for `job`.
///
/// `<LogonTrigger>`-free: every trigger here is calendar-based. The
/// task runs in the interactive-token context of whoever installs it
/// (`<LogonType>InteractiveToken</LogonType>`), which is what keeps
/// this a per-user, no-elevation feature.
pub fn render_windows_task_xml(job: &ScheduledJob) -> String {
    let trigger = match job.trigger {
        // Task Scheduler expresses "every hour" as a daily trigger with
        // a 1-hour repetition covering the whole day.
        ScheduleTrigger::Hourly { minute } => format!(
            "    <CalendarTrigger>\n      <StartBoundary>2026-01-01T00:{minute:02}:00</StartBoundary>\n      <Repetition>\n        <Interval>PT1H</Interval>\n        <StopAtDurationEnd>false</StopAtDurationEnd>\n      </Repetition>\n      <ScheduleByDay>\n        <DaysInterval>1</DaysInterval>\n      </ScheduleByDay>\n      <Enabled>true</Enabled>\n    </CalendarTrigger>"
        ),
        ScheduleTrigger::Daily { hour, minute } => format!(
            "    <CalendarTrigger>\n      <StartBoundary>2026-01-01T{hour:02}:{minute:02}:00</StartBoundary>\n      <ScheduleByDay>\n        <DaysInterval>1</DaysInterval>\n      </ScheduleByDay>\n      <Enabled>true</Enabled>\n    </CalendarTrigger>"
        ),
        ScheduleTrigger::Weekly {
            weekday,
            hour,
            minute,
        } => format!(
            "    <CalendarTrigger>\n      <StartBoundary>2026-01-01T{hour:02}:{minute:02}:00</StartBoundary>\n      <ScheduleByWeek>\n        <WeeksInterval>1</WeeksInterval>\n        <DaysOfWeek>\n          <{day}/>\n        </DaysOfWeek>\n      </ScheduleByWeek>\n      <Enabled>true</Enabled>\n    </CalendarTrigger>",
            day = windows_weekday(weekday),
        ),
    };
    let start_when_available = matches!(job.missed_run, MissedRunPolicy::RunWhenAvailable);
    let args = job
        .args
        .iter()
        .map(|a| win_quote(a))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         \x20 <RegistrationInfo>\n\
         \x20   <Description>{label}</Description>\n\
         \x20 </RegistrationInfo>\n\
         \x20 <Triggers>\n{trigger}\n  </Triggers>\n\
         \x20 <Principals>\n\
         \x20   <Principal id=\"Author\">\n\
         \x20     <LogonType>InteractiveToken</LogonType>\n\
         \x20     <RunLevel>LeastPrivilege</RunLevel>\n\
         \x20   </Principal>\n\
         \x20 </Principals>\n\
         \x20 <Settings>\n\
         \x20   <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
         \x20   <StartWhenAvailable>{start_when_available}</StartWhenAvailable>\n\
         \x20   <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
         \x20   <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
         \x20   <Enabled>true</Enabled>\n\
         \x20 </Settings>\n\
         \x20 <Actions Context=\"Author\">\n\
         \x20   <Exec>\n\
         \x20     <Command>{program}</Command>\n\
         \x20     <Arguments>{args}</Arguments>\n\
         \x20   </Exec>\n\
         \x20 </Actions>\n\
         </Task>\n",
        label = xml_escape(&job.label),
        program = xml_escape(&job.program.to_string_lossy()),
        args = xml_escape(&args),
    )
}

const fn windows_weekday(weekday: u32) -> &'static str {
    match weekday {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        _ => "Saturday",
    }
}

/// launchd plist for `job`.
pub fn render_launchd_plist(job: &ScheduledJob) -> String {
    let interval = match job.trigger {
        ScheduleTrigger::Hourly { minute } => format!(
            "  <key>StartCalendarInterval</key>\n  <dict>\n    <key>Minute</key><integer>{minute}</integer>\n  </dict>"
        ),
        ScheduleTrigger::Daily { hour, minute } => format!(
            "  <key>StartCalendarInterval</key>\n  <dict>\n    <key>Hour</key><integer>{hour}</integer>\n    <key>Minute</key><integer>{minute}</integer>\n  </dict>"
        ),
        ScheduleTrigger::Weekly {
            weekday,
            hour,
            minute,
        } => format!(
            "  <key>StartCalendarInterval</key>\n  <dict>\n    <key>Weekday</key><integer>{weekday}</integer>\n    <key>Hour</key><integer>{hour}</integer>\n    <key>Minute</key><integer>{minute}</integer>\n  </dict>"
        ),
    };
    let mut argv = String::new();
    argv.push_str(&format!(
        "    <string>{}</string>\n",
        xml_escape(&job.program.to_string_lossy())
    ));
    for a in &job.args {
        argv.push_str(&format!("    <string>{}</string>\n", xml_escape(a)));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key>\n  <string>{label_id}</string>\n\
         \x20 <key>ProgramArguments</key>\n  <array>\n{argv}  </array>\n\
         {interval}\n\
         \x20 <key>RunAtLoad</key>\n  <false/>\n\
         </dict>\n\
         </plist>\n",
        label_id = launchd_label(&job.id),
    )
}

fn launchd_label(id: &str) -> String {
    format!("dev.freally.job.{id}")
}

/// Escape `%` for a systemd unit file.
///
/// A unit file is **not** a shell: `%` introduces a specifier
/// (`%i`, `%h`, `%%`, …) and systemd expands it before anything sees a
/// quote. `shell_quote` wraps a value in single quotes, which stops the
/// *shell* from touching it and does nothing about specifier expansion.
///
/// Without this, a destination like `/mnt/backup 50% full` (or a
/// manifest at `100%.txt`) makes the `.service` fail to load while the
/// `.timer` parses fine and enables — both `systemctl` calls exit 0,
/// install reports success, and every firing silently does nothing,
/// forever.
fn systemd_escape(s: &str) -> String {
    s.replace('%', "%%")
}

/// systemd `.service` unit for `job`.
pub fn render_systemd_service(job: &ScheduledJob) -> String {
    let mut exec = shell_quote(&job.program.to_string_lossy());
    for a in &job.args {
        exec.push(' ');
        exec.push_str(&shell_quote(a));
    }
    format!(
        "[Unit]\nDescription={label}\n\n[Service]\nType=oneshot\nExecStart={exec}\n",
        label = systemd_escape(&job.label),
        exec = systemd_escape(&exec),
    )
}

/// systemd `.timer` unit for `job`.
pub fn render_systemd_timer(job: &ScheduledJob) -> String {
    let on_calendar = match job.trigger {
        ScheduleTrigger::Hourly { minute } => format!("*-*-* *:{minute:02}:00"),
        ScheduleTrigger::Daily { hour, minute } => format!("*-*-* {hour:02}:{minute:02}:00"),
        ScheduleTrigger::Weekly {
            weekday,
            hour,
            minute,
        } => format!(
            "{day} *-*-* {hour:02}:{minute:02}:00",
            day = systemd_weekday(weekday)
        ),
    };
    let persistent = matches!(job.missed_run, MissedRunPolicy::RunWhenAvailable);
    format!(
        "[Unit]\nDescription={label} (timer)\n\n[Timer]\nOnCalendar={on_calendar}\nPersistent={persistent}\nUnit={unit}.service\n\n[Install]\nWantedBy=timers.target\n",
        label = systemd_escape(&job.label),
        unit = systemd_unit_stem(&job.id),
    )
}

const fn systemd_weekday(weekday: u32) -> &'static str {
    match weekday {
        0 => "Sun",
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        _ => "Sat",
    }
}

fn systemd_unit_stem(id: &str) -> String {
    format!("freally-{id}")
}

fn windows_task_name(id: &str) -> String {
    format!("\\Freally\\{id}")
}

// ---------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------

/// Install (or replace) `job` in the host's per-user scheduler.
pub fn install(job: &ScheduledJob) -> Result<(), SchedulerError> {
    validate_job(job)?;
    #[cfg(target_os = "windows")]
    {
        windows_impl::install(job)
    }
    #[cfg(target_os = "macos")]
    {
        macos_impl::install(job)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux_impl::install(job)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = job;
        Err(SchedulerError::Unsupported)
    }
}

/// Remove the scheduled job named `id`. Removing an id that is not
/// installed succeeds — the caller's intent (it should not be there) is
/// already satisfied, and a partially-installed pair must stay
/// removable.
pub fn remove(id: &str) -> Result<(), SchedulerError> {
    validate_id(id)?;
    #[cfg(target_os = "windows")]
    {
        windows_impl::remove(id)
    }
    #[cfg(target_os = "macos")]
    {
        macos_impl::remove(id)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux_impl::remove(id)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = id;
        Err(SchedulerError::Unsupported)
    }
}

/// Every schedule id this app currently has installed in the host
/// scheduler.
///
/// Listing once beats probing per id. On Windows `is_installed` spawns
/// `schtasks /Query` — ~29 ms per call — so decorating a list of ten
/// schedules cost ~250 ms of blocked UI; the folder-scoped query below
/// is one ~22 ms spawn regardless of how many there are. On macOS and
/// Linux the per-id probe is only a `stat`, but enumerating a directory
/// is still one syscall instead of N.
///
/// An id present in the OS but unknown to the app is included: callers
/// intersect against their own list, and silently hiding a stray
/// `\Freally\…` task would make an orphan invisible.
pub fn installed_ids() -> Result<std::collections::HashSet<String>, SchedulerError> {
    #[cfg(target_os = "windows")]
    {
        windows_impl::installed_ids()
    }
    #[cfg(target_os = "macos")]
    {
        macos_impl::installed_ids()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux_impl::installed_ids()
    }
    #[cfg(not(any(windows, unix)))]
    {
        Ok(std::collections::HashSet::new())
    }
}

/// Whether `id` is currently installed in the host scheduler.
///
/// Prefer [`installed_ids`] when checking more than one.
pub fn is_installed(id: &str) -> Result<bool, SchedulerError> {
    validate_id(id)?;
    #[cfg(target_os = "windows")]
    {
        windows_impl::is_installed(id)
    }
    #[cfg(target_os = "macos")]
    {
        macos_impl::is_installed(id)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        linux_impl::is_installed(id)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = id;
        Ok(false)
    }
}

/// Absolute path to an OS-provided tool.
///
/// **Never** spawn these by bare name. Rust's Windows `Command`
/// resolution searches the directory of `current_exe()` *before*
/// System32, so a `schtasks.exe` dropped next to the app wins over the
/// real one. On a normal `C:\Program Files` install that is only
/// same-user, but a **portable** install lives on a stick, a share, or
/// a synced folder — anywhere a second party can write one file — and
/// then it is arbitrary code execution as the Freally user the first
/// time they save a schedule. Unix gets the same treatment against a
/// hostile `PATH`.
#[cfg(any(windows, unix))]
fn os_tool(name: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"));
        root.join("System32").join(format!("{name}.exe"))
    }
    #[cfg(unix)]
    {
        for dir in ["/bin", "/usr/bin", "/sbin", "/usr/sbin"] {
            let candidate = std::path::Path::new(dir).join(name);
            if candidate.exists() {
                return candidate;
            }
        }
        // Nothing found at a standard location: fall back to the bare
        // name so the error names the tool rather than a path that was
        // never going to exist.
        std::path::PathBuf::from(name)
    }
}

/// Crate-visible alias so sibling modules (`autostart`) get the same
/// hardening without duplicating the lookup.
#[cfg(any(windows, unix))]
pub(crate) fn os_tool_pub(name: &str) -> std::path::PathBuf {
    os_tool(name)
}

/// Run `program` with `args`, mapping a non-zero exit into a typed
/// error carrying the child's stderr.
#[cfg(any(windows, unix))]
fn run(program: &str, args: &[&str]) -> Result<(), SchedulerError> {
    let out = std::process::Command::new(os_tool(program))
        .args(args)
        .output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(SchedulerError::CommandFailed {
        command: program.to_string(),
        code: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

#[cfg(unix)]
fn home_dir() -> Result<PathBuf, SchedulerError> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or(SchedulerError::NoHomeDir)
}

/// Write `contents` to `path`, creating the parent directory.
#[cfg(unix)]
fn write_stanza(path: &std::path::Path, contents: &str) -> Result<(), SchedulerError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    Ok(())
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::collections::HashSet;

    use super::{
        PathBuf, ScheduledJob, SchedulerError, render_windows_task_xml, run, windows_task_name,
    };

    /// Task Scheduler rejects a `/XML` file that is not UTF-16 — it
    /// reads the declared `encoding="UTF-16"` and expects the bytes to
    /// match. Emit a little-endian BOM plus UTF-16LE code units.
    fn utf16_bytes(s: &str) -> Vec<u8> {
        let mut out = vec![0xFF, 0xFE];
        for unit in s.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    /// Create the staging file **exclusively**, in a private directory,
    /// under an unpredictable name.
    ///
    /// `schtasks /XML` reads the file by path in a second process, so
    /// the window between our write and its read is a substitution
    /// opportunity. Three things close it: the file lives beside our
    /// own config rather than in the shared temp root, `create_new`
    /// refuses to follow an existing file or reparse point, and the
    /// name carries entropy so it cannot be pre-created to either
    /// hijack the install or DoS it.
    fn staged_xml(id: &str, bytes: &[u8]) -> Result<PathBuf, SchedulerError> {
        use std::io::Write;

        let dir = staging_dir()?;
        std::fs::create_dir_all(&dir)?;
        for attempt in 0..16u32 {
            let path = dir.join(format!("{id}-{}-{attempt}.xml", nonce()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    f.write_all(bytes)?;
                    f.flush()?;
                    return Ok(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(SchedulerError::Io(e)),
            }
        }
        Err(SchedulerError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not create a private staging file for the task XML",
        )))
    }

    /// Per-install staging directory. Falls back to the temp root only
    /// if the config dir cannot be resolved at all — in which case the
    /// `create_new` + nonce above still carry the guarantee.
    fn staging_dir() -> Result<PathBuf, SchedulerError> {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Ok(base.join("freally-file-manager").join("staging"))
    }

    /// Cheap per-call entropy. Not a CSPRNG and does not need to be —
    /// the security property comes from `create_new`; this only stops
    /// an attacker pre-creating the one name we would otherwise use.
    fn nonce() -> u64 {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut h = RandomState::new().build_hasher();
        h.write_u64(std::process::id() as u64);
        h.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
        );
        h.finish()
    }

    pub fn install(job: &ScheduledJob) -> Result<(), SchedulerError> {
        let xml = render_windows_task_xml(job);
        let staged = staged_xml(&job.id, &utf16_bytes(&xml))?;
        let name = windows_task_name(&job.id);
        let staged_str = staged.to_string_lossy().into_owned();
        let result = run(
            "schtasks",
            &["/Create", "/TN", &name, "/XML", &staged_str, "/F"],
        );
        // The staged XML carries the full command line; it has served
        // its purpose either way, so clear it before propagating.
        let _ = std::fs::remove_file(&staged);
        result
    }

    pub fn remove(id: &str) -> Result<(), SchedulerError> {
        // Only a query that *positively* reports the task absent
        // justifies skipping the delete. Previously any query failure
        // — Task Scheduler service stopped, task owned by another
        // session and access denied — read as "not installed", so
        // `remove` returned Ok without ever running `/Delete` and
        // `schedule_remove_impl` then dropped the settings row. The app
        // had permanently forgotten a task that was still installed and
        // still firing nightly.
        //
        // When we cannot tell, attempt the delete and let its own exit
        // status decide.
        if let Ok(false) = is_installed(id) {
            return Ok(());
        }
        let name = windows_task_name(id);
        run("schtasks", &["/Delete", "/TN", &name, "/F"])
    }

    pub fn is_installed(id: &str) -> Result<bool, SchedulerError> {
        // Deliberately *not* a per-task `/Query`: that exits 1 both for
        // "no such task" and for "could not ask", and the reason text
        // is localized, so it cannot be told apart. Enumerating the
        // folder gives a signal we can trust — if the enumeration
        // itself succeeds, the absence of `id` from the result is
        // authoritative.
        Ok(installed_ids()?.contains(id))
    }

    fn query(args: &[&str]) -> Result<std::process::Output, SchedulerError> {
        Ok(std::process::Command::new(super::os_tool("schtasks"))
            .args(args)
            .output()?)
    }

    /// Enumerate `\Freally\*` in one call.
    ///
    /// `/NH` drops the header row, so the output is not localisation-
    /// sensitive; the first CSV field is the task path.
    pub fn installed_ids() -> Result<HashSet<String>, SchedulerError> {
        let out = query(&["/Query", "/FO", "CSV", "/NH", "/TN", r"\Freally\"])?;
        if !out.status.success() {
            // Either our folder does not exist yet (nothing installed
            // — the common case) or the query itself failed. `schtasks`
            // exits 1 for both. Ask a question whose answer we can
            // trust: enumerate the root. If *that* works the service is
            // reachable and our folder is genuinely absent; if it does
            // not, we simply could not tell, and reporting "nothing is
            // installed" would let `remove` drop a settings row for a
            // task that is still there and still firing.
            let root = query(&["/Query", "/FO", "CSV", "/NH"])?;
            if root.status.success() {
                return Ok(HashSet::new());
            }
            return Err(SchedulerError::Io(std::io::Error::other(
                "could not query the Windows Task Scheduler",
            )));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .filter_map(|line| {
                let first = line.trim().trim_start_matches('"');
                let first = first.split('"').next().unwrap_or(first);
                first.strip_prefix(r"\Freally\").map(str::to_string)
            })
            .filter(|id| !id.is_empty())
            .collect())
    }
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use std::collections::HashSet;

    use super::{
        PathBuf, ScheduledJob, SchedulerError, home_dir, launchd_label, render_launchd_plist, run,
        write_stanza,
    };

    fn plist_path(id: &str) -> Result<PathBuf, SchedulerError> {
        Ok(home_dir()?
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", launchd_label(id))))
    }

    pub fn install(job: &ScheduledJob) -> Result<(), SchedulerError> {
        let path = plist_path(&job.id)?;
        // Unload any prior generation first: launchctl refuses to load
        // a label that is already registered, so a re-install of an
        // edited schedule would otherwise write the new plist and keep
        // running the old one.
        let path_str = path.to_string_lossy().into_owned();
        if path.exists() {
            let _ = run("launchctl", &["unload", "-w", &path_str]);
        }
        write_stanza(&path, &render_launchd_plist(job))?;
        // Roll the plist back if the load fails. Returning `Err` with
        // the file still on disk left a live orphan: `schedule_save`
        // propagates the error *before* pushing the settings row, but
        // launchd loads every plist in LaunchAgents at the next login —
        // so the job would start firing with no settings row backing it
        // and no UI affordance able to remove it.
        //
        // Failure here is not exotic: a stale label, a non-Aqua session
        // or a SIP/TCC denial all land on this path.
        match run("launchctl", &["load", "-w", &path_str]) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(e)
            }
        }
    }

    pub fn remove(id: &str) -> Result<(), SchedulerError> {
        let path = plist_path(id)?;
        if !path.exists() {
            return Ok(());
        }
        let path_str = path.to_string_lossy().into_owned();
        let unloaded = run("launchctl", &["unload", "-w", &path_str]);
        // Delete the plist even if unload failed — leaving it behind
        // means the agent returns at next login.
        std::fs::remove_file(&path)?;
        unloaded
    }

    pub fn is_installed(id: &str) -> Result<bool, SchedulerError> {
        Ok(plist_path(id)?.exists())
    }

    /// Read `~/Library/LaunchAgents` once and pick out our labels.
    pub fn installed_ids() -> Result<HashSet<String>, SchedulerError> {
        let dir = home_dir()?.join("Library").join("LaunchAgents");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(HashSet::new());
        };
        Ok(entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| {
                name.strip_suffix(".plist")
                    .and_then(|stem| stem.strip_prefix("dev.freally.job."))
                    .map(str::to_string)
            })
            .collect())
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod linux_impl {
    use std::collections::HashSet;

    use super::{
        PathBuf, ScheduledJob, SchedulerError, home_dir, render_systemd_service,
        render_systemd_timer, run, systemd_unit_stem, write_stanza,
    };

    fn unit_dir() -> Result<PathBuf, SchedulerError> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x),
            _ => home_dir()?.join(".config"),
        };
        Ok(base.join("systemd").join("user"))
    }

    pub fn install(job: &ScheduledJob) -> Result<(), SchedulerError> {
        let dir = unit_dir()?;
        let stem = systemd_unit_stem(&job.id);
        write_stanza(
            &dir.join(format!("{stem}.service")),
            &render_systemd_service(job),
        )?;
        write_stanza(
            &dir.join(format!("{stem}.timer")),
            &render_systemd_timer(job),
        )?;
        run("systemctl", &["--user", "daemon-reload"])?;
        let timer = format!("{stem}.timer");
        run("systemctl", &["--user", "enable", "--now", &timer])
    }

    pub fn remove(id: &str) -> Result<(), SchedulerError> {
        let dir = unit_dir()?;
        let stem = systemd_unit_stem(id);
        let timer = format!("{stem}.timer");
        let service = dir.join(format!("{stem}.service"));
        let timer_path = dir.join(&timer);
        if !timer_path.exists() && !service.exists() {
            return Ok(());
        }
        // Disable before unlinking: systemd resolves the unit by name,
        // so deleting the file first leaves the enable symlink dangling
        // and `daemon-reload` warns forever.
        let disabled = run("systemctl", &["--user", "disable", "--now", &timer]);
        for p in [&timer_path, &service] {
            if p.exists() {
                std::fs::remove_file(p)?;
            }
        }
        run("systemctl", &["--user", "daemon-reload"])?;
        disabled
    }

    pub fn is_installed(id: &str) -> Result<bool, SchedulerError> {
        Ok(unit_dir()?
            .join(format!("{}.timer", systemd_unit_stem(id)))
            .exists())
    }

    /// Read `~/.config/systemd/user` once and pick out our timers.
    pub fn installed_ids() -> Result<HashSet<String>, SchedulerError> {
        let Ok(entries) = std::fs::read_dir(unit_dir()?) else {
            return Ok(HashSet::new());
        };
        Ok(entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| {
                name.strip_suffix(".timer")
                    .and_then(|stem| stem.strip_prefix("freally-"))
                    .map(str::to_string)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(trigger: ScheduleTrigger, missed_run: MissedRunPolicy) -> ScheduledJob {
        ScheduledJob {
            id: "nightly-backup".to_string(),
            label: "Nightly backup".to_string(),
            program: if cfg!(windows) {
                PathBuf::from(r"C:\Program Files\Freally\freally.exe")
            } else {
                PathBuf::from("/usr/bin/freally")
            },
            args: vec!["--enqueue".into(), "copy".into()],
            trigger,
            missed_run,
        }
    }

    #[test]
    fn ids_are_restricted_to_a_safe_alphabet() {
        assert!(validate_id("nightly-backup").is_ok());
        assert!(validate_id("a").is_ok());
        for bad in [
            "",
            "Nightly",
            "night ly",
            "../escape",
            "night.ly",
            "night/ly",
            "night\\ly",
            "nïghtly",
            &"a".repeat(49),
        ] {
            assert!(validate_id(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn control_characters_never_reach_a_stanza() {
        let mut j = job(
            ScheduleTrigger::Daily { hour: 3, minute: 0 },
            MissedRunPolicy::Skip,
        );
        j.label = "Nightly\n[Service]\nExecStart=/bin/sh".to_string();
        assert!(matches!(
            validate_job(&j),
            Err(SchedulerError::InvalidLabel)
        ));

        let mut j = job(
            ScheduleTrigger::Daily { hour: 3, minute: 0 },
            MissedRunPolicy::Skip,
        );
        j.args = vec!["--dst".into(), "/tmp/x\nExecStart=/bin/sh".into()];
        assert!(matches!(
            validate_job(&j),
            Err(SchedulerError::InvalidCommand)
        ));
    }

    #[test]
    fn relative_programs_are_refused() {
        let mut j = job(
            ScheduleTrigger::Daily { hour: 3, minute: 0 },
            MissedRunPolicy::Skip,
        );
        j.program = PathBuf::from("freally.exe");
        assert!(matches!(
            validate_job(&j),
            Err(SchedulerError::InvalidCommand)
        ));
    }

    #[test]
    fn out_of_range_triggers_are_refused() {
        for t in [
            ScheduleTrigger::Hourly { minute: 60 },
            ScheduleTrigger::Daily {
                hour: 24,
                minute: 0,
            },
            ScheduleTrigger::Weekly {
                weekday: 7,
                hour: 0,
                minute: 0,
            },
        ] {
            let j = job(t, MissedRunPolicy::Skip);
            assert!(matches!(
                validate_job(&j),
                Err(SchedulerError::InvalidTrigger)
            ));
        }
    }

    #[test]
    fn next_run_hourly_rolls_to_the_following_hour() {
        // 1970-01-01 00:30:00 local, trigger at :15 → 01:15.
        let now = 30 * 60;
        assert_eq!(
            next_run_after(ScheduleTrigger::Hourly { minute: 15 }, now),
            3_600 + 15 * 60
        );
        // Exactly on the boundary must move forward, never return `now`.
        assert_eq!(
            next_run_after(ScheduleTrigger::Hourly { minute: 0 }, 3_600),
            7_200
        );
    }

    #[test]
    fn next_run_daily_rolls_to_tomorrow_once_past() {
        let now = 4 * 3_600; // 04:00
        assert_eq!(
            next_run_after(ScheduleTrigger::Daily { hour: 3, minute: 0 }, now),
            86_400 + 3 * 3_600
        );
        assert_eq!(
            next_run_after(
                ScheduleTrigger::Daily {
                    hour: 5,
                    minute: 30
                },
                now
            ),
            5 * 3_600 + 30 * 60
        );
    }

    #[test]
    fn next_run_weekly_lands_on_the_requested_weekday() {
        // Epoch day 0 (1970-01-01) is a Thursday = weekday 4.
        let thursday_noon = 12 * 3_600;
        // Next Sunday (weekday 0) is 3 days out.
        assert_eq!(
            next_run_after(
                ScheduleTrigger::Weekly {
                    weekday: 0,
                    hour: 3,
                    minute: 0
                },
                thursday_noon
            ),
            3 * 86_400 + 3 * 3_600
        );
        // Same weekday but already past → a full week later.
        assert_eq!(
            next_run_after(
                ScheduleTrigger::Weekly {
                    weekday: 4,
                    hour: 3,
                    minute: 0
                },
                thursday_noon
            ),
            7 * 86_400 + 3 * 3_600
        );
        // Same weekday, still ahead → today.
        assert_eq!(
            next_run_after(
                ScheduleTrigger::Weekly {
                    weekday: 4,
                    hour: 18,
                    minute: 0
                },
                thursday_noon
            ),
            18 * 3_600
        );
    }

    #[test]
    fn next_run_is_stable_before_the_epoch() {
        // Negative local seconds must not produce a firing in the past;
        // `rem_euclid` is what keeps the day boundary correct here.
        let now = -3_600; // 1969-12-31 23:00
        let next = next_run_after(ScheduleTrigger::Daily { hour: 3, minute: 0 }, now);
        assert!(next > now);
        assert_eq!(next, 3 * 3_600);
    }

    #[test]
    fn windows_xml_escapes_and_carries_the_missed_run_policy() {
        let mut j = job(
            ScheduleTrigger::Daily { hour: 3, minute: 5 },
            MissedRunPolicy::RunWhenAvailable,
        );
        j.label = "Backup <A & B>".to_string();
        j.args = vec!["--destination".into(), r#"D:\a"b"#.into()];
        let xml = render_windows_task_xml(&j);
        assert!(xml.contains("<Description>Backup &lt;A &amp; B&gt;</Description>"));
        assert!(xml.contains("<StartWhenAvailable>true</StartWhenAvailable>"));
        assert!(xml.contains("2026-01-01T03:05:00"));
        // The raw quote must survive as a doubled cmd-quote *and* be
        // XML-escaped — one escaping does not substitute for the other.
        assert!(xml.contains(r"&quot;D:\a&quot;&quot;b&quot;"));
        assert!(!xml.contains("<A & B>"));

        let skip = render_windows_task_xml(&job(
            ScheduleTrigger::Daily { hour: 3, minute: 5 },
            MissedRunPolicy::Skip,
        ));
        assert!(skip.contains("<StartWhenAvailable>false</StartWhenAvailable>"));
    }

    #[test]
    fn win_quote_survives_a_commandlinetoargvw_round_trip() {
        // A trailing backslash escapes the closing quote, so the
        // argument absorbs the next one. `D:\` is an entirely ordinary
        // destination — this is reachable without malice, and the
        // symptom is an unattended job writing somewhere other than
        // where the user pointed it.
        assert_eq!(win_quote(r"D:\"), r#""D:\\""#);
        assert_eq!(win_quote(r"D:\backup\"), r#""D:\backup\\""#);
        // Interior backslashes are literal and must NOT be doubled.
        assert_eq!(win_quote(r"D:\a\b"), r#""D:\a\b""#);
        // A run of backslashes before a quote is doubled, then the
        // quote itself.
        assert_eq!(win_quote(r#"a\"b"#), r#""a\\""b""#);
        assert_eq!(win_quote("plain"), r#""plain""#);
    }

    #[test]
    fn windows_weekly_names_the_day_element() {
        let xml = render_windows_task_xml(&job(
            ScheduleTrigger::Weekly {
                weekday: 0,
                hour: 3,
                minute: 0,
            },
            MissedRunPolicy::Skip,
        ));
        assert!(xml.contains("<Sunday/>"));
        assert!(xml.contains("<ScheduleByWeek>"));
    }

    #[test]
    fn launchd_plist_lists_argv_and_the_calendar_dict() {
        let plist = render_launchd_plist(&job(
            ScheduleTrigger::Weekly {
                weekday: 2,
                hour: 4,
                minute: 30,
            },
            MissedRunPolicy::Skip,
        ));
        assert!(plist.contains("<string>dev.freally.job.nightly-backup</string>"));
        assert!(plist.contains("<key>Weekday</key><integer>2</integer>"));
        assert!(plist.contains("<key>Hour</key><integer>4</integer>"));
        assert!(plist.contains("<string>--enqueue</string>"));
        // RunAtLoad must stay false: loading the agent at login is not
        // the same as the user's chosen schedule firing.
        assert!(plist.contains("<key>RunAtLoad</key>\n  <false/>"));
    }

    #[test]
    fn systemd_units_quote_exec_and_map_the_policy() {
        let mut j = job(
            ScheduleTrigger::Daily { hour: 3, minute: 0 },
            MissedRunPolicy::RunWhenAvailable,
        );
        j.program = PathBuf::from("/usr/bin/freally it's");
        let service = render_systemd_service(&j);
        assert!(service.contains(r"ExecStart='/usr/bin/freally it'\''s' '--enqueue' 'copy'"));

        let timer = render_systemd_timer(&j);
        assert!(timer.contains("OnCalendar=*-*-* 03:00:00"));
        assert!(timer.contains("Persistent=true"));
        assert!(timer.contains("Unit=freally-nightly-backup.service"));

        let skip = render_systemd_timer(&job(
            ScheduleTrigger::Daily { hour: 3, minute: 0 },
            MissedRunPolicy::Skip,
        ));
        assert!(skip.contains("Persistent=false"));
    }

    #[test]
    fn systemd_units_escape_percent_signs() {
        // A unit file is not a shell. `shell_quote`'s single quotes stop
        // the shell from touching a value but do nothing about systemd
        // specifier expansion, so a literal `%` has to become `%%`.
        //
        // Left unescaped, a destination like `/mnt/backup 50% full`
        // makes the .service fail to load while the .timer enables
        // fine, both systemctl calls exit 0, install reports success —
        // and every firing silently does nothing, forever.
        let mut j = job(
            ScheduleTrigger::Daily { hour: 3, minute: 0 },
            MissedRunPolicy::Skip,
        );
        j.label = "Backup 50% full".to_string();
        j.args = vec!["--destination".to_string(), "/mnt/100%.d".to_string()];

        let service = render_systemd_service(&j);
        assert!(
            service.contains("Description=Backup 50%% full"),
            "label must be escaped: {service}",
        );
        assert!(
            service.contains("'/mnt/100%%.d'"),
            "args must be escaped: {service}",
        );
        assert!(
            !service.contains("50% full"),
            "no bare `%` may survive into a unit file: {service}",
        );

        let timer = render_systemd_timer(&j);
        assert!(
            timer.contains("Description=Backup 50%% full (timer)"),
            "the timer's Description needs it too: {timer}",
        );
    }

    #[test]
    fn systemd_weekly_prefixes_the_day() {
        let timer = render_systemd_timer(&job(
            ScheduleTrigger::Weekly {
                weekday: 1,
                hour: 6,
                minute: 15,
            },
            MissedRunPolicy::Skip,
        ));
        assert!(timer.contains("OnCalendar=Mon *-*-* 06:15:00"));
    }

    #[test]
    fn macos_reports_the_policy_it_actually_honors() {
        if cfg!(target_os = "macos") {
            assert!(!policy_is_honored(MissedRunPolicy::Skip));
            assert!(policy_is_honored(MissedRunPolicy::RunWhenAvailable));
        } else {
            assert!(policy_is_honored(MissedRunPolicy::Skip));
            assert!(policy_is_honored(MissedRunPolicy::RunWhenAvailable));
        }
    }

    #[test]
    fn install_and_remove_reject_a_hostile_id_before_touching_the_os() {
        let mut j = job(
            ScheduleTrigger::Daily { hour: 3, minute: 0 },
            MissedRunPolicy::Skip,
        );
        j.id = "../../evil".to_string();
        assert!(matches!(install(&j), Err(SchedulerError::InvalidId)));
        assert!(matches!(
            remove("../../evil"),
            Err(SchedulerError::InvalidId)
        ));
        assert!(matches!(
            is_installed("../../evil"),
            Err(SchedulerError::InvalidId)
        ));
    }
}
