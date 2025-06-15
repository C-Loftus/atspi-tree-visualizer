use atspi::connection::set_session_accessibility;
use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt};

use atspi::{DocumentEvents, Event, ObjectRef, State};

use atspi_proxies::proxy_ext::ProxyExt;
use eframe::egui;
use egui::{Color32, Rangef, Rect};

use futures::executor::block_on;
use std::error::Error;
use std::sync::Arc;

use tokio::sync::mpsc::{self, UnboundedReceiver};

use tokio_stream::StreamExt;
use zbus::Connection;

/// Performs a depth-first search to collect children in the accessibility tree.
async fn dfs_collect_children(
    root: AccessibleProxy<'_>,
    conn: &Arc<Connection>,
) -> Result<Vec<ObjectRef>, Box<dyn Error>> {
    let mut stack = vec![root];
    let mut collected = Vec::new();

    while let Some(proxy) = stack.pop() {
        let children = proxy.get_children().await?;

        for child in children {
            let child_proxy = child.clone().into_accessible_proxy(&conn).await?;

            stack.push(child_proxy.clone());

            let state = block_on(child_proxy.get_state())?;

            if state.contains(State::Showing) {
                collected.push(child);
            }
        }
    }
    println!("Collected {} children", collected.len());
    Ok(collected)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let atspi = Arc::new(atspi::AccessibilityConnection::new().await?);
    let conn = Arc::new(atspi.connection().clone());

    set_session_accessibility(true).await?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_transparent(true)
            .with_decorations(false)
            .with_mouse_passthrough(true)
            .with_fullscreen(true),
        ..Default::default()
    };

    let (tx_gui, rx_gui) = mpsc::unbounded_channel();

    eframe::run_native(
        "Atspi Visualizer",
        options,
        Box::new({ 
            let atspi = atspi.clone();
            let conn = conn.clone();
            move |cc| {
                let frame = cc.egui_ctx.clone();

                tokio::spawn(async move {
                    frame.request_repaint();
                });

                let atspi_clone = atspi.clone();
                let conn_clone = conn.clone();
                let tx_gui_clone = tx_gui.clone();

                tokio::spawn(async move {
                    atspi_clone.register_event::<DocumentEvents>().await.unwrap();
                    let mut events = atspi_clone.event_stream();

                    while let Some(event) = events.next().await {
                        match event {
                            Ok(Event::Document(DocumentEvents::LoadComplete(ev))) => {
                                let conn_inner = conn_clone.clone();
                                let tx_inner = tx_gui_clone.clone();

                                tokio::spawn(async move {
                                   let a11y_proxy = ev.item.into_accessible_proxy(&conn_inner).await;
                                   match a11y_proxy {
                                      Ok(proxy) => {
                                         match dfs_collect_children(proxy, &conn_inner).await {
                                             Ok(object_refs) => {
                                                 if let Err(err) = tx_inner.send(object_refs) {
                                                     eprintln!("Error sending object refs: {err}")
                                                 }
                                             }
                                             Err(err) => eprintln!("Error collecting children: {err}"),
                                         }
                                      }
                                      Err(err) => eprintln!("Error creating proxy: {err}"),
                                   }
                                });
                            }
                            Ok(_) => println!("Other event"),
                            Err(err) => println!("Error: {err}"),
                        }
                    }
                });

                Ok(Box::new(ScreenPainterGUI::new(conn.clone(), rx_gui)))
            }
        }),
    )?;

    Ok(())
}

struct ScreenPainterGUI {
    conn: Arc<Connection>,
    points: UnboundedReceiver<Vec<ObjectRef>>,
    state: Option<Vec<ObjectRef>>,
}

impl ScreenPainterGUI {
    fn new(conn: Arc<Connection>, rx_gui: UnboundedReceiver<Vec<ObjectRef>>) -> Self {
        Self {
            conn,
            points: rx_gui,
            state: None,
        }
    }
}

impl eframe::App for ScreenPainterGUI {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));

        if let Ok(points) = self.points.try_recv() {
            if !points.is_empty() {
                self.state = Some(points);
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                if let Some(state) = &self.state {
                    println!("Rendering points: {}", state.len());

                    use futures::stream::StreamExt;

                    let stream = futures::stream::iter(state.iter()).for_each_concurrent(None, |point| {
                        let conn = self.conn.clone();
                        let painter = ui.painter();

                        async move {
                            match point.as_accessible_proxy(&conn).await {
                                Ok(proxy) => match proxy.proxies().await {
                                   Ok(proxies) => match proxies.component().await {
                                      Ok(component) => match component.get_extents(atspi::CoordType::Screen).await {
                                         Ok((x0, y0, _, _)) => {
                                             let x_range = Rangef::new(x0 as f32, (x0 as f32) + 10.0);
                                             let y_range = Rangef::new(y0 as f32, (y0 as f32) + 10.0);
                                             painter.rect_filled(
                                                 Rect::from_x_y_ranges(x_range, y_range),
                                                 0,
                                                 Color32::RED,
                                             );
                                         }
                                         Err(err) => eprintln!("Error: Failed to get extents from component: {err}"),
                                      },
                                      Err(err) => eprintln!("Error: Failed to get component from proxies: {err}"),
                                   },
                                   Err(err) => eprintln!("Error: Failed to get proxies from proxy: {err}"),
                                },
                                Err(err) => eprintln!("Error: Failed to create AccessibleProxy: {err}"),
                            }
                        }
                    });

                    block_on(stream);
                }
            });
    }
}
