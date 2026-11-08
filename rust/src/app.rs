use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;

use core_graphics::access::ScreenCaptureAccess;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::*;

use crate::capture::{capture_rect, CaptureConverter};
use crate::frame::{GrayFrame, LongImageStitcher, StitchStep, MAX_CANVAS_BYTES};
use crate::geometry::{make_region, Region};
use crate::hotkey;

const HUD_WIDTH: f64 = 720.0;
const HUD_HEIGHT: f64 = 82.0;

struct Session {
    region: Region,
    converter: CaptureConverter,
    stitcher: LongImageStitcher,
    warmup: u32,
    has_content: bool,
    has_scrolled: bool,
}

#[derive(Default)]
pub struct AppIvars {
    status_item: OnceCell<Retained<NSStatusItem>>,
    menu: OnceCell<Retained<NSMenu>>,
    start_item: OnceCell<Retained<NSMenuItem>>,
    finish_item: OnceCell<Retained<NSMenuItem>>,
    adjust_item: OnceCell<Retained<NSMenuItem>>,
    cancel_item: OnceCell<Retained<NSMenuItem>>,
    open_last_item: OnceCell<Retained<NSMenuItem>>,
    selection_window: RefCell<Option<Retained<OverlayWindow>>>,
    selection_view: RefCell<Option<Retained<SelectionView>>>,
    selection_screen: RefCell<Option<Retained<NSScreen>>>,
    hud_window: RefCell<Option<Retained<NSPanel>>>,
    hud_title: RefCell<Option<Retained<NSTextField>>>,
    hud_detail: RefCell<Option<Retained<NSTextField>>>,
    hud_height: RefCell<Option<Retained<NSTextField>>>,
    capture_masks: RefCell<Vec<Retained<NSWindow>>>,
    adjust_window: RefCell<Option<Retained<OverlayWindow>>>,
    pending_resize: RefCell<Option<Region>>,
    session: RefCell<Option<Session>>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    previous_app: RefCell<Option<Retained<NSRunningApplication>>>,
    last_saved: RefCell<Option<PathBuf>>,
    capture_errors: Cell<u32>,
    match_errors: Cell<u32>,
    warning_shown: Cell<bool>,
    escape_hotkey: RefCell<Option<hotkey::EscapeHotKey>>,
}

struct SelectionIvars {
    delegate: Retained<AppDelegate>,
    drag_origin: RefCell<Option<NSPoint>>,
    selection: RefCell<NSRect>,
    showing_instructions: Cell<bool>,
    showing_size: Cell<bool>,
    title_label: Retained<NSTextField>,
    subtitle_label: Retained<NSTextField>,
    size_label: Retained<NSTextField>,
}

// MARK: - Overlay window

define_class!(
    #[unsafe(super(NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct OverlayWindow;

    unsafe impl NSObjectProtocol for OverlayWindow {}

    impl OverlayWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool { true }
        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool { false }
    }
);

impl OverlayWindow {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe {
            msg_send![
                super(this),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        }
    }
}

// MARK: - Selection view

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SelectionIvars]
    struct SelectionView;

    unsafe impl NSObjectProtocol for SelectionView {}

    impl SelectionView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw_selection(self);
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }

        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool { false }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            unsafe { Retained::retain(self as *const Self as *mut NSView) }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = event.locationInWindow();
            self.ivars().drag_origin.replace(Some(point));
            self.ivars().selection.replace(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)));
            self.ivars().showing_instructions.set(false);
            self.ivars().showing_size.set(false);
            self.ivars().title_label.setHidden(true);
            self.ivars().subtitle_label.setHidden(true);
            self.ivars().size_label.setHidden(true);
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(origin) = *self.ivars().drag_origin.borrow() else { return; };
            let current = event.locationInWindow();
            let bounds = self.bounds();
            let x0 = origin.x.min(current.x).max(0.0);
            let y0 = origin.y.min(current.y).max(0.0);
            let x1 = origin.x.max(current.x).min(bounds.size.width);
            let y1 = origin.y.max(current.y).min(bounds.size.height);
            let rect = NSRect::new(NSPoint::new(x0, y0), NSSize::new((x1 - x0).max(0.0), (y1 - y0).max(0.0)));
            self.ivars().selection.replace(rect);
            self.ivars().showing_size.set(true);
            self.ivars().size_label.setHidden(false);
            self.ivars().size_label.setStringValue(&NSString::from_str(&format!(
                "{} × {}",
                rect.size.width.round() as i64,
                rect.size.height.round() as i64
            )));
            let size = NSSize::new(138.0, 24.0);
            let mut lx = rect.origin.x;
            let mut ly = rect.origin.y + rect.size.height + 9.0;
            if lx + size.width > bounds.size.width { lx = bounds.size.width - size.width - 2.0; }
            if ly + size.height > bounds.size.height { ly = bounds.size.height - size.height - 2.0; }
            lx = lx.max(4.0);
            ly = ly.max(4.0);
            self.ivars().size_label.setFrame(NSRect::new(NSPoint::new(lx, ly), size));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            if self.ivars().drag_origin.borrow().is_none() { return; }
            self.ivars().drag_origin.replace(None);
            let selection = *self.ivars().selection.borrow();
            if selection.size.width < 60.0 || selection.size.height < 60.0 {
                self.ivars().selection.replace(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)));
                self.ivars().showing_instructions.set(true);
                self.ivars().showing_size.set(false);
                self.ivars().title_label.setHidden(false);
                self.ivars().subtitle_label.setHidden(false);
                self.ivars().size_label.setHidden(true);
                let _ = NSBeep();
                self.setNeedsDisplay(true);
                return;
            }
            let Some(window) = self.window() else { return; };
            let screen_rect = window.convertRectToScreen(selection);
            let _keep = unsafe { Retained::retain(self as *const Self as *mut SelectionView) };
            self.ivars().delegate.selection_finished(screen_rect);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.keyCode() == 53 {
                let _keep = unsafe { Retained::retain(self as *const Self as *mut SelectionView) };
                self.ivars().delegate.selection_cancelled();
            } else {
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }
    }
);

impl SelectionView {
    fn new(mtm: MainThreadMarker, frame: NSRect, delegate: Retained<AppDelegate>) -> Retained<Self> {
        let title = NSTextField::labelWithString(ns_string!("拖动选择要拼接的区域"), mtm);
        title.setFrame(NSRect::new(NSPoint::new(frame.size.width / 2.0 - 190.0, frame.size.height / 2.0 + 12.0), NSSize::new(380.0, 26.0)));
        title.setFont(Some(&NSFont::systemFontOfSize(19.0)));
        title.setTextColor(Some(&NSColor::whiteColor()));
        title.setAlignment(NSTextAlignment::Center);

        let subtitle = NSTextField::labelWithString(ns_string!("松开后滚动鼠标滚轮，内容会自动拼接"), mtm);
        subtitle.setFrame(NSRect::new(NSPoint::new(frame.size.width / 2.0 - 240.0, frame.size.height / 2.0 - 18.0), NSSize::new(480.0, 18.0)));
        subtitle.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        subtitle.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.78, 1.0)));
        subtitle.setAlignment(NSTextAlignment::Center);

        let size_label = NSTextField::labelWithString(ns_string!(""), mtm);
        size_label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        size_label.setTextColor(Some(&NSColor::whiteColor()));
        size_label.setAlignment(NSTextAlignment::Center);
        size_label.setHidden(true);

        let this = Self::alloc(mtm).set_ivars(SelectionIvars {
            delegate,
            drag_origin: RefCell::new(None),
            selection: RefCell::new(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0))),
            showing_instructions: Cell::new(true),
            showing_size: Cell::new(false),
            title_label: title,
            subtitle_label: subtitle,
            size_label,
        });
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let base: &NSView = &view;
        base.addSubview(&view.ivars().title_label);
        base.addSubview(&view.ivars().subtitle_label);
        base.addSubview(&view.ivars().size_label);
        view
    }
}

fn point_in_rect(point: NSPoint, rect: NSRect) -> bool {
    point.x >= rect.origin.x && point.x <= rect.origin.x + rect.size.width
        && point.y >= rect.origin.y && point.y <= rect.origin.y + rect.size.height
}

fn inset_rect(rect: NSRect, dx: f64, dy: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.origin.x - dx, rect.origin.y - dy),
        NSSize::new((rect.size.width + dx * 2.0).max(0.0), (rect.size.height + dy * 2.0).max(0.0)),
    )
}

fn card_rect(bounds: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(bounds.size.width / 2.0 - 230.0, bounds.size.height / 2.0 - 52.0),
        NSSize::new(460.0, 100.0),
    )
}

fn fill_rect(rect: NSRect, color: &NSColor) {
    color.setFill();
    NSBezierPath::fillRect(rect);
}

fn stroke_rect(rect: NSRect, color: &NSColor, width: f64) {
    let path = NSBezierPath::bezierPathWithRect(rect);
    path.setLineWidth(width);
    color.setStroke();
    path.stroke();
}

fn draw_selection(view: &SelectionView) {
    let bounds = view.bounds();
    let selection = *view.ivars().selection.borrow();
    let dim = NSColor::colorWithWhite_alpha(0.0, 0.30);
    if selection.size.width > 1.0 && selection.size.height > 1.0 {
        let top_h = bounds.size.height - (selection.origin.y + selection.size.height);
        fill_rect(NSRect::new(NSPoint::new(0.0, selection.origin.y + selection.size.height), NSSize::new(bounds.size.width, top_h.max(0.0))), &dim);
        fill_rect(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(bounds.size.width, selection.origin.y.max(0.0))), &dim);
        fill_rect(NSRect::new(NSPoint::new(0.0, selection.origin.y), NSSize::new(selection.origin.x.max(0.0), selection.size.height)), &dim);
        let right_x = selection.origin.x + selection.size.width;
        fill_rect(NSRect::new(NSPoint::new(right_x, selection.origin.y), NSSize::new((bounds.size.width - right_x).max(0.0), selection.size.height)), &dim);
        let accent = NSColor::colorWithSRGBRed_green_blue_alpha(0.19, 0.52, 1.0, 1.0);
        stroke_rect(inset_rect(selection, -0.75, -0.75), &accent, 1.5);
    } else {
        fill_rect(bounds, &dim);
    }

    if view.ivars().showing_instructions.get() {
        let card = card_rect(bounds);
        let card_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(card, 16.0, 16.0);
        NSColor::colorWithWhite_alpha(0.06, 0.88).setFill();
        card_path.fill();
        NSColor::colorWithWhite_alpha(1.0, 0.13).setStroke();
        card_path.setLineWidth(1.0);
        card_path.stroke();
    }

    if view.ivars().showing_size.get() {
        let label_frame = view.ivars().size_label.frame();
        let pill = inset_rect(label_frame, -9.0, -4.0);
        let pill_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(pill, 6.0, 6.0);
        NSColor::colorWithWhite_alpha(0.05, 0.9).setFill();
        pill_path.fill();
    }
}

// MARK: - Persistent capture frame

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = MaskIvars]
    struct CaptureMaskView;

    unsafe impl NSObjectProtocol for CaptureMaskView {}

    impl CaptureMaskView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            NSColor::colorWithWhite_alpha(0.0, 0.22).setFill();
            NSBezierPath::fillRect(bounds);
            let accent = NSColor::colorWithSRGBRed_green_blue_alpha(0.19, 0.52, 1.0, 1.0);
            let span_width = (self.ivars().span_end - self.ivars().span_start).max(0.0);
            let strip = match self.ivars().edge {
                0 => NSRect::new(NSPoint::new(self.ivars().span_start, 0.0), NSSize::new(span_width, 2.0)),
                1 => NSRect::new(NSPoint::new(self.ivars().span_start, bounds.size.height - 2.0), NSSize::new(span_width, 2.0)),
                2 => NSRect::new(NSPoint::new(bounds.size.width - 2.0, 0.0), NSSize::new(2.0, bounds.size.height)),
                _ => NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(2.0, bounds.size.height)),
            };
            accent.setFill();
            NSBezierPath::fillRect(strip);
        }
    }
);

struct MaskIvars {
    edge: u8,
    span_start: f64,
    span_end: f64,
}

impl CaptureMaskView {
    fn new(mtm: MainThreadMarker, frame: NSRect, edge: u8, span_start: f64, span_end: f64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MaskIvars { edge, span_start, span_end });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

// MARK: - HUD view

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = HudIvars]
    struct HUDView;

    unsafe impl NSObjectProtocol for HUDView {}

    impl HUDView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                NSRect::new(NSPoint::new(0.5, 0.5), NSSize::new(bounds.size.width - 1.0, bounds.size.height - 1.0)),
                14.0, 14.0,
            );
            NSColor::colorWithWhite_alpha(0.055, 0.95).setFill();
            path.fill();
            NSColor::colorWithWhite_alpha(1.0, 0.16).setStroke();
            path.setLineWidth(1.0);
            path.stroke();

            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 0.26, 0.30, 1.0).setFill();
            NSBezierPath::bezierPathWithOvalInRect(NSRect::new(NSPoint::new(20.0, 49.0), NSSize::new(9.0, 9.0))).fill();

            let finish = hud_finish_rect(bounds);
            let finish_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(finish, 9.0, 9.0);
            NSColor::colorWithSRGBRed_green_blue_alpha(0.17, 0.48, 1.0, 0.96).setFill();
            finish_path.fill();
            NSColor::colorWithWhite_alpha(1.0, 0.22).setStroke();
            finish_path.setLineWidth(1.0);
            finish_path.stroke();

            let adjust = hud_adjust_rect(bounds);
            let adjust_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(adjust, 9.0, 9.0);
            NSColor::colorWithWhite_alpha(1.0, 0.15).setFill();
            adjust_path.fill();
            NSColor::colorWithWhite_alpha(1.0, 0.22).setStroke();
            adjust_path.setLineWidth(1.0);
            adjust_path.stroke();

            let cancel = hud_cancel_rect(bounds);
            let cancel_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(cancel, 9.0, 9.0);
            NSColor::colorWithWhite_alpha(1.0, 0.13).setFill();
            cancel_path.fill();
            NSColor::colorWithWhite_alpha(1.0, 0.18).setStroke();
            cancel_path.setLineWidth(1.0);
            cancel_path.stroke();
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            unsafe { Retained::retain(self as *const Self as *mut NSView) }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }

        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool { false }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = event.locationInWindow();
            let bounds = self.bounds();
            let _keep = unsafe { Retained::retain(self as *const Self as *mut HUDView) };
            if point_in_rect(point, hud_finish_rect(bounds)) {
                self.ivars().delegate.finish_session();
            } else if point_in_rect(point, hud_adjust_rect(bounds)) {
                self.ivars().delegate.adjust_bottom_region();
            } else if point_in_rect(point, hud_cancel_rect(bounds)) {
                self.ivars().delegate.cancel_session();
            }
        }
    }
);

struct HudIvars {
    delegate: Retained<AppDelegate>,
    finish_label: Retained<NSTextField>,
    adjust_label: Retained<NSTextField>,
    cancel_label: Retained<NSTextField>,
}

fn hud_finish_rect(bounds: NSRect) -> NSRect {
    NSRect::new(NSPoint::new(bounds.size.width - 210.0, 22.0), NSSize::new(110.0, 38.0))
}

fn hud_adjust_rect(bounds: NSRect) -> NSRect {
    NSRect::new(NSPoint::new(bounds.size.width - 290.0, 22.0), NSSize::new(80.0, 38.0))
}

fn hud_cancel_rect(bounds: NSRect) -> NSRect {
    NSRect::new(NSPoint::new(bounds.size.width - 90.0, 22.0), NSSize::new(70.0, 38.0))
}

impl HUDView {
    fn new(mtm: MainThreadMarker, frame: NSRect, delegate: Retained<AppDelegate>) -> Retained<Self> {
        let finish_label = NSTextField::labelWithString(ns_string!("完成并保存"), mtm);
        finish_label.setFont(Some(&NSFont::systemFontOfSize(12.5)));
        finish_label.setTextColor(Some(&NSColor::whiteColor()));
        finish_label.setAlignment(NSTextAlignment::Center);
        let adjust_label = NSTextField::labelWithString(ns_string!("调整底部"), mtm);
        adjust_label.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        adjust_label.setTextColor(Some(&NSColor::whiteColor()));
        adjust_label.setAlignment(NSTextAlignment::Center);
        let cancel_label = NSTextField::labelWithString(ns_string!("取消"), mtm);
        cancel_label.setFont(Some(&NSFont::systemFontOfSize(12.5)));
        cancel_label.setTextColor(Some(&NSColor::whiteColor()));
        cancel_label.setAlignment(NSTextAlignment::Center);

        let this = Self::alloc(mtm).set_ivars(HudIvars { delegate, finish_label, adjust_label, cancel_label });
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let base: &NSView = &view;
        let finish_rect = hud_finish_rect(frame);
        let adjust_rect = hud_adjust_rect(frame);
        let cancel_rect = hud_cancel_rect(frame);
        view.ivars().finish_label.setFrame(NSRect::new(
            NSPoint::new(finish_rect.origin.x, finish_rect.origin.y + 9.0),
            NSSize::new(finish_rect.size.width, 20.0),
        ));
        view.ivars().adjust_label.setFrame(NSRect::new(
            NSPoint::new(adjust_rect.origin.x, adjust_rect.origin.y + 9.0),
            NSSize::new(adjust_rect.size.width, 20.0),
        ));
        view.ivars().cancel_label.setFrame(NSRect::new(
            NSPoint::new(cancel_rect.origin.x, cancel_rect.origin.y + 9.0),
            NSSize::new(cancel_rect.size.width, 20.0),
        ));
        base.addSubview(&view.ivars().finish_label);
        base.addSubview(&view.ivars().adjust_label);
        base.addSubview(&view.ivars().cancel_label);
        view
    }
}

// MARK: - Bottom edge adjuster

struct ResizeIvars {
    delegate: Retained<AppDelegate>,
    region: Cell<NSRect>,
    screen: NSRect,
    scale: f64,
    dragging: Cell<bool>,
    title_label: Retained<NSTextField>,
    detail_label: Retained<NSTextField>,
    size_label: Retained<NSTextField>,
    apply_label: Retained<NSTextField>,
    cancel_label: Retained<NSTextField>,
}

fn resize_card_rect(bounds: NSRect) -> NSRect {
    let w = 680.0;
    let h = 104.0;
    NSRect::new(
        NSPoint::new((bounds.size.width - w) / 2.0, bounds.size.height - h - 18.0),
        NSSize::new(w, h),
    )
}

fn resize_apply_rect(bounds: NSRect) -> NSRect {
    let card = resize_card_rect(bounds);
    NSRect::new(NSPoint::new(card.origin.x + 470.0, card.origin.y + 35.0), NSSize::new(100.0, 34.0))
}

fn resize_cancel_rect(bounds: NSRect) -> NSRect {
    let card = resize_card_rect(bounds);
    NSRect::new(NSPoint::new(card.origin.x + 580.0, card.origin.y + 35.0), NSSize::new(80.0, 34.0))
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ResizeIvars]
    struct ResizeBottomView;

    unsafe impl NSObjectProtocol for ResizeBottomView {}

    impl ResizeBottomView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let region = self.ivars().region.get();
            let dim = NSColor::colorWithWhite_alpha(0.0, 0.28);

            let top_h = bounds.size.height - (region.origin.y + region.size.height);
            if top_h > 0.0 {
                dim.setFill();
                NSBezierPath::fillRect(NSRect::new(
                    NSPoint::new(0.0, region.origin.y + region.size.height),
                    NSSize::new(bounds.size.width, top_h),
                ));
            }
            if region.origin.y > 0.0 {
                dim.setFill();
                NSBezierPath::fillRect(NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(bounds.size.width, region.origin.y),
                ));
            }
            if region.origin.x > 0.0 {
                dim.setFill();
                NSBezierPath::fillRect(NSRect::new(
                    NSPoint::new(0.0, region.origin.y),
                    NSSize::new(region.origin.x, region.size.height),
                ));
            }
            let right_w = bounds.size.width - (region.origin.x + region.size.width);
            if right_w > 0.0 {
                dim.setFill();
                NSBezierPath::fillRect(NSRect::new(
                    NSPoint::new(region.origin.x + region.size.width, region.origin.y),
                    NSSize::new(right_w, region.size.height),
                ));
            }

            let accent = NSColor::colorWithSRGBRed_green_blue_alpha(0.19, 0.52, 1.0, 1.0);
            accent.setStroke();
            let border = NSBezierPath::bezierPathWithRect(region);
            border.setLineWidth(2.0);
            border.stroke();

            let pill_w = 110.0;
            let pill_h = 18.0;
            let pill = NSRect::new(
                NSPoint::new(region.origin.x + region.size.width / 2.0 - pill_w / 2.0, region.origin.y - pill_h / 2.0),
                NSSize::new(pill_w, pill_h),
            );
            let pill_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(pill, 9.0, 9.0);
            accent.setFill();
            pill_path.fill();
            NSColor::colorWithWhite_alpha(1.0, 0.72).setFill();
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(pill.origin.x + 34.0, pill.origin.y + 8.0),
                NSSize::new(pill_w - 68.0, 2.0),
            ));

            let card = resize_card_rect(bounds);
            let card_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(card, 14.0, 14.0);
            NSColor::colorWithWhite_alpha(0.06, 0.96).setFill();
            card_path.fill();
            NSColor::colorWithWhite_alpha(1.0, 0.16).setStroke();
            card_path.setLineWidth(1.0);
            card_path.stroke();

            let apply = resize_apply_rect(bounds);
            let apply_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(apply, 9.0, 9.0);
            NSColor::colorWithSRGBRed_green_blue_alpha(0.17, 0.48, 1.0, 0.96).setFill();
            apply_path.fill();
            let cancel = resize_cancel_rect(bounds);
            let cancel_path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(cancel, 9.0, 9.0);
            NSColor::colorWithWhite_alpha(1.0, 0.14).setFill();
            cancel_path.fill();
            NSColor::colorWithWhite_alpha(1.0, 0.20).setStroke();
            cancel_path.setLineWidth(1.0);
            cancel_path.stroke();
        }

        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            unsafe { Retained::retain(self as *const Self as *mut NSView) }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool { true }

        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool { false }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let _keep = unsafe { Retained::retain(self as *const Self as *mut ResizeBottomView) };
            let point = event.locationInWindow();
            let bounds = self.bounds();
            if point_in_rect(point, resize_apply_rect(bounds)) {
                self.ivars().delegate.adjust_applied(self.global_rect());
                return;
            }
            if point_in_rect(point, resize_cancel_rect(bounds)) {
                self.ivars().delegate.adjust_cancelled();
                return;
            }
            let region = self.ivars().region.get();
            if point.x >= region.origin.x
                && point.x <= region.origin.x + region.size.width
                && (point.y - region.origin.y).abs() <= 24.0
            {
                self.ivars().dragging.set(true);
                NSCursor::resizeUpDownCursor().set();
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            if !self.ivars().dragging.get() { return; }
            let point = event.locationInWindow();
            let mut region = self.ivars().region.get();
            let top = region.origin.y + region.size.height;
            let new_bottom = point.y.clamp(0.0, top - 60.0);
            region.origin.y = new_bottom;
            region.size.height = top - new_bottom;
            self.ivars().region.set(region);
            self.update_size();
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            self.ivars().dragging.set(false);
            NSCursor::arrowCursor().set();
        }
    }
);

impl ResizeBottomView {
    fn new(mtm: MainThreadMarker, frame: NSRect, screen: NSRect, region: NSRect, scale: f64, delegate: Retained<AppDelegate>) -> Retained<Self> {
        let card = resize_card_rect(frame);
        let title_label = NSTextField::labelWithString(ns_string!("调整捕获下边缘"), mtm);
        title_label.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        title_label.setTextColor(Some(&NSColor::whiteColor()));
        title_label.setFrame(NSRect::new(NSPoint::new(card.origin.x + 24.0, card.origin.y + 62.0), NSSize::new(430.0, 20.0)));
        let detail_label = NSTextField::labelWithString(ns_string!("上下拖动蓝色下边缘，点“应用调整”重新拼接底部"), mtm);
        detail_label.setFont(Some(&NSFont::systemFontOfSize(11.5)));
        detail_label.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.72, 1.0)));
        detail_label.setFrame(NSRect::new(NSPoint::new(card.origin.x + 24.0, card.origin.y + 38.0), NSSize::new(440.0, 16.0)));
        let size_label = NSTextField::labelWithString(ns_string!(""), mtm);
        size_label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        size_label.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.64, 1.0)));
        size_label.setFrame(NSRect::new(NSPoint::new(card.origin.x + 24.0, card.origin.y + 20.0), NSSize::new(240.0, 14.0)));

        let apply = resize_apply_rect(frame);
        let apply_label = NSTextField::labelWithString(ns_string!("应用调整"), mtm);
        apply_label.setFont(Some(&NSFont::systemFontOfSize(12.5)));
        apply_label.setTextColor(Some(&NSColor::whiteColor()));
        apply_label.setAlignment(NSTextAlignment::Center);
        apply_label.setFrame(NSRect::new(NSPoint::new(apply.origin.x, apply.origin.y + 8.0), NSSize::new(apply.size.width, 18.0)));
        let cancel = resize_cancel_rect(frame);
        let cancel_label = NSTextField::labelWithString(ns_string!("取消"), mtm);
        cancel_label.setFont(Some(&NSFont::systemFontOfSize(12.5)));
        cancel_label.setTextColor(Some(&NSColor::whiteColor()));
        cancel_label.setAlignment(NSTextAlignment::Center);
        cancel_label.setFrame(NSRect::new(NSPoint::new(cancel.origin.x, cancel.origin.y + 8.0), NSSize::new(cancel.size.width, 18.0)));

        let this = Self::alloc(mtm).set_ivars(ResizeIvars {
            delegate,
            region: Cell::new(region),
            screen,
            scale,
            dragging: Cell::new(false),
            title_label,
            detail_label,
            size_label,
            apply_label,
            cancel_label,
        });
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let base: &NSView = &view;
        base.addSubview(&view.ivars().title_label);
        base.addSubview(&view.ivars().detail_label);
        base.addSubview(&view.ivars().size_label);
        base.addSubview(&view.ivars().apply_label);
        base.addSubview(&view.ivars().cancel_label);
        view.update_size();
        view
    }

    fn update_size(&self) {
        let region = self.ivars().region.get();
        let px = (region.size.height * self.ivars().scale).round() as usize;
        self.ivars().size_label.setStringValue(&NSString::from_str(&format!("当前高度 {} px", px)));
    }

    fn global_rect(&self) -> NSRect {
        let region = self.ivars().region.get();
        let screen = self.ivars().screen;
        NSRect::new(
            NSPoint::new(screen.origin.x + region.origin.x, screen.origin.y + region.origin.y),
            region.size,
        )
    }
}

// MARK: - App delegate

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppIvars]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = self.mtm();
            let app = NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            self.install_status_item();
            {
                let app = self.retained_clone();
                hotkey::install_handler(Box::new(move || app.escape_pressed()));
            }






        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            self.stop_timer();
            self.unregister_escape();
        }
    }

    impl AppDelegate {
        #[unsafe(method(statusClicked:))]
        fn status_clicked(&self, _sender: &AnyObject) {
            let app = NSApplication::sharedApplication(self.mtm());
            if let Some(event) = app.currentEvent() {
                if event.r#type() == NSEventType::RightMouseUp
                    || event.modifierFlags().contains(NSEventModifierFlags::Control) {
                    self.show_menu();
                    return;
                }
            }
            self.begin_selection();
        }

        #[unsafe(method(startFromMenu:))]
        fn start_from_menu(&self, _sender: &AnyObject) {
            self.begin_selection();
        }

        #[unsafe(method(finishFromMenu:))]
        fn finish_from_menu(&self, _sender: &AnyObject) {
            self.finish_session();
        }

        #[unsafe(method(adjustFromMenu:))]
        fn adjust_from_menu(&self, _sender: &AnyObject) {
            self.adjust_bottom_region();
        }

        #[unsafe(method(performPendingResize:))]
        fn perform_pending_resize_timer(&self, _timer: &NSTimer) {
            self.perform_pending_resize();
        }

        #[unsafe(method(cancelFromMenu:))]
        fn cancel_from_menu(&self, _sender: &AnyObject) {
            self.cancel_session();
        }

        #[unsafe(method(helpFromMenu:))]
        fn help_from_menu(&self, _sender: &AnyObject) {
            self.show_help();
        }

        #[unsafe(method(permissionsFromMenu:))]
        fn permissions_from_menu(&self, _sender: &AnyObject) {
            let _ = std::process::Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
                .status();
        }

        #[unsafe(method(openLastFromMenu:))]
        fn open_last_from_menu(&self, _sender: &AnyObject) {
            self.open_last_saved();
        }

        #[unsafe(method(hudFinish:))]
        fn hud_finish(&self, sender: &NSButton) {
            let _keep = unsafe { Retained::retain(sender as *const NSButton as *mut NSButton) };
            self.finish_session();
        }

        #[unsafe(method(hudCancel:))]
        fn hud_cancel(&self, sender: &NSButton) {
            let _keep = unsafe { Retained::retain(sender as *const NSButton as *mut NSButton) };
            self.cancel_session();
        }

        #[unsafe(method(captureTick:))]
        fn capture_tick(&self, _timer: &NSTimer) {
            self.tick();
        }

        #[unsafe(method(makeSelectionKey:))]
        fn make_selection_key(&self, _timer: &NSTimer) {
            if let Some(window) = self.ivars().selection_window.borrow().as_ref() {
                window.makeKeyWindow();
                if let Some(view) = self.ivars().selection_view.borrow().as_ref() {
                    let base: &NSView = view;
                    let _ = window.makeFirstResponder(Some(base));
                }
            }
        }

    }
);

impl AppDelegate {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    fn install_status_item(&self) {
        let mtm = self.mtm();
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(-1.0);
        if let Some(button) = item.button(mtm) {
            button.setTitle(ns_string!("Longshot"));
            if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("rectangle.stack"),
                Some(ns_string!("长截图")),
            ) {
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            unsafe {
                button.setTarget(Some(self));
                button.setAction(Some(sel!(statusClicked:)));
            }
            button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        }
        self.ivars().status_item.set(item).ok();

        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
        let start = menu_item(mtm, ns_string!("开始截取"), sel!(startFromMenu:), self);
        let finish = menu_item(mtm, ns_string!("完成并保存"), sel!(finishFromMenu:), self);
        finish.setEnabled(false);
        let adjust = menu_item(mtm, ns_string!("调整捕获下边缘…"), sel!(adjustFromMenu:), self);
        adjust.setEnabled(false);
        let cancel = menu_item(mtm, ns_string!("取消当前截取"), sel!(cancelFromMenu:), self);
        cancel.setEnabled(false);
        let open_last = menu_item(mtm, ns_string!("打开最近保存的图片"), sel!(openLastFromMenu:), self);
        open_last.setEnabled(false);
        let help = menu_item(mtm, ns_string!("使用说明…"), sel!(helpFromMenu:), self);
        let perm = menu_item(mtm, ns_string!("屏幕录制权限…"), sel!(permissionsFromMenu:), self);
        menu.addItem(&start);
        menu.addItem(&finish);
        menu.addItem(&adjust);
        menu.addItem(&cancel);
        menu.addItem(&open_last);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&help);
        menu.addItem(&perm);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let quit = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("退出 Longshot"),
                Some(sel!(terminate:)),
                ns_string!("q"),
            )
        };
        menu.addItem(&quit);

        self.ivars().menu.set(menu).ok();
        self.ivars().start_item.set(start).ok();
        self.ivars().finish_item.set(finish).ok();
        self.ivars().adjust_item.set(adjust).ok();
        self.ivars().cancel_item.set(cancel).ok();
        self.ivars().open_last_item.set(open_last).ok();
    }

    fn show_menu(&self) {
        let mtm = self.mtm();
        let Some(item) = self.ivars().status_item.get() else { return; };
        let Some(menu) = self.ivars().menu.get() else { return; };
        let Some(button) = item.button(mtm) else { return; };
        let b = button.bounds();
        menu.popUpMenuPositioningItem_atLocation_inView(
            None,
            NSPoint::new(0.0, b.size.height + 4.0),
            Some(&button),
        );
    }

    fn adjust_bottom_region(&self) {
        if self.ivars().adjust_window.borrow().is_some() { return; }
        let Some(region) = self.ivars().session.borrow().as_ref().map(|s| s.region.clone()) else {
            let _ = NSBeep();
            return;
        };
        self.stop_timer();
        self.hide_hud();
        self.hide_capture_masks();

        let mtm = self.mtm();
        let screen = region.screen_frame;
        let local = NSRect::new(
            NSPoint::new(region.cocoa_rect.origin.x - screen.origin.x, region.cocoa_rect.origin.y - screen.origin.y),
            region.cocoa_rect.size,
        );
        let window = OverlayWindow::new(mtm, screen);
        window.setLevel(NSStatusWindowLevel + 2);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setOpaque(false);
        window.setHasShadow(false);
        unsafe { window.setReleasedWhenClosed(false) };
        window.setHidesOnDeactivate(false);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        let view = ResizeBottomView::new(
            mtm,
            NSRect::new(NSPoint::new(0.0, 0.0), screen.size),
            screen,
            local,
            region.scale,
            self.retained_clone(),
        );
        let base: &NSView = &view;
        window.setContentView(Some(base));
        self.activate_app();
        window.makeKeyAndOrderFront(None);
        window.orderFrontRegardless();
        self.ivars().adjust_window.replace(Some(window));
    }

    fn adjust_cancelled(&self) {
        if let Some(window) = self.ivars().adjust_window.borrow_mut().take() {
            window.orderOut(None);
        }
        NSCursor::arrowCursor().set();
        self.ivars().pending_resize.borrow_mut().take();
        let Some(region) = self.ivars().session.borrow().as_ref().map(|s| s.region.clone()) else { return; };
        self.show_capture_masks(&region);
        self.show_hud(&region);
        if let Some(session) = self.ivars().session.borrow().as_ref() {
            self.update_hud(session.stitcher.height, session.stitcher.screen_count(), Some("继续缓慢滚动，内容会自动拼接"), false);
        }
        self.restore_previous_app();
        self.start_timer();
    }

    fn adjust_applied(&self, cocoa_rect: NSRect) {
        if let Some(window) = self.ivars().adjust_window.borrow_mut().take() {
            window.orderOut(None);
        }
        NSCursor::arrowCursor().set();
        let Some(old) = self.ivars().session.borrow().as_ref().map(|s| s.region.clone()) else { return; };
        let main_height = NSScreen::mainScreen(self.mtm()).map(|s| s.frame().size.height).unwrap_or(old.screen_frame.size.height);
        let Some(new_region) = make_region(cocoa_rect, old.screen_frame, old.visible_frame, main_height, old.scale) else {
            self.adjust_cancelled();
            return;
        };
        if new_region.pixel_width != old.pixel_width {
            self.adjust_cancelled();
            return;
        }
        self.ivars().pending_resize.replace(Some(new_region));
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.15,
                {
                    let target: &AnyObject = self;
                    target
                },
                sel!(performPendingResize:),
                None,
                false,
            );
        }
    }

    fn perform_pending_resize(&self) {
        let Some(new_region) = self.ivars().pending_resize.borrow_mut().take() else { return; };
        let Some(image) = capture_rect(new_region.cg_rect) else {
            self.adjust_cancelled();
            return;
        };
        let Some(mut converter) = CaptureConverter::new(new_region.pixel_width, new_region.pixel_height) else {
            self.adjust_cancelled();
            return;
        };
        let (rgba, w, h) = converter.convert(&image);
        let gray_width = (w / 14).clamp(96, 192);
        let Some(gray) = GrayFrame::new(&rgba, w, h, gray_width) else {
            self.adjust_cancelled();
            return;
        };
        let mut ok = false;
        if let Some(session) = self.ivars().session.borrow_mut().as_mut() {
            if new_region.pixel_width == session.region.pixel_width {
                if session.stitcher.resize_viewport(h, &rgba, gray).is_some() {
                    session.region = new_region.clone();
                    session.converter = converter;
                    session.warmup = 1;
                    session.has_content = true;
                    ok = true;
                }
            }
        }
        if !ok {
            self.adjust_cancelled();
            return;
        }
        self.show_capture_masks(&new_region);
        self.show_hud(&new_region);
        if let Some(session) = self.ivars().session.borrow().as_ref() {
            self.update_hud(session.stitcher.height, session.stitcher.screen_count(), Some("已调整捕获底部 · 可继续滚动或完成"), false);
        }
        self.restore_previous_app();
        self.start_timer();
    }

    fn escape_pressed(&self) {
        if self.ivars().selection_window.borrow().is_some() {
            self.selection_cancelled();
        } else if self.ivars().adjust_window.borrow().is_some() {
            self.adjust_cancelled();
        } else if self.ivars().pending_resize.borrow().is_some() {
            return;
        } else if self.ivars().session.borrow().is_some() {
            self.cancel_session();
        }
    }

    fn register_escape(&self) {
        if self.ivars().escape_hotkey.borrow().is_none() {
            self.ivars().escape_hotkey.replace(hotkey::EscapeHotKey::register());
        }
    }

    fn unregister_escape(&self) {
        let _ = self.ivars().escape_hotkey.borrow_mut().take();
    }

    fn begin_selection(&self) {
        if self.ivars().session.borrow().is_some() {
            self.show_menu();
            return;
        }
        if self.ivars().selection_window.borrow().is_some() {
            let _ = NSBeep();
            return;
        }
        let access = ScreenCaptureAccess;
        if !access.preflight() {
            let granted = access.request();
            if !granted {
                self.show_alert(
                    "需要屏幕录制权限",
                    "Longshot 需要读取屏幕内容，才能把滚动区域拼成一张长图。\n\n请打开“系统设置 → 隐私与安全性 → 屏幕录制”，勾选 Longshot，然后重新打开应用。",
                );
                return;
            }
        }

        let mtm = self.mtm();
        let workspace = NSWorkspace::sharedWorkspace();
        if let Some(front) = workspace.frontmostApplication() {
            if front.processIdentifier() != std::process::id() as i32 {
                self.ivars().previous_app.replace(Some(front));
            }
        }

        let mouse = NSEvent::mouseLocation();
        let mut chosen: Option<Retained<NSScreen>> = NSScreen::mainScreen(mtm);
        for screen in NSScreen::screens(mtm).iter() {
            let f = screen.frame();
            if mouse.x >= f.origin.x && mouse.x < f.origin.x + f.size.width
                && mouse.y >= f.origin.y && mouse.y < f.origin.y + f.size.height {
                chosen = Some(screen.clone());
                break;
            }
        }
        let Some(screen) = chosen else { return; };
        let screen_frame = screen.frame();
        let visible_frame = screen.visibleFrame();
        let scale = screen.backingScaleFactor();

        let window = OverlayWindow::new(mtm, screen_frame);
        window.setLevel(NSStatusWindowLevel + 1);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setOpaque(false);
        window.setHasShadow(false);
        unsafe { window.setReleasedWhenClosed(false) };
        window.setHidesOnDeactivate(false);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );

        let view = SelectionView::new(mtm, NSRect::new(NSPoint::new(0.0, 0.0), screen_frame.size), self.retained_clone());
        let base: &NSView = &view;
        window.setContentView(Some(base));
        let _ = window.makeFirstResponder(Some(base));
        window.makeKeyAndOrderFront(None);
        window.orderFrontRegardless();

        NSCursor::crosshairCursor().set();
        self.ivars().selection_screen.replace(Some(screen));
        self.ivars().selection_view.replace(Some(view));
        self.ivars().selection_window.replace(Some(window.clone()));

        self.activate_app();
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.06,
                {
                    let target: &AnyObject = self;
                    target
                },
                sel!(makeSelectionKey:),
                None,
                false,
            );
        }
        self.register_escape();
    }

    fn selection_screen_values(&self) -> Option<(NSRect, NSRect, f64, Retained<NSScreen>, f64)> {
        let screen = self.ivars().selection_screen.borrow().clone()?;
        let frame = screen.frame();
        let visible = screen.visibleFrame();
        let scale = screen.backingScaleFactor();
        let main_height = NSScreen::mainScreen(self.mtm()).map(|s| s.frame().size.height).unwrap_or(frame.size.height);
        Some((frame, visible, scale, screen, main_height))
    }

    fn selection_finished(&self, screen_rect: NSRect) {
        let Some((frame, visible, scale, _screen, main_height)) = self.selection_screen_values() else {
            self.dismiss_selection();
            return;
        };
        let region = make_region(screen_rect, frame, visible, main_height, scale);
        self.dismiss_selection();
        if region.is_none() {
            self.unregister_escape();
            return;
        }
        self.restore_previous_app();
        self.start_session(region.unwrap());
    }

    fn selection_cancelled(&self) {
        self.unregister_escape();
        self.dismiss_selection();
        self.restore_previous_app();
    }

    fn dismiss_selection(&self) {
        NSCursor::arrowCursor().set();
        if let Some(window) = self.ivars().selection_window.borrow_mut().take() {
            window.orderOut(None);
        }
        self.ivars().selection_view.borrow_mut().take();
        self.ivars().selection_screen.borrow_mut().take();
    }

    fn show_capture_masks(&self, region: &Region) {
        self.hide_capture_masks();
        let mtm = self.mtm();
        let s = region.screen_frame;
        let r = region.cocoa_rect;
        let top_h = (s.origin.y + s.size.height) - (r.origin.y + r.size.height);
        let bottom_h = r.origin.y - s.origin.y;
        let left_w = r.origin.x - s.origin.x;
        let right_w = (s.origin.x + s.size.width) - (r.origin.x + r.size.width);

        let mut frames: Vec<(NSRect, u8, f64, f64)> = Vec::new();
        let span_start = r.origin.x - s.origin.x;
        let span_end = span_start + r.size.width;
        if top_h > 0.0 {
            frames.push((NSRect::new(NSPoint::new(s.origin.x, r.origin.y + r.size.height), NSSize::new(s.size.width, top_h)), 0, span_start, span_end));
        }
        if bottom_h > 0.0 {
            frames.push((NSRect::new(NSPoint::new(s.origin.x, s.origin.y), NSSize::new(s.size.width, bottom_h)), 1, span_start, span_end));
        }
        if left_w > 0.0 {
            frames.push((NSRect::new(NSPoint::new(s.origin.x, r.origin.y), NSSize::new(left_w, r.size.height)), 2, 0.0, left_w));
        }
        if right_w > 0.0 {
            frames.push((NSRect::new(NSPoint::new(r.origin.x + r.size.width, r.origin.y), NSSize::new(right_w, r.size.height)), 3, 0.0, right_w));
        }

        for (frame, edge, span_start, span_end) in frames {
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    frame,
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            window.setLevel(NSStatusWindowLevel - 1);
            window.setBackgroundColor(Some(&NSColor::clearColor()));
            window.setOpaque(false);
            window.setHasShadow(false);
            unsafe { window.setReleasedWhenClosed(false) };
            window.setHidesOnDeactivate(false);
            window.setIgnoresMouseEvents(true);
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle,
            );
            let view = CaptureMaskView::new(
                mtm,
                NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
                edge,
                span_start,
                span_end,
            );
            let base: &NSView = &view;
            window.setContentView(Some(base));
            window.orderFrontRegardless();
            self.ivars().capture_masks.borrow_mut().push(window);
        }
    }

    fn hide_capture_masks(&self) {
        for window in self.ivars().capture_masks.borrow_mut().drain(..) {
            window.orderOut(None);
        }
    }

    fn start_session(&self, region: Region) {
        let Some(converter) = CaptureConverter::new(region.pixel_width, region.pixel_height) else { return; };
        let session = Session {
            region: region.clone(),
            converter,
            stitcher: LongImageStitcher::new(MAX_CANVAS_BYTES),
            warmup: 3,
            has_content: false,
            has_scrolled: false,
        };
        self.show_capture_masks(&session.region);
        self.show_hud(&session.region);
        self.ivars().capture_errors.set(0);
        self.ivars().match_errors.set(0);
        self.ivars().warning_shown.set(false);
        self.ivars().session.replace(Some(session));
        self.set_capturing(true);
        self.register_escape();
        self.start_timer();
    }

    fn tick(&self) {
        let mut update: Option<(usize, usize, Option<String>, bool)> = None;
        let mut fail_message: Option<String> = None;
        let mut limit = false;
        {
            let mut guard = self.ivars().session.borrow_mut();
            let Some(session) = guard.as_mut() else { return; };
            if session.warmup > 0 {
                session.warmup -= 1;
                return;
            }
            let image = capture_rect(session.region.cg_rect);
            if image.is_some() {
                self.ivars().capture_errors.set(0);
            } else {
                let errors = self.ivars().capture_errors.get() + 1;
                self.ivars().capture_errors.set(errors);
                if errors >= 5 {
                    fail_message = Some("连续多次无法读取屏幕，请检查屏幕录制权限。".to_string());
                }
            }
            if let Some(image) = image {
                let (rgba, w, h) = session.converter.convert(&image);
                let gray_width = (w / 14).clamp(96, 192);
                if let Some(gray) = GrayFrame::new(&rgba, w, h, gray_width) {
                    let step = if session.stitcher.is_started() {
                        session.stitcher.ingest(&rgba, w, h, gray)
                    } else {
                        session.stitcher.begin(&rgba, w, h, gray)
                    };
                    match step {
                        StitchStep::Initialized { height } => {
                            session.has_content = true;
                            session.has_scrolled = false;
                            self.ivars().match_errors.set(0);
                            self.ivars().warning_shown.set(false);
                            update = Some((height, session.stitcher.screen_count(), Some("开始滚动鼠标滚轮，内容会自动拼接".into()), false));
                        }
                        StitchStep::Stitched { added_rows, trimmed_rows, shift_rows } => {
                            if shift_rows != 0 { session.has_scrolled = true; }
                            self.ivars().match_errors.set(0);
                            self.ivars().warning_shown.set(false);
                            if trimmed_rows > 0 {
                                update = Some((session.stitcher.height, session.stitcher.screen_count(), Some("已回退，长图底部已裁到当前画面".into()), false));
                            } else if added_rows > 0 {
                                update = Some((session.stitcher.height, session.stitcher.screen_count(), Some("已捕获新内容 · 继续缓慢滚动".into()), false));
                            }
                        }
                        StitchStep::Failed => {
                            let errors = self.ivars().match_errors.get() + 1;
                            self.ivars().match_errors.set(errors);
                            if session.has_content && session.has_scrolled && errors >= 8 && !self.ivars().warning_shown.get() {
                                self.ivars().warning_shown.set(true);
                                update = Some((session.stitcher.height, session.stitcher.screen_count(), Some("这一段滚动没有识别成功 · 请慢一点再试".into()), false));
                            }
                        }
                        StitchStep::LimitReached => {
                            limit = true;
                            update = Some((session.stitcher.height, session.stitcher.screen_count(), Some("已达到长图长度上限，请点击“完成并保存”".into()), false));
                        }
                        StitchStep::Unchanged => {
                            self.ivars().match_errors.set(0);
                            if self.ivars().warning_shown.get() {
                                self.ivars().warning_shown.set(false);
                                update = Some((session.stitcher.height, session.stitcher.screen_count(), Some("继续缓慢滚动，内容会自动拼接".into()), false));
                            }
                        }
                    }
                } else {
                    fail_message = Some("无法读取屏幕图像。".to_string());
                }
            }
        }
        if let Some((h, sc, detail, is_error)) = update {
            self.update_hud(h, sc, detail.as_deref(), is_error);
        }
        if limit {
            self.stop_timer();
        }
        if let Some(message) = fail_message {
            self.fail_session(&message);
        }
    }

    fn update_hud(&self, height: usize, screens: usize, detail: Option<&str>, is_error: bool) {
        if let Some(label) = self.ivars().hud_height.borrow().as_ref() {
            label.setStringValue(&NSString::from_str(&format!("{} px · {} 屏", height, screens)));
        }
        if let Some(text) = detail {
            if let Some(label) = self.ivars().hud_detail.borrow().as_ref() {
                label.setStringValue(&NSString::from_str(text));
            }
        }
        if let Some(label) = self.ivars().hud_title.borrow().as_ref() {
            label.setStringValue(&NSString::from_str(if is_error { "捕获遇到问题" } else { "正在拼接长图" }));
        }
    }

    fn show_hud(&self, region: &Region) {
        let mtm = self.mtm();
        let hud_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(HUD_WIDTH, HUD_HEIGHT));
        let panel: Retained<NSPanel> = unsafe {
            msg_send![
                NSPanel::alloc(mtm),
                initWithContentRect: hud_frame,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        panel.setLevel(NSStatusWindowLevel);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setOpaque(false);
        panel.setHasShadow(true);
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setHidesOnDeactivate(false);
        panel.setFloatingPanel(true);
        panel.setLevel(NSStatusWindowLevel);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );

        let content = HUDView::new(mtm, hud_frame, self.retained_clone());
        let title = NSTextField::labelWithString(ns_string!("正在拼接长图"), mtm);
        title.setFont(Some(&NSFont::systemFontOfSize(13.5)));
        title.setTextColor(Some(&NSColor::whiteColor()));
        title.setFrame(NSRect::new(NSPoint::new(40.0, 43.0), NSSize::new(215.0, 20.0)));

        let detail = NSTextField::labelWithString(ns_string!("滚动鼠标滚轮，松开后会自动捕获新内容"), mtm);
        detail.setFont(Some(&NSFont::systemFontOfSize(11.5)));
        detail.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.72, 1.0)));
        detail.setFrame(NSRect::new(NSPoint::new(40.0, 23.0), NSSize::new(340.0, 16.0)));

        let height = NSTextField::labelWithString(ns_string!("0 px"), mtm);
        height.setFont(Some(&NSFont::systemFontOfSize(11.5)));
        height.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.68, 1.0)));
        height.setAlignment(NSTextAlignment::Right);
        height.setFrame(NSRect::new(NSPoint::new(250.0, 45.0), NSSize::new(145.0, 16.0)));

        let base: &NSView = &content;
        base.addSubview(&title);
        base.addSubview(&detail);
        base.addSubview(&height);
        panel.setContentView(Some(base));

        let w = HUD_WIDTH;
        let h = HUD_HEIGHT;
        let visible = region.visible_frame;
        let mut x = region.cocoa_rect.origin.x + region.cocoa_rect.size.width / 2.0 - w / 2.0;
        x = x.max(visible.origin.x + 10.0).min(visible.origin.x + visible.size.width - w - 10.0);
        let mut y = region.cocoa_rect.origin.y + region.cocoa_rect.size.height + 12.0;
        if y + h > visible.origin.y + visible.size.height {
            y = region.cocoa_rect.origin.y - 12.0 - h;
        }
        if y < visible.origin.y + 10.0 {
            y = (visible.origin.y + visible.size.height - h - 10.0).max(visible.origin.y + 10.0);
        }
        panel.setFrame_display(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)), true);
        panel.orderFrontRegardless();

        self.ivars().hud_window.replace(Some(panel));
        self.ivars().hud_title.replace(Some(title));
        self.ivars().hud_detail.replace(Some(detail));
        self.ivars().hud_height.replace(Some(height));
    }

    fn hide_hud(&self) {
        if let Some(panel) = self.ivars().hud_window.borrow_mut().take() {
            panel.orderOut(None);
        }
        self.ivars().hud_title.borrow_mut().take();
        self.ivars().hud_detail.borrow_mut().take();
        self.ivars().hud_height.borrow_mut().take();
    }

    fn finish_session(&self) {
        if self.ivars().adjust_window.borrow().is_some() {
            self.adjust_cancelled();
        }
        let Some(mut session) = self.ivars().session.borrow_mut().take() else { let _ = NSBeep(); return; };
        if !session.stitcher.is_started() {
            self.ivars().session.replace(Some(session));
            let _ = NSBeep();
            return;
        }
        self.stop_timer();
        self.unregister_escape();
        if let Some(t) = self.ivars().hud_title.borrow().as_ref() { t.setStringValue(ns_string!("长图已暂停")); }
        if let Some(d) = self.ivars().hud_detail.borrow().as_ref() { d.setStringValue(ns_string!("取消保存后会继续捕获")); }
        self.activate_app();
        if let Some(panel) = self.ivars().hud_window.borrow().as_ref() {
            panel.orderOut(None);
        }

        let mtm = self.mtm();
        let panel = NSSavePanel::savePanel(mtm);
        panel.setTitle(Some(ns_string!("保存长截图")));
        panel.setNameFieldStringValue(&NSString::from_str(&format!(
            "长截图 {}.png",
            chrono::Local::now().format("%Y-%m-%d %H.%M.%S")
        )));
        if let Ok(home) = std::env::var("HOME") {
            let desktop = format!("{}/Desktop", home);
            let url = NSURL::fileURLWithPath(&NSString::from_str(&desktop));
            panel.setDirectoryURL(Some(&url));
        }
        let response = panel.runModal();
        if response != NSModalResponseOK {
            self.resume_session(session);
            return;
        }
        let Some(url) = panel.URL() else { self.resume_session(session); return; };
        let Some(path) = url.path() else { self.resume_session(session); return; };
        let path_str = path.to_string();
        let width = session.stitcher.width as u32;
        let height = session.stitcher.height as u32;
        if image::save_buffer(&path_str, &session.stitcher.canvas, width, height, image::ColorType::Rgba8).is_err() {
            self.show_alert("保存失败", "无法把长图写入所选位置，请换一个位置后重试。");
            self.resume_session(session);
            return;
        }
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_image(arboard::ImageData {
                width: session.stitcher.width,
                height: session.stitcher.height,
                bytes: std::borrow::Cow::Owned(session.stitcher.canvas.clone()),
            });
        }
        let saved = PathBuf::from(path_str);
        self.ivars().last_saved.replace(Some(saved));
        if let Some(item) = self.ivars().open_last_item.get() { item.setEnabled(true); }
        self.hide_capture_masks();
        self.hide_hud();
        self.set_capturing(false);
        self.restore_previous_app();
        drop(session);
    }

    fn resume_session(&self, session: Session) {
        self.ivars().session.replace(Some(session));
        self.register_escape();
        if let Some(panel) = self.ivars().hud_window.borrow().as_ref() {
            panel.orderFrontRegardless();
        }
        if let Some(t) = self.ivars().hud_title.borrow().as_ref() { t.setStringValue(ns_string!("正在拼接长图")); }
        if let Some(d) = self.ivars().hud_detail.borrow().as_ref() { d.setStringValue(ns_string!("滚动鼠标滚轮，松开后会自动捕获新内容")); }
        self.restore_previous_app();
        self.start_timer();
    }

    fn cancel_session(&self) {
        if let Some(window) = self.ivars().adjust_window.borrow_mut().take() {
            window.orderOut(None);
        }
        NSCursor::arrowCursor().set();
        self.ivars().pending_resize.borrow_mut().take();
        self.stop_timer();
        self.unregister_escape();
        self.ivars().session.borrow_mut().take();
        self.hide_capture_masks();
        self.hide_hud();
        self.set_capturing(false);
        self.restore_previous_app();
    }

    fn fail_session(&self, message: &str) {
        if let Some(window) = self.ivars().adjust_window.borrow_mut().take() {
            window.orderOut(None);
        }
        NSCursor::arrowCursor().set();
        self.ivars().pending_resize.borrow_mut().take();
        self.stop_timer();
        self.unregister_escape();
        self.ivars().session.borrow_mut().take();
        self.hide_capture_masks();
        self.hide_hud();
        self.set_capturing(false);
        self.show_alert("无法继续拼接长图", message);
        self.restore_previous_app();
    }

    fn set_capturing(&self, capturing: bool) {
        if let Some(item) = self.ivars().start_item.get() { item.setEnabled(!capturing); }
        if let Some(item) = self.ivars().finish_item.get() { item.setEnabled(capturing); }
        if let Some(item) = self.ivars().adjust_item.get() { item.setEnabled(capturing); }
        if let Some(item) = self.ivars().cancel_item.get() { item.setEnabled(capturing); }
    }

    fn start_timer(&self) {
        self.stop_timer();
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.1,
                {
                    let target: &AnyObject = self;
                    target
                },
                sel!(captureTick:),
                None,
                true,
            )
        };
        self.ivars().timer.replace(Some(timer));
    }

    fn stop_timer(&self) {
        if let Some(timer) = self.ivars().timer.borrow_mut().take() {
            timer.invalidate();
        }
    }

    fn show_help(&self) {
        self.activate_app();
        self.show_alert(
            "长截图使用说明",
            "1. 单击菜单栏中的“长截图”图标。\n2. 拖动框选要拼接的区域。\n3. 在框选区域内滚动鼠标滚轮。\n4. 点击悬浮控制条的“完成”，选择保存位置。\n\n建议只框选网页正文，避开浏览器工具栏和固定底栏；滚动慢一点效果最好。",
        );
    }

    fn show_alert(&self, title: &str, detail: &str) {
        let mtm = self.mtm();
        let was_registered = self.ivars().escape_hotkey.borrow().is_some();
        self.unregister_escape();
        self.activate_app();
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Informational);
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(detail));
        alert.addButtonWithTitle(ns_string!("好"));
        alert.runModal();
        if was_registered && (self.ivars().session.borrow().is_some() || self.ivars().selection_window.borrow().is_some()) {
            self.register_escape();
        }
    }

    fn open_last_saved(&self) {
        let Some(path) = self.ivars().last_saved.borrow().clone() else { let _ = NSBeep(); return; };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        NSWorkspace::sharedWorkspace().openURL(&url);
    }

    fn activate_app(&self) {
        let mtm = self.mtm();
        let app = NSApplication::sharedApplication(mtm);
        app.activateIgnoringOtherApps(true);
        let current = NSRunningApplication::currentApplication();
        current.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    }

    fn restore_previous_app(&self) {
        let Some(previous) = self.ivars().previous_app.borrow().clone() else { return; };
        if previous.processIdentifier() == std::process::id() as i32 { return; }
        let current = NSRunningApplication::currentApplication();
        previous.activateFromApplication_options(&current, NSApplicationActivationOptions::ActivateAllWindows);
    }

    fn retained_clone(&self) -> Retained<AppDelegate> {
        unsafe { Retained::retain(self as *const Self as *mut Self).expect("retain app delegate") }
    }
}

fn menu_item(mtm: MainThreadMarker, title: &NSString, action: objc2::runtime::Sel, target: &AppDelegate) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            title,
            Some(action),
            ns_string!(""),
        )
    };
    unsafe { item.setTarget(Some(target)) };
    item
}
