pub mod events {
    pub mod commands;
    pub mod context;
    pub mod engine;
    pub mod io;
    pub mod runner;
    pub mod keyboard {
        pub mod dispatch;
        pub mod interrupt;
        pub mod paste;
        pub mod router;

        pub mod keys {
            pub mod enter {
                pub mod router;
                pub use router::{handle_enter, EnterResult};
            }
            pub mod navigation {
                pub mod history;
                pub mod picker_nav;
                pub mod popup_nav;
                pub mod router;
                pub mod scroll;
                pub use router::handle_navigation;
            }
            pub mod typing {
                pub mod backspace;
                pub mod char_input;
                pub mod clear_line;
                pub mod router;
                pub use router::handle_typing;
            }
        }

        pub use router::{handle_keyboard_event, KeyResult};
    }

    pub mod mouse {
        pub mod drag;
        pub mod router;
        pub mod scroll {
            pub mod router;
            pub use router::handle_scroll;
        }
        pub use router::handle_mouse_event;
    }
}

pub mod render {
    pub mod markdown {
        pub mod block_parser;
        pub mod inline;
        #[cfg(test)]
        mod inline_tests {
            mod edge_cases;
            mod fixtures;
            mod formatting;
        }
        pub mod table;
        #[cfg(test)]
        mod table_tests {
            mod alignment;
            mod cjk;
            mod edge_cases;
            mod fixtures;
        }
        pub mod blocks {
            pub mod code;
            #[cfg(test)]
            mod code_test;
            pub mod header;
            #[cfg(test)]
            mod header_tests {
                mod fixtures;
                mod headers;
                mod horizontal_rule;
            }
            pub mod list;
            #[cfg(test)]
            mod list_test;
        }

        pub use block_parser::parse_markdown;
    }
    pub mod router;
    pub mod selection {
        pub mod highlight;
        pub mod predicate;
        pub mod wrapper;
    }
    pub mod spinner;
    pub mod theme;
    pub mod components {
        pub mod context_meter;
        pub mod header;
        pub mod input;
        pub mod login;
        pub mod picker;
        pub mod popup;
        pub mod popup_geometry;
        pub mod queue;
        pub mod status_line;
        pub mod toast;

        pub mod content {
            pub mod extractor;
            pub mod router;
            pub mod spacing;
            pub mod blocks {
                pub mod assistant;
                pub mod collapsible;
                pub mod divider;
                pub mod reasoning;
                pub mod system_notice;
                pub mod tool_call;
                pub mod user;
            }
        }
    }

    pub use router::{chrome_height, render_ui};
}

use anyhow::Result;

use crate::screens::terminal::{TerminalGuard, TerminalModeConfig};
use crate::session_setup::{OpenedSession, SessionSetup};
use crate::ui::App;

fn mouse_capture_enabled_from_env(value: Option<&str>) -> bool {
    match value.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off" | "native") => false,
        Some(value) if value.is_empty() => true,
        Some(value) => matches!(
            value.as_str(),
            "1" | "true" | "yes" | "on" | "capture" | "app"
        ),
        None => true,
    }
}

pub fn run(rt: &tokio::runtime::Runtime, setup: SessionSetup, opened: OpenedSession) -> Result<()> {
    let mouse_capture =
        mouse_capture_enabled_from_env(std::env::var("XJPCODE_MOUSE_CAPTURE").ok().as_deref());
    let mut terminal = TerminalGuard::enter(TerminalModeConfig {
        mouse_capture,
        focus_events: true,
    })?;

    let mut app = App::new();
    app.mouse_capture_enabled = mouse_capture;
    let ctx = events::context::EventContext::new(&mut app, setup, opened, rt.handle().clone());
    rt.block_on(events::runner::main_loop(&mut terminal, &mut app, ctx))
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    terminal.restore();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::mouse_capture_enabled_from_env;

    #[test]
    fn mouse_capture_defaults_to_tui_mouse_for_scroll() {
        assert!(mouse_capture_enabled_from_env(None));
        assert!(mouse_capture_enabled_from_env(Some("")));
        assert!(!mouse_capture_enabled_from_env(Some("native")));
        assert!(!mouse_capture_enabled_from_env(Some("off")));
        assert!(mouse_capture_enabled_from_env(Some("on")));
    }
}
