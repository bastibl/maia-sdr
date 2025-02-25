//! User interaction with the waterfall.
//!
//! Implements the actions performed by the user to control the waterfall, such
//! as zooming or dragging to pan in frequency.

use crate::pointer::{PointerGesture, PointerTracker};
use crate::render::RenderEngine;
use crate::ui::Ui;
use crate::waterfall::Waterfall;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, PointerEvent, WheelEvent};

/// Waterfall interaction controller.
///
/// This registers events that act on the waterfall to perform the following functions:
/// * Control of zoom via on-wheel events.
/// * Control of zoom via pinch gestures generated by a [`PointerTracker`].
/// * Control of center frequency via drag gestures generated by a `PointerTracker`.
/// * Control of the cursor style according to whether the pointer is hovering or clicking
///   on the waterfall.
#[derive(Clone)]
pub struct WaterfallInteraction {
    render_engine: Rc<RefCell<RenderEngine>>,
    canvas: Rc<HtmlCanvasElement>,
    pointer_tracker: Rc<RefCell<PointerTracker>>,
    waterfall: Rc<RefCell<Waterfall>>,
    ui: Ui,
    center_freq_overflow: Rc<RefCell<f32>>,
}

impl WaterfallInteraction {
    /// Creates a waterfall interaction controller.
    ///
    /// The controller needs access to the [`RenderEngine`], in order to convert
    /// from pixels to units, to the [`Waterfall`] and its associated canvas
    /// element, and to the [`Ui`] (which is used when the RX frequency needs to
    /// be updated because of waterfall dragging).
    ///
    /// After this function returns, it is necessary to call
    /// [`WaterfallInteraction::set_callbacks`] to create and register the
    /// required event callbacks.
    pub fn new(
        render_engine: Rc<RefCell<RenderEngine>>,
        canvas: Rc<HtmlCanvasElement>,
        ui: Ui,
        waterfall: Rc<RefCell<Waterfall>>,
    ) -> WaterfallInteraction {
        WaterfallInteraction {
            render_engine,
            canvas,
            pointer_tracker: Rc::new(RefCell::new(PointerTracker::new())),
            waterfall,
            ui,
            center_freq_overflow: Rc::new(RefCell::new(0.0)),
        }
    }

    /// Sets the callbacks required by the interaction controller.
    ///
    /// This registers callbacks for the on wheel and on pointer
    /// up/down/cancel/leave/move events of the waterfall canvas.
    pub fn set_callbacks(&self) {
        // We leak all the closures produced by self to prevent them from being
        // dropped immediately.
        self.canvas
            .set_onwheel(Some(self.onwheel().into_js_value().unchecked_ref()));

        self.canvas
            .set_onpointerdown(Some(self.onpointerdown().into_js_value().unchecked_ref()));
        let onpointerup = self.onpointerup();
        self.canvas
            .set_onpointercancel(Some(onpointerup.as_ref().unchecked_ref()));
        self.canvas
            .set_onpointerout(Some(onpointerup.as_ref().unchecked_ref()));
        self.canvas
            .set_onpointerleave(Some(onpointerup.as_ref().unchecked_ref()));
        self.canvas
            .set_onpointerup(Some(onpointerup.into_js_value().unchecked_ref()));

        self.canvas
            .set_onpointermove(Some(self.onpointermove().into_js_value().unchecked_ref()));
    }

    fn clamp_zoom(zoom: f32) -> f32 {
        let min_zoom = 1.0;
        let max_zoom = 128.0;
        zoom.clamp(min_zoom, max_zoom)
    }

    fn clamp_center_frequency(frequency: f32, zoom: f32) -> f32 {
        let max_freq = 1.0 - 1.0 / zoom;
        frequency.clamp(-max_freq, max_freq)
    }

    fn units_per_px(render_engine: &RenderEngine, waterfall: &Waterfall) -> f32 {
        let canvas_width = render_engine.canvas_dims().css_pixels().0;
        let width_units = 2.0 / waterfall.get_zoom();
        width_units / canvas_width as f32
    }

    fn apply_dilation(
        render_engine: &RenderEngine,
        waterfall: &mut Waterfall,
        dilation: f32,
        center: i32,
    ) {
        let zoom = waterfall.get_zoom();
        let new_zoom = Self::clamp_zoom(dilation * zoom);
        if new_zoom == zoom {
            return;
        }
        let units_per_px = Self::units_per_px(render_engine, waterfall);
        let freq = waterfall.get_center_frequency();
        let center = freq + center as f32 * units_per_px - 1.0 / zoom;
        let freq = ((dilation - 1.0) * center + freq) / dilation;
        let freq = Self::clamp_center_frequency(freq, new_zoom);
        waterfall.set_zoom(new_zoom);
        waterfall.set_center_frequency(freq);
    }

    fn onwheel(&self) -> Closure<dyn Fn(WheelEvent)> {
        let render_engine = Rc::clone(&self.render_engine);
        let waterfall = Rc::clone(&self.waterfall);
        Closure::new(move |event: WheelEvent| {
            event.prevent_default();
            let dilation = (-1e-3 * event.delta_y() as f32).exp();
            let center = event.client_x();
            Self::apply_dilation(
                &render_engine.borrow(),
                &mut waterfall.borrow_mut(),
                dilation,
                center,
            );
        })
    }

    fn onpointerdown(&self) -> Closure<dyn Fn(PointerEvent)> {
        let canvas = Rc::clone(&self.canvas);
        let pointer_tracker = Rc::clone(&self.pointer_tracker);
        Closure::new(move |event: PointerEvent| {
            canvas.style().set_property("cursor", "col-resize").unwrap();
            pointer_tracker.borrow_mut().on_pointer_down(event);
        })
    }

    fn onpointerup(&self) -> Closure<dyn Fn(PointerEvent)> {
        let interaction = self.clone();
        Closure::new(move |event: PointerEvent| {
            let mut pointer_tracker = interaction.pointer_tracker.borrow_mut();
            pointer_tracker.on_pointer_up(event);
            if !pointer_tracker.has_active_pointers() {
                interaction
                    .canvas
                    .style()
                    .set_property("cursor", "crosshair")
                    .unwrap();
                // Reset frequency overflow when we release.
                *interaction.center_freq_overflow.borrow_mut() = 0.0;
            }
        })
    }

    fn onpointermove(&self) -> Closure<dyn Fn(PointerEvent)> {
        let interaction = self.clone();
        Closure::new(move |event: PointerEvent| {
            if let Some(gesture) = interaction
                .pointer_tracker
                .borrow_mut()
                .on_pointer_move(event)
            {
                interaction.process_gesture(gesture).unwrap();
            }
        })
    }

    fn process_gesture(&self, gesture: PointerGesture) -> Result<(), JsValue> {
        match gesture {
            PointerGesture::Drag { dx, .. } => {
                let mut waterfall = self.waterfall.borrow_mut();
                let units_per_px = Self::units_per_px(&self.render_engine.borrow(), &waterfall);
                let freq = waterfall.get_center_frequency() - (dx as f32 * units_per_px);
                let clamped = Self::clamp_center_frequency(freq, waterfall.get_zoom());
                let mut overflow = self.center_freq_overflow.borrow_mut();
                *overflow += freq - clamped;
                let shift_threshold = 0.25;
                if overflow.abs() >= shift_threshold {
                    // Change receive frequency
                    let shift = shift_threshold.copysign(*overflow);
                    *overflow -= shift;
                    let (fc, fs) = waterfall.get_freq_samprate();
                    let new_fc = fc + 0.5 * f64::from(shift) * fs;
                    self.ui.set_rx_lo_frequency(new_fc as u64)?;
                } else {
                    waterfall.set_center_frequency(clamped);
                }
            }
            PointerGesture::Pinch { center, dilation } => Self::apply_dilation(
                &self.render_engine.borrow(),
                &mut self.waterfall.borrow_mut(),
                dilation.0,
                center.0,
            ),
        }
        Ok(())
    }
}
