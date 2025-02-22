use std::env;
use std::fs::{self, read, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

use ctrlc;
use log::{debug, info};
use systemd_rs::login::monitor::{Category, Monitor};
use systemd_rs::login::session as login_session;

use crate::controller::{ControllerInterface, ServiceMainFn};
use crate::session;
use crate::Error;
use crate::ServiceEvent;

use super::BasicServiceStatus;

type LinuxServiceMainWrapperFn = extern "system" fn(args: Vec<String>);
pub type Session = session::Session_<String>;

fn systemctl_execute_with_result(args: &[&str]) -> Result<String, Error> {
    let mut process = Command::new("systemctl");
    process.args(args);

    let output = process
        .output()
        .map_err(|e| Error::new(&format!("Failed to execute command {}: {}", args[0], e)))?;

    if !output.status.success() {
        return Err(Error::new(&format!(
            "Command \"{}\" failed ({}): {}",
            args[0],
            output.status.code().expect("Process terminated by signal"),
            std::str::from_utf8(&output.stderr).unwrap_or_default()
        )));
    }

    if !output.stdout.is_empty() {
        info!("{}", String::from_utf8_lossy(&output.stdout));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn systemctl_execute(args: &[&str]) -> Result<(), Error> {
    let mut process = Command::new("systemctl");
    process.args(args);

    let output = process
        .output()
        .map_err(|e| Error::new(&format!("Failed to execute command {}: {}", args[0], e)))?;

    if !output.status.success() {
        return Err(Error::new(&format!(
            "Command \"{}\" failed ({}): {}",
            args[0],
            output.status.code().expect("Process terminated by signal"),
            std::str::from_utf8(&output.stderr).unwrap_or_default()
        )));
    }

    if !output.stdout.is_empty() {
        info!("{}", String::from_utf8_lossy(&output.stdout));
    }

    Ok(())
}

fn systemd_install_daemon(name: &str) -> Result<(), Error> {
    systemctl_execute(&["daemon-reload"])?;
    systemctl_execute(&["enable", name])
}

fn systemd_uninstall_daemon(name: &str) -> Result<(), Error> {
    systemctl_execute(&["disable", name])?;
    systemctl_execute(&["daemon-reload"])
        .map_err(|e| debug!("{}", e))
        .ok();
    systemctl_execute(&["reset-failed"])
        .map_err(|e| debug!("{}", e))
        .ok();

    Ok(())
}

fn systemd_start_daemon(name: &str) -> Result<(), Error> {
    systemctl_execute(&["start", name])
}

fn systemd_stop_daemon(name: &str) -> Result<(), Error> {
    systemctl_execute(&["stop", name])
}

pub struct LinuxController {
    pub service_name: String,
    pub display_name: String,
    pub description: String,
    pub config: Option<String>,
}

#[derive(Debug)]
pub enum ActiveState {
    Running,
    Exited,
    Waiting,
    Dead,
}

#[derive(Debug)]
pub enum InactiveState {
    Dead,
    Exited,
    Waiting,
    Resetting,
}

#[derive(Debug)]
pub enum ServiceState {
    Active(ActiveState),
    Inactive(InactiveState),
}

#[derive(Debug)]
pub struct ServiceStatus {
    pub state: ServiceState,
    pub cmdline: String,
    pub pid: u32,
    pub is_failed: bool,
    pub details: String,
}

impl ServiceStatus {
    pub fn is_active(&self) -> bool {
        matches!(&self.state, ServiceState::Active(_))
    }
    pub fn is_inactive(&self) -> bool {
        matches!(&self.state, ServiceState::Inactive(_))
    }
}

impl BasicServiceStatus for ServiceStatus {
    fn is_running(&self) -> bool {
        matches!(&self.state, ServiceState::Active(state) if matches!(state, ActiveState::Running))
    }

    fn is_failed(&self) -> bool {
        self.is_failed
    }

    fn get_cmdline(&self) -> &str {
        &self.cmdline
    }
}
impl LinuxController {
    pub fn new(service_name: &str, display_name: &str, description: &str) -> LinuxController {
        LinuxController {
            service_name: service_name.to_string(),
            display_name: display_name.to_string(),
            description: description.to_string(),
            config: None,
        }
    }

    pub fn register(
        &mut self,
        service_main_wrapper: LinuxServiceMainWrapperFn,
    ) -> Result<(), Error> {
        service_main_wrapper(env::args().collect());
        Ok(())
    }

    fn get_service_file_name(&self) -> String {
        format!("{}.service", &self.service_name)
    }

    fn get_service_unit_path(&self) -> PathBuf {
        Path::new("/lib/systemd/system/").join(self.get_service_file_name())
    }

    fn get_service_dropin_dir(&self) -> PathBuf {
        Path::new("/lib/systemd/system/").join(format!("{}.d", self.get_service_file_name()))
    }

    fn get_service_unit_content(&self) -> Result<String, Error> {
        Ok(format!(
            r#"
[Unit]
Description={}

[Service]
ExecStart={}

[Install]
WantedBy=multi-user.target"#,
            self.service_name,
            fs::read_link("/proc/self/exe")
                .map_err(|e| Error::new(&format!("Failed to read /proc/self/exe: {}", e)))?
                .to_str()
                .ok_or("Failed to parse /proc/self/exe")?
        ))
    }

    fn write_service_config(&self) -> Result<(), Error> {
        let path = self.get_service_unit_path();
        let content = self.get_service_unit_content()?;
        info!("Writing service file {}", path.display());
        File::create(&path)
            .and_then(|mut file| file.write_all(content.as_bytes()))
            .map_err(|e| Error::new(&format!("Failed to write {}: {}", path.display(), e)))?;

        if let Some(ref config) = self.config {
            let path = self
                .get_service_dropin_dir()
                .join(format!("{}.conf", self.service_name));
            fs::create_dir(path.parent().unwrap())
                .map_err(|e| Error::new(&format!("Failed to create {}: {}", path.display(), e)))?;
            info!("Writing config file {}", path.display());
            File::create(&path)
                .and_then(|mut file| file.write_all(config.as_bytes()))
                .map_err(|e| Error::new(&format!("Failed to write {}: {}", path.display(), e)))?;
        }

        Ok(())
    }
}

impl ControllerInterface for LinuxController {
    fn create(&mut self) -> Result<(), Error> {
        self.write_service_config()?;

        systemd_install_daemon(&self.service_name)
    }

    fn delete(&mut self) -> Result<(), Error> {
        systemd_uninstall_daemon(&self.service_name)?;

        let path = self.get_service_unit_path();
        fs::remove_file(&path)
            .map_err(|e| debug!("Failed to delete {}: {}", path.display(), e))
            .ok();

        let path = self.get_service_dropin_dir();
        fs::remove_dir_all(self.get_service_dropin_dir())
            .map_err(|e| debug!("Failed to delete {}: {}", path.display(), e))
            .ok();

        Ok(())
    }

    fn start(&mut self) -> Result<(), Error> {
        systemd_start_daemon(&self.service_name)
    }

    fn stop(&mut self) -> Result<(), Error> {
        systemd_stop_daemon(&self.service_name)
    }

    fn get_status(&self) -> Result<ServiceStatus, Error> {
        let pid = systemctl_execute_with_result(&["show", "-p", "MainPID", &self.service_name])?
            .trim_start_matches("MainPID=")
            .trim()
            .parse::<u32>()
            .unwrap();

        let is_failed =
            if let Ok(ret) = systemctl_execute_with_result(&["is-failed", &self.service_name]) {
                ret.contains("failed")
            } else {
                false
            };

        let cmdline = String::from_utf8(read(&format!("/proc/{}/cmdline", pid)).unwrap()).unwrap();

        let result = systemctl_execute_with_result(&["status", &self.service_name])?;
        if result.contains("active (") {
            Ok(ServiceStatus {
                state: if result.contains(" (running)") {
                    ServiceState::Active(ActiveState::Running)
                } else if result.contains(" (exited)") {
                    ServiceState::Active(ActiveState::Exited)
                } else if result.contains(" (waiting)") {
                    ServiceState::Active(ActiveState::Waiting)
                } else if result.contains(" (dead)") {
                    ServiceState::Active(ActiveState::Dead)
                } else {
                    return Err(Error::new(&format!("Invalid ActiveState : {}", result)));
                },
                details: result,
                cmdline,
                pid,
                is_failed,
            })
        } else {
            Ok(ServiceStatus {
                state: if result.contains(" (dead)") {
                    ServiceState::Inactive(InactiveState::Dead)
                } else if result.contains(" (exited)") {
                    ServiceState::Inactive(InactiveState::Exited)
                } else if result.contains(" (waiting)") {
                    ServiceState::Inactive(InactiveState::Waiting)
                } else if result.contains(" (resetting)") {
                    ServiceState::Inactive(InactiveState::Resetting)
                } else {
                    return Err(Error::new(&format!("Invalid ActiveState : {}", result)));
                },
                details: result,
                cmdline,
                pid,
                is_failed,
            })
        }
    }
}

fn run_monitor<T: Send + 'static>(
    tx: mpsc::Sender<ServiceEvent<T>>,
) -> Result<Monitor, std::io::Error> {
    let monitor = Monitor::new()?;

    let mut current_session = match login_session::get_active_session() {
        Ok(s) => Some(s),
        Err(e) => {
            debug!("Failed to get active session {}", e);
            None
        }
    };

    monitor.init(Category::Sessions, move || {
        let active_session = match login_session::get_active_session() {
            Ok(s) => Some(s),
            Err(e) => {
                debug!("Failed to get active session {}", e);
                None
            }
        };

        let session_changed = match (&current_session, &active_session) {
            (Some(current_session), Some(active_session)) => current_session != active_session,
            (None, None) => false,
            _ => true,
        };

        if session_changed {
            if let Some(active_session) = active_session.as_ref() {
                let _ = tx.send(ServiceEvent::SessionConnect(Session::new(
                    active_session.identifier.to_string(),
                )));
            }

            if let Some(current_session) = current_session.as_ref() {
                let _ = tx.send(ServiceEvent::SessionDisconnect(Session::new(
                    current_session.identifier.to_string(),
                )));
            }
        }

        current_session = active_session;
    })?;

    Ok(monitor)
}

#[macro_export]
macro_rules! Service {
    ($name:expr, $function:ident) => {
        extern "system" fn service_main_wrapper(args: Vec<String>) {
            dispatch($function, args);
        }
    };
}

#[doc(hidden)]
pub fn dispatch<T: Send + 'static>(service_main: ServiceMainFn<T>, args: Vec<String>) {
    let (tx, rx) = mpsc::channel();

    let _monitor = run_monitor(tx.clone()).expect("Failed to run session monitor");
    let _tx = tx.clone();

    ctrlc::set_handler(move || {
        let _ = tx.send(ServiceEvent::Stop);
    })
    .expect("Failed to register Ctrl-C handler");
    service_main(rx, _tx, args, false);
}
