use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSPoint, NSRect, NSSize};

#[derive(Clone, Debug)]
pub struct Region {
    pub cg_rect: CGRect,
    pub cocoa_rect: NSRect,
    pub visible_frame: NSRect,
    pub screen_frame: NSRect,
    pub scale: f64,
    pub pixel_width: usize,
    pub pixel_height: usize,
}

pub fn make_region(
    cocoa_rect: NSRect,
    screen_frame: NSRect,
    visible_frame: NSRect,
    main_screen_height: f64,
    scale: f64,
) -> Option<Region> {
    let min_x = cocoa_rect.origin.x.max(screen_frame.origin.x);
    let min_y = cocoa_rect.origin.y.max(screen_frame.origin.y);
    let max_x = (cocoa_rect.origin.x + cocoa_rect.size.width)
        .min(screen_frame.origin.x + screen_frame.size.width);
    let max_y = (cocoa_rect.origin.y + cocoa_rect.size.height)
        .min(screen_frame.origin.y + screen_frame.size.height);
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    let x0 = min_x.floor();
    let y0 = min_y.floor();
    let x1 = max_x.ceil();
    let y1 = max_y.ceil();
    if x1 - x0 < 60.0 || y1 - y0 < 60.0 {
        return None;
    }
    let cocoa = NSRect::new(NSPoint::new(x0, y0), NSSize::new(x1 - x0, y1 - y0));
    let cg_rect = CGRect::new(
        &CGPoint::new(x0, main_screen_height - y1),
        &CGSize::new(x1 - x0, y1 - y0),
    );
    let pixel_width = ((x1 - x0) * scale).round().max(1.0) as usize;
    let pixel_height = ((y1 - y0) * scale).round().max(1.0) as usize;
    Some(Region {
        cg_rect,
        cocoa_rect: cocoa,
        visible_frame,
        screen_frame,
        scale,
        pixel_width,
        pixel_height,
    })
}
