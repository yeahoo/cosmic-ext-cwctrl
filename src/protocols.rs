//! Local bindings for COSMIC's private Wayland protocols.
//!
//! The `cosmic-protocols` crate is deliberately **not** used: it declares
//! GPL-3.0-only, which would make this binary GPL as well. The protocol XML it
//! ships, however, is MIT/HPND ("Permission to use, copy, modify, distribute,
//! and sell ... without fee"). This project therefore generates its bindings
//! straight from the vendored `protocols/*.xml`, keeping the whole project MIT.
//!
//! The `wayland_protocol!` macro is equivalent to the one used internally by the
//! `wayland-protocols` crate (which does not `#[macro_export]` it, so upstream
//! `cosmic-protocols` had to copy it as well).
//!
//! XML source: <https://github.com/pop-os/cosmic-protocols> (`unstable/`).
//! Each file keeps its original copyright notice -- do not remove it.

macro_rules! wayland_protocol {
    ($path:expr, [$($imports:path),*]) => {
        pub use self::generated::client;

        mod generated {
            #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
            #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
            #![allow(clippy::all)]

            pub mod client {
                use wayland_client;
                use wayland_client::protocol::*;
                $(use $imports::{client::*};)*

                pub mod __interfaces {
                    use wayland_client::protocol::__interfaces::*;
                    $(use $imports::{client::__interfaces::*};)*
                    wayland_scanner::generate_interfaces!($path);
                }
                use self::__interfaces::*;

                wayland_scanner::generate_client_code!($path);
            }
        }
    };
}

pub mod workspace {
    //! `zcosmic_workspace_*` -- referenced only by deprecated members of the two
    //! protocols below; this project itself uses `ext_workspace_manager_v1`.

    #[allow(missing_docs)]
    pub mod v1 {
        wayland_protocol!("./protocols/cosmic-workspace-unstable-v1.xml", []);
    }
}

pub mod toplevel_info {
    //! `zcosmic_toplevel_info_v1` / `zcosmic_toplevel_handle_v1`

    #[allow(missing_docs)]
    pub mod v1 {
        wayland_protocol!(
            "./protocols/cosmic-toplevel-info-unstable-v1.xml",
            [
                crate::protocols::workspace::v1,
                wayland_protocols::ext::foreign_toplevel_list::v1,
                wayland_protocols::ext::workspace::v1
            ]
        );
    }
}

pub mod toplevel_management {
    //! `zcosmic_toplevel_manager_v1`

    #[allow(missing_docs)]
    pub mod v1 {
        wayland_protocol!(
            "./protocols/cosmic-toplevel-management-unstable-v1.xml",
            [
                crate::protocols::workspace::v1,
                crate::protocols::toplevel_info::v1,
                wayland_protocols::ext::workspace::v1
            ]
        );
    }
}
