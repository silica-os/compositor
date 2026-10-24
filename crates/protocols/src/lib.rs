//! XML and bindings of the silica-* protocols.
//!
//! - `silica-glass-v1` (`xml/silica-glass-v1.xml`): glass regions of a surface.
//! - `silica-shell-v1` (`xml/silica-shell-v1.xml`): shell commands to the compositor.
//! - `silica-window-v1` (`xml/silica-window-v1.xml`): appearance of an app window
//!   (full-size content with the compositor's buttons on top).
//! - `silica-menu-bar-v1` (`xml/silica-menu-bar-v1.xml`): surfaces in the compositor's menu
//!   bar (an app's menu titles, status items).
//! - `silica-dock-v1` (`xml/silica-dock-v1.xml`): the Dock's control of nodes (windows and layer
//!   surfaces), gestures and hot corners.
//!
//! The server side (`server`) is for the compositor, the client side (`client`) for the embedder.

/// `silica-glass-v1`.
pub mod glass {
    #[cfg(feature = "server")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod server {
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-glass-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("xml/silica-glass-v1.xml");
    }

    #[cfg(feature = "client")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;

        pub mod __interfaces {
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-glass-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("xml/silica-glass-v1.xml");
    }

    /// Pending regions beyond which the request is a client error.
    pub const MAX_REGIONS: usize = 64;
}

/// `silica-shell-v1`.
pub mod shell {
    #[cfg(feature = "server")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod server {
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-shell-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("xml/silica-shell-v1.xml");
    }

    #[cfg(feature = "client")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;

        pub mod __interfaces {
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-shell-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("xml/silica-shell-v1.xml");
    }

    /// Environment variable that the compositor sets on the shell components it launches: only their
    /// processes see `silica_shell_manager_v1` and the window list.
    pub const SHELL_ENV: &str = "SILICA_SHELL";
}

/// `silica-window-v1`.
pub mod window {
    #[cfg(feature = "server")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod server {
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-window-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("xml/silica-window-v1.xml");
    }

    #[cfg(feature = "client")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;

        pub mod __interfaces {
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-window-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("xml/silica-window-v1.xml");
    }
}

/// `silica-dock-v1`.
pub mod dock {
    #[cfg(feature = "server")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod server {
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-dock-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("xml/silica-dock-v1.xml");
    }

    #[cfg(feature = "client")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;

        pub mod __interfaces {
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-dock-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("xml/silica-dock-v1.xml");
    }
}

/// `silica-menu-bar-v1`.
pub mod menu_bar {
    #[cfg(feature = "server")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod server {
        use wayland_protocols::xdg::shell::server::*;
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_protocols::xdg::shell::server::__interfaces::*;
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-menu-bar-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("xml/silica-menu-bar-v1.xml");
    }

    #[cfg(feature = "client")]
    #[allow(
        missing_docs,
        clippy::all,
        clippy::pedantic,
        unused_imports,
        non_upper_case_globals
    )]
    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;
        use wayland_protocols::xdg::shell::client::*;

        pub mod __interfaces {
            use wayland_client::protocol::__interfaces::*;
            use wayland_protocols::xdg::shell::client::__interfaces::*;
            wayland_scanner::generate_interfaces!("xml/silica-menu-bar-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("xml/silica-menu-bar-v1.xml");
    }
}
