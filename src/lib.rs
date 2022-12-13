//! ceviche is a wrapper to write a service/daemon.
//!
//! At the moment only Windows services are supported. The Service macro is inspired
//! from the [winservice](https://crates.io/crates/winservice) crate.
//!
//! A service implements a service main function and is generated by invoking
//! the `Service!` macro. The events are sent to the service over the `rx` channel.
//!
//! ```rust,ignore
//!  enum CustomServiceEvent {}
//!
//! fn my_service_main(
//!     rx: mpsc::Receiver<ServiceEvent<CustomServiceEvent>>,
//!     _tx: mpsc::Sender<ServiceEvent<CustomServiceEvent>>,
//!     args: Vec<String>,
//!     standalone_mode: bool) -> u32 {
//!    loop {
//!        if let Ok(control_code) = rx.recv() {
//!            match control_code {
//!                ServiceEvent::Stop => break,
//!                _ => (),
//!            }
//!        }
//!    }
//!    0
//! }
//!
//! Service!("Foobar", my_service_main);
//! ```
//!
//! The Controller is a helper to create, remove, start or stop the service
//! on the system. ceviche also supports a standalone mode were the service
//! code runs as a normal executable which can be useful for development and
//! debugging.
//!
//! ```rust,ignore
//! static SERVICE_NAME: &'static str = "foobar";
//! static DISPLAY_NAME: &'static str = "FooBar Service";
//! static DESCRIPTION: &'static str = "This is the FooBar service";
//!
//! fn main() {
//!     let yaml = load_yaml!("cli.yml");
//!     let app = App::from_yaml(yaml);
//!     let matches = app.version(crate_version!()).get_matches();
//!     let cmd = matches.value_of("cmd").unwrap_or("").to_string();
//!
//!     let mut controller = Controller::new(SERVICE_NAME, DISPLAY_NAME, DESCRIPTION);
//!
//!     match cmd.as_str() {
//!         "create" => controller.create(),
//!         "delete" => controller.delete(),
//!         "start" => controller.start(),
//!         "stop" => controller.stop(),
//!         "standalone" => {
//!             let (tx, rx) = mpsc::channel();
//!
//!             ctrlc::set_handler(move || {
//!                 let _ = tx.send(ServiceEvent::Stop);
//!             }).expect("Failed to register Ctrl-C handler");
//!
//!             my_service_main(rx, vec![], true);
//!         }
//!         _ => {
//!             let _result = controller.register(service_main_wrapper);
//!         }
//!     }
//! }
//!
//! ```

#[macro_use]
extern crate cfg_if;

/// Manages the service on the system.
pub mod controller;
pub mod session;

#[cfg(windows)]
pub use winapi;

use self::controller::Session;
use std::fmt;

/// Service errors
#[derive(Debug)]
pub struct Error {
    pub message: String,
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Error {
            message: message.to_string(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        write!(f, "{}", self.message,)
    }
}

impl std::error::Error for Error {
    fn description(&self) -> &str {
        &self.message
    }
}

impl Error {
    pub fn new(message: &str) -> Error {
        Error {
            message: String::from(message),
        }
    }
}

/// Events that are sent to the service.
pub enum ServiceEvent<T> {
    Continue,
    Pause,
    Stop,
    SessionConnect(Session),
    SessionDisconnect(Session),
    SessionRemoteConnect(Session),
    SessionRemoteDisconnect(Session),
    SessionLogon(Session),
    SessionLogoff(Session),
    SessionLock(Session),
    SessionUnlock(Session),
    Custom(T),
}

impl<T> fmt::Display for ServiceEvent<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        match self {
            ServiceEvent::Continue => write!(f, "Continue"),
            ServiceEvent::Pause => write!(f, "Pause"),
            ServiceEvent::Stop => write!(f, "Stop"),
            ServiceEvent::SessionConnect(id) => write!(f, "SessionConnect({})", id),
            ServiceEvent::SessionDisconnect(id) => write!(f, "SessionDisconnect({})", id),
            ServiceEvent::SessionRemoteConnect(id) => write!(f, "SessionRemoteConnect({})", id),
            ServiceEvent::SessionRemoteDisconnect(id) => {
                write!(f, "SessionRemoteDisconnect({})", id)
            }
            ServiceEvent::SessionLogon(id) => write!(f, "SessionLogon({})", id),
            ServiceEvent::SessionLogoff(id) => write!(f, "SessionLogoff({})", id),
            ServiceEvent::SessionLock(id) => write!(f, "SessionLock({})", id),
            ServiceEvent::SessionUnlock(id) => write!(f, "SessionUnlock({})", id),
            ServiceEvent::Custom(_) => write!(f, "Custom"),
        }
    }
}

#[cfg(test)]
mod tests {
    cfg_if! {
        if #[cfg(target_os = "windows")] {
            use std::mem;
            use winapi::shared::minwindef::{DWORD, LPVOID};
            use winapi::um::handleapi::CloseHandle;
            use winapi::um::processthreadsapi::{GetCurrentProcess, OpenProcessToken};
            use winapi::um::securitybaseapi::GetTokenInformation;
            use winapi::um::winnt::{TokenElevation, HANDLE, TOKEN_ELEVATION, TOKEN_QUERY};
        } else {
            use std::env;
        }
    }

    fn is_admin() -> bool {
        cfg_if! {
            if #[cfg(target_os = "linux")] {
                env::var("EUID").unwrap_or_default() == "0" || env::var("UID").unwrap_or_default() == "0"
            } else if #[cfg(target_os = "macos")] {
                env::var("USER").unwrap_or_default() == "root"
            } else if #[cfg(target_os = "windows")] {
                unsafe {
                    let mut token_handle: HANDLE = mem::zeroed();
                    if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) != 0 {
                        let mut token_elevation: TOKEN_ELEVATION = mem::zeroed();
                        let mut size: DWORD = 0;
                        let ret = GetTokenInformation(
                            token_handle,
                            TokenElevation,
                            &mut token_elevation as *mut _ as LPVOID,
                            mem::size_of::<TOKEN_ELEVATION>() as u32,
                            &mut size,
                        );
                        CloseHandle(token_handle);
                        if ret != 0 {
                            return token_elevation.TokenIsElevated != 0;
                        }
                    }
                }
                false
            } else {
                todo!();
            }
        }
    }

    use crate::controller::{BasicServiceStatus, Controller, ControllerInterface};

    #[test]
    fn create_delete_test() {
        if is_admin() {
            let mut controller = Controller::new("ceviche-rs-test-svc", "", "");
            assert!(controller.create().is_ok());
            assert!(controller.delete().is_ok());
        }
    }

    #[test]
    fn status_test() {
        cfg_if! {
            if #[cfg(target_os = "linux")] {
                let controller = Controller::new("dbus", "", "");
                use crate::controller::ServiceState;
            } else if #[cfg(target_os = "windows")] {
                let controller = Controller::new("ntfs", "", "");
            }
        }
        let status = controller.get_status();
        assert_eq!(status.is_ok(), true);
        if let Ok(status) = status {
            status.get_cmdline();
            status.is_running();
            status.is_failed();
            println!("status: {:?}", status);
            cfg_if! {
                if #[cfg(target_os = "linux")] {
                    status.is_active();
                    if matches!(status.state, ServiceState::Active(_)) {
                        println!("is active: {:?}", status.state);
                    }
                } else if #[cfg(target_os = "windows")] {
                    println!("is active: {:?}", status);
                } else if #[cfg(target_os = "macos")] {
                }
            }
        }
    }
}
