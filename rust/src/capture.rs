use core_graphics::base::{kCGImageAlphaPremultipliedLast, kCGBitmapByteOrder32Big};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_graphics::image::CGImage;
use core_graphics::window::{create_image, kCGWindowImageBestResolution, kCGWindowListOptionOnScreenOnly};

pub struct CaptureConverter {
    width: usize,
    height: usize,
    context: CGContext,
}

impl CaptureConverter {
    pub fn new(width: usize, height: usize) -> Option<Self> {
        if width == 0 || height == 0 { return None; }
        let cs = CGColorSpace::create_device_rgb();
        let context = CGContext::create_bitmap_context(
            None,
            width,
            height,
            8,
            width * 4,
            &cs,
            kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big,
        );
        Some(Self { width, height, context })
    }

    pub fn convert(&mut self, image: &CGImage) -> (Vec<u8>, usize, usize) {
        let rect = CGRect::new(
            &CGPoint::new(0.0, 0.0),
            &CGSize::new(self.width as f64, self.height as f64),
        );
        self.context.draw_image(rect, image);
        let data = self.context.data();
        let mut out = Vec::with_capacity(self.width * self.height * 4);
        for px in data.chunks_exact(4) {
            out.extend_from_slice(&[px[0], px[1], px[2], px[3]]);
        }
        (out, self.width, self.height)
    }

    pub fn convert_resized(&mut self, image: &CGImage) -> (Vec<u8>, usize, usize) {
        let w = image.width();
        let h = image.height();
        let rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(self.width as f64, self.height as f64));
        self.context.draw_image(rect, image);
        let data = self.context.data();
        let mut out = Vec::with_capacity(self.width * self.height * 4);
        for px in data.chunks_exact(4) {
            out.extend_from_slice(&[px[0], px[1], px[2], px[3]]);
        }
        (out, w.min(self.width), h.min(self.height))
    }
}

pub fn capture_rect(rect: CGRect) -> Option<CGImage> {
    create_image(
        rect,
        kCGWindowListOptionOnScreenOnly,
        0,
        kCGWindowImageBestResolution,
    )
}
