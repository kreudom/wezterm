//! GuiWin represents a Gui TermWindow (as opposed to a Mux window) in lua code
use super::luaerr;
use super::pane::PaneObject;
use crate::termwindow::TermWindowNotif;
use crate::TermWindow;
use config::keyassignment::{ClipboardCopyDestination, KeyAssignment};
use luahelper::*;
use mlua::{UserData, UserDataMethods};
use mux::pane::PaneId;
use mux::window::WindowId as MuxWindowId;
use mux::Mux;
use termwiz::cell::CellAttributes;
use termwiz::surface::{Change, Line};
use termwiz_funcs::new_wezterm_terminfo_renderer;
use wezterm_dynamic::{FromDynamic, ToDynamic};
use wezterm_toast_notification::ToastNotification;
use window::{Connection, ConnectionOps, DeadKeyStatus, WindowOps, WindowState};

#[derive(Clone)]
pub struct GuiWin {
    pub mux_window_id: MuxWindowId,
    pub window: ::window::Window,
}

impl GuiWin {
    pub fn new(term_window: &TermWindow) -> Self {
        let window = term_window.window.clone().unwrap();
        let mux_window_id = term_window.mux_window_id;
        Self {
            window,
            mux_window_id,
        }
    }
}

impl UserData for GuiWin {
    fn add_methods<'lua, M: UserDataMethods<'lua, Self>>(methods: &mut M) {
        methods.add_meta_method(mlua::MetaMethod::ToString, |_, this, _: ()| {
            Ok(format!(
                "GuiWin(mux_window_id:{}, pid:{})",
                this.mux_window_id,
                unsafe { libc::getpid() }
            ))
        });

        methods.add_method("window_id", |_, this, _: ()| Ok(this.mux_window_id));
        methods.add_method("mux_window", |_, this, _: ()| {
            Ok(mux_lua::MuxWindow(this.mux_window_id))
        });
        methods.add_method(
            "set_inner_size",
            |_, this, (width, height): (usize, usize)| {
                this.window.set_inner_size(width, height);
                Ok(())
            },
        );
        methods.add_method("set_position", |_, this, (x, y): (isize, isize)| {
            this.window.set_window_position(euclid::point2(x, y));
            Ok(())
        });
        methods.add_method("maximize", |_, this, _: ()| {
            this.window.maximize();
            Ok(())
        });
        methods.add_method("restore", |_, this, _: ()| {
            this.window.restore();
            Ok(())
        });
        methods.add_method("toggle_fullscreen", |_, this, _: ()| {
            this.window.toggle_fullscreen();
            Ok(())
        });
        methods.add_method(
            "toast_notification",
            |_, _, (title, message, url, timeout): (String, String, Option<String>, Option<u64>)| {
                wezterm_toast_notification::show(ToastNotification {
                    title,
                    message,
                    url,
                    timeout: timeout.map(std::time::Duration::from_millis)
                });
                Ok(())
            },
        );
        methods.add_method("get_appearance", |_, _, _: ()| {
            Ok(Connection::get().unwrap().get_appearance().to_string())
        });
        methods.add_method("set_right_status", |_, this, status: String| {
            this.window.notify(TermWindowNotif::SetRightStatus(status));
            Ok(())
        });
        methods.add_async_method("get_dimensions", |_, this, _: ()| async move {
            let (tx, rx) = smol::channel::bounded(1);
            this.window.notify(TermWindowNotif::GetDimensions(tx));
            let (dims, window_state) = rx
                .recv()
                .await
                .map_err(|e| anyhow::anyhow!("{:#}", e))
                .map_err(luaerr)?;

            #[derive(FromDynamic, ToDynamic)]
            struct Dims {
                pixel_width: usize,
                pixel_height: usize,
                dpi: usize,
                is_full_screen: bool,
            }
            impl_lua_conversion_dynamic!(Dims);

            let dims = Dims {
                pixel_width: dims.pixel_width,
                pixel_height: dims.pixel_height,
                dpi: dims.dpi,
                is_full_screen: window_state.contains(WindowState::FULL_SCREEN),
                // FIXME: expose other states here
            };
            Ok(dims)
        });
        methods.add_async_method(
            "get_selection_text_for_pane",
            |_, this, pane: PaneObject| async move {
                let (tx, rx) = smol::channel::bounded(1);
                this.window.notify(TermWindowNotif::GetSelectionForPane {
                    pane_id: pane.pane,
                    tx,
                });
                let text = rx
                    .recv()
                    .await
                    .map_err(|e| anyhow::anyhow!("{:#}", e))
                    .map_err(luaerr)?;

                Ok(text)
            },
        );
        methods.add_method(
            "perform_action",
            |_, this, (assignment, pane): (KeyAssignment, PaneObject)| {
                this.window.notify(TermWindowNotif::PerformAssignment {
                    pane_id: pane.pane,
                    assignment,
                });
                Ok(())
            },
        );
        methods.add_async_method("effective_config", |_, this, _: ()| async move {
            let (tx, rx) = smol::channel::bounded(1);
            this.window.notify(TermWindowNotif::GetEffectiveConfig(tx));
            let config = rx
                .recv()
                .await
                .map_err(|e| anyhow::anyhow!("{:#}", e))
                .map_err(luaerr)?;

            Ok((*config).clone())
        });
        methods.add_async_method("get_config_overrides", |lua, this, _: ()| async move {
            let (tx, rx) = smol::channel::bounded(1);
            this.window.notify(TermWindowNotif::GetConfigOverrides(tx));
            let overrides = rx
                .recv()
                .await
                .map_err(|e| anyhow::anyhow!("{:#}", e))
                .map_err(luaerr)?;

            dynamic_to_lua_value(lua, overrides)
        });
        methods.add_method("set_config_overrides", |_, this, value: mlua::Value| {
            let value = lua_value_to_dynamic(value)?;
            this.window
                .notify(TermWindowNotif::SetConfigOverrides(value));
            Ok(())
        });
        methods.add_async_method("leader_is_active", |_, this, _: ()| async move {
            let (tx, rx) = smol::channel::bounded(1);
            this.window
                .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                    tx.try_send(term_window.leader_is_active()).ok();
                })));
            let result = rx
                .recv()
                .await
                .map_err(|e| anyhow::anyhow!("{:#}", e))
                .map_err(luaerr)?;

            Ok(result)
        });
        methods.add_async_method("composition_status", |_, this, _: ()| async move {
            let (tx, rx) = smol::channel::bounded(1);
            this.window
                .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                    tx.try_send(match term_window.composition_status() {
                        DeadKeyStatus::None => None,
                        DeadKeyStatus::Composing(s) => Some(s.clone()),
                    })
                    .ok();
                })));
            let result = rx
                .recv()
                .await
                .map_err(|e| anyhow::anyhow!("{:#}", e))
                .map_err(luaerr)?;

            Ok(result)
        });
        methods.add_async_method("active_key_table", |_, this, _: ()| async move {
            let (tx, rx) = smol::channel::bounded(1);
            this.window
                .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                    tx.try_send(term_window.current_key_table_name()).ok();
                })));
            let result = rx
                .recv()
                .await
                .map_err(|e| anyhow::anyhow!("{:#}", e))
                .map_err(luaerr)?;

            Ok(result)
        });
        methods.add_method("active_workspace", |_, _, _: ()| {
            let mux = Mux::get()
                .ok_or_else(|| anyhow::anyhow!("must be called on main thread"))
                .map_err(luaerr)?;
            Ok(mux.active_workspace().to_string())
        });
        methods.add_method(
            "copy_to_clipboard",
            |_, this, (text, clipboard): (String, Option<ClipboardCopyDestination>)| {
                let clipboard = clipboard.unwrap_or_default();
                this.window
                    .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                        term_window.copy_to_clipboard(clipboard, text);
                    })));
                Ok(())
            },
        );
        methods.add_async_method(
            "get_selection_escapes_for_pane",
            |_, this, pane: PaneObject| async move {
                let (tx, rx) = smol::channel::bounded(1);
                let pane_id = pane.pane;
                this.window
                    .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                        fn do_it(
                            pane_id: PaneId,
                            term_window: &mut TermWindow,
                        ) -> anyhow::Result<String> {
                            let mux = Mux::get()
                                .ok_or_else(|| anyhow::anyhow!("not called on main thread"))?;
                            let pane = mux
                                .get_pane(pane_id)
                                .ok_or_else(|| anyhow::anyhow!("invalid pane {pane_id}"))?;
                            let lines = term_window.selection_lines(&pane);
                            lines_to_escapes(lines)
                        }
                        tx.try_send(do_it(pane_id, term_window).map_err(|err| format!("{err:#}")))
                            .ok();
                    })));
                let result = rx.recv().await.map_err(mlua::Error::external)?;

                Ok(result)
            },
        );
    }
}

fn lines_to_escapes(lines: Vec<Line>) -> anyhow::Result<String> {
    let mut changes = vec![];
    let mut attr = CellAttributes::blank();
    for line in lines {
        changes.append(&mut line.changes(&attr));
        changes.push(Change::Text("\r\n".to_string()));
        if let Some(a) = line.cells().last().map(|cell| cell.attrs()) {
            attr = a.clone();
        }
    }
    changes.push(Change::AllAttributes(CellAttributes::blank()));
    let mut renderer = new_wezterm_terminfo_renderer();

    struct Target {
        target: Vec<u8>,
    }

    impl std::io::Write for Target {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.target, buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl termwiz::render::RenderTty for Target {
        fn get_size_in_cells(&mut self) -> termwiz::Result<(usize, usize)> {
            Ok((80, 24))
        }
    }

    let mut target = Target { target: vec![] };
    renderer.render_to(&changes, &mut target)?;
    Ok(String::from_utf8(target.target)?)
}
