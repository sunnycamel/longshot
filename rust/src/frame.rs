use image::RgbaImage;

pub const MAX_CANVAS_BYTES: usize = 300 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct GrayFrame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    pub coarse: Vec<u8>,
    pub coarse_height: usize,
    pub contrast: f64,
}

impl GrayFrame {
    pub fn new(rgba: &[u8], width: usize, height: usize, gray_width: usize) -> Option<Self> {
        if width == 0 || height == 0 || rgba.len() < width * height * 4 {
            return None;
        }
        let gw = gray_width.max(32).min(width);
        let mut pixels = vec![0u8; gw * height];
        for y in 0..height {
            let row = y * width * 4;
            for gx in 0..gw {
                let sx = (gx * width / gw).min(width - 1);
                let i = row + sx * 4;
                let lum = (rgba[i] as u32 * 3 + rgba[i + 1] as u32 * 6 + rgba[i + 2] as u32 * 10) / 19;
                pixels[y * gw + gx] = lum as u8;
            }
        }
        let ch = (height / 4).max(1);
        let mut coarse = vec![0u8; gw * ch];
        for cy in 0..ch {
            let y0 = cy * 4;
            let y1 = (y0 + 4).min(height);
            let count = (y1 - y0).max(1) as u32;
            for x in 0..gw {
                let mut sum = 0u32;
                for y in y0..y1 {
                    sum += pixels[y * gw + x] as u32;
                }
                coarse[cy * gw + x] = (sum / count) as u8;
            }
        }
        let band_top = (height as f64 * 0.18) as usize;
        let band_bottom = ((height as f64 * 0.95) as usize).min(height);
        let step = (height / 200).max(1);
        let x_step = (gw / 80).max(1);
        let mut values = Vec::new();
        let mut y = band_top.max(1);
        while y < band_bottom {
            let mut x = 0;
            while x < gw {
                values.push(pixels[y * gw + x] as f64);
                x += x_step;
            }
            y += step;
        }
        let contrast = if values.is_empty() {
            0.0
        } else {
            let mean: f64 = values.iter().sum::<f64>() / values.len() as f64;
            let var: f64 = values.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / values.len() as f64;
            var.sqrt()
        };
        Some(Self { width: gw, height, pixels, coarse, coarse_height: ch, contrast })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ScrollMatch {
    Still,
    Shift { rows: isize, score: f64 },
    Failed,
}

const MIN_SHIFT_ROWS: isize = 3;
const MAX_MEAN_DIFFERENCE: f64 = 12.0;
const MIN_IMPROVEMENT: f64 = 2.0;

fn score(
    prev: &[u8],
    curr: &[u8],
    width: usize,
    row_count: usize,
    band_start: usize,
    band_end: usize,
    shift: isize,
    stride: usize,
    min_overlap: usize,
) -> Option<(f64, usize)> {
    let y0 = (band_start as isize).max(-shift).max(0) as usize;
    let y1 = ((band_end as isize).min(row_count as isize - shift)) as usize;
    if y1 <= y0 || y1 - y0 < min_overlap {
        return None;
    }
    let rows = y1 - y0;
    let samples = (width + stride - 1) / stride;
    let mut sum = 0u64;
    for y in y0..y1 {
        let pi = ((y as isize + shift) as usize) * width;
        let ci = y * width;
        let mut x = 0;
        while x < width {
            sum += (prev[pi + x] as i32 - curr[ci + x] as i32).unsigned_abs() as u64;
            x += stride;
        }
    }
    Some((sum as f64 / (rows * samples) as f64, rows))
}

pub fn match_frames(previous: &GrayFrame, current: &GrayFrame) -> ScrollMatch {
    if previous.width != current.width || previous.height != current.height || previous.height <= 40 {
        return ScrollMatch::Failed;
    }
    let width = previous.width;
    let height = previous.height;
    let band_top = (height as f64 * 0.18) as usize;
    let band_bottom = (height as f64 * 0.95) as usize;
    let min_overlap = ((height as f64 * 0.15) as usize).max(24);
    let max_positive = height as isize - band_top as isize - min_overlap as isize;
    let max_negative = band_bottom as isize - min_overlap as isize;
    let max_shift = max_positive.max(max_negative).max(8);

    if current.contrast < 2.5 {
        return ScrollMatch::Still;
    }

    let mut quick_sum = 0u64;
    let mut quick_n = 0u64;
    let mut y = band_top;
    while y < band_bottom {
        let off = y * width;
        let mut x = 0;
        while x < width {
            quick_sum += (previous.pixels[off + x] as i32 - current.pixels[off + x] as i32).unsigned_abs() as u64;
            x += 2;
            quick_n += 1;
        }
        y += 2;
    }
    if quick_n > 0 && quick_sum as f64 / quick_n as f64 <= 0.5 {
        return ScrollMatch::Still;
    }

    let coarse_height = previous.coarse_height;
    let coarse_top = band_top / 4;
    let coarse_bottom = (band_bottom / 4).min(coarse_height).max(coarse_top + 2);
    let coarse_min_overlap = (min_overlap / 4).max(4);
    let coarse_max_shift = (max_shift / 4).max(2);
    let stride = if width >= 96 { 2 } else { 1 };

    #[derive(Clone, Copy)]
    struct Cand { shift: isize, score: f64 }
    let mut coarse_candidates = Vec::new();
    let coarse_upper = coarse_max_shift.min((coarse_height as isize - coarse_top as isize - coarse_min_overlap as isize).max(0));
    if coarse_upper > 0 {
        for d in -coarse_upper..=coarse_upper {
            if d == 0 { continue; }
            if let Some((s, _)) = score(&previous.coarse, &current.coarse, width, coarse_height, coarse_top, coarse_bottom, d, stride, coarse_min_overlap) {
                coarse_candidates.push(Cand { shift: d, score: s });
            }
        }
    }
    coarse_candidates.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut valleys: Vec<Cand> = Vec::new();
    for c in &coarse_candidates {
        if valleys.iter().all(|v| (v.shift - c.shift).abs() >= 3) {
            valleys.push(*c);
        }
        if valleys.len() == 4 { break; }
    }
    if valleys.is_empty() {
        return ScrollMatch::Failed;
    }

    let refine = |valley: Cand| -> Option<Cand> {
        let center = valley.shift * 4;
        let mut best: Option<Cand> = None;
        for d in (center - 6)..=(center + 6) {
            if d.abs() < MIN_SHIFT_ROWS { continue; }
            if let Some((s, _)) = score(&previous.pixels, &current.pixels, width, height, band_top, band_bottom, d, 1, min_overlap) {
                if best.map(|b| s < b.score).unwrap_or(true) {
                    best = Some(Cand { shift: d, score: s });
                }
            }
        }
        best
    };
    let mut refined = Vec::new();
    for v in &valleys {
        if let Some(c) = refine(*v) {
            if refined.iter().all(|r: &Cand| (r.shift - c.shift).abs() >= 6) {
                refined.push(c);
            }
        }
    }
    refined.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
    let Some(best) = refined.first().copied() else { return ScrollMatch::Failed; };

    let mut still_sum = 0u64;
    for y in band_top..band_bottom {
        let off = y * width;
        for x in 0..width {
            still_sum += (previous.pixels[off + x] as i32 - current.pixels[off + x] as i32).unsigned_abs() as u64;
        }
    }
    let still_score = still_sum as f64 / ((band_bottom - band_top) * width) as f64;

    if best.score > MAX_MEAN_DIFFERENCE { return ScrollMatch::Failed; }
    if still_score - best.score < MIN_IMPROVEMENT { return ScrollMatch::Failed; }

    // Verify each vertical third independently.
    let overlap_start = (band_top as isize).max(-best.shift) as usize;
    let overlap_end = ((band_bottom as isize).min(height as isize - best.shift)) as usize;
    let overlap_rows = overlap_end.saturating_sub(overlap_start);
    if overlap_rows >= 60 {
        let third = overlap_rows / 3;
        let mut seg_scores = Vec::new();
        for i in 0..3 {
            let start = overlap_start + i * third;
            let end = if i == 2 { overlap_end } else { start + third };
            if end - start < 8 { continue; }
            let mut sum = 0u64;
            for y in start..end {
                let pi = ((y as isize + best.shift) as usize) * width;
                let ci = y * width;
                for x in 0..width {
                    sum += (previous.pixels[pi + x] as i32 - current.pixels[ci + x] as i32).unsigned_abs() as u64;
                }
            }
            seg_scores.push(sum as f64 / ((end - start) * width) as f64);
        }
        if seg_scores.iter().cloned().fold(0.0, f64::max) > 24.0 {
            return ScrollMatch::Failed;
        }
    }

    // Ambiguity guard.
    if let Some(second) = refined.iter().find(|c| (c.shift - best.shift).abs() >= 8) {
        let margin = 1.5f64.max(best.score * 0.35);
        if second.score < best.score + margin {
            return ScrollMatch::Failed;
        }
    }

    ScrollMatch::Shift { rows: best.shift, score: best.score }
}

#[derive(Clone, Debug)]
pub enum StitchStep {
    Initialized { height: usize },
    Unchanged,
    Stitched { shift_rows: isize, added_rows: usize, trimmed_rows: usize },
    Failed,
    LimitReached,
}

pub struct LongImageStitcher {
    pub canvas: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub viewport_top: isize,
    pub viewport_height: usize,
    pub max_bytes: usize,
    last_gray: Option<GrayFrame>,
    limit_reached: bool,
}

impl LongImageStitcher {
    pub fn new(max_bytes: usize) -> Self {
        Self { canvas: Vec::new(), width: 0, height: 0, viewport_top: 0, viewport_height: 0, max_bytes, last_gray: None, limit_reached: false }
    }

    pub fn is_started(&self) -> bool { self.height > 0 }
    pub fn bytes_per_row(&self) -> usize { self.width * 4 }
    fn max_rows(&self) -> usize { (self.max_bytes / self.bytes_per_row().max(1)).max(1) }

    pub fn begin(&mut self, frame: &[u8], width: usize, height: usize, gray: GrayFrame) -> StitchStep {
        if width == 0 || height == 0 || frame.len() < width * height * 4 {
            return StitchStep::Failed;
        }
        self.width = width;
        self.height = height;
        self.viewport_height = height;
        self.viewport_top = 0;
        self.canvas = frame[..width * height * 4].to_vec();
        self.last_gray = Some(gray);
        self.limit_reached = false;
        StitchStep::Initialized { height }
    }

    pub fn ingest(&mut self, frame: &[u8], width: usize, height: usize, gray: GrayFrame) -> StitchStep {
        if !self.is_started() || width != self.width || height != self.viewport_height || frame.len() < width * height * 4 {
            return StitchStep::Failed;
        }
        if self.limit_reached {
            return StitchStep::LimitReached;
        }
        let Some(prev) = self.last_gray.as_ref() else { return StitchStep::Failed; };
        match match_frames(prev, &gray) {
            ScrollMatch::Still => { self.last_gray = Some(gray); StitchStep::Unchanged }
            ScrollMatch::Failed => StitchStep::Failed,
            ScrollMatch::Shift { rows, .. } => {
                if rows == 0 { self.last_gray = Some(gray); return StitchStep::Unchanged; }
                self.viewport_top += rows;
                let mut added = 0usize;
                if self.viewport_top < 0 {
                    added = (-self.viewport_top) as usize;
                    if self.height + added > self.max_rows() { self.limit_reached = true; self.last_gray = Some(gray); return StitchStep::LimitReached; }
                    let bpr = self.bytes_per_row();
                    let byte_count = added * bpr;
                    let mut new_canvas = Vec::with_capacity(self.canvas.len() + byte_count);
                    new_canvas.extend_from_slice(&frame[..byte_count]);
                    new_canvas.extend_from_slice(&self.canvas);
                    self.canvas = new_canvas;
                    self.height += added;
                    self.viewport_top = 0;
                }
                let viewport_bottom = self.viewport_top + self.viewport_height as isize;
                if viewport_bottom > self.height as isize {
                    let new_rows = (viewport_bottom - self.height as isize) as usize;
                    if self.height + new_rows > self.max_rows() { self.limit_reached = true; self.last_gray = Some(gray); return StitchStep::LimitReached; }
                    let bpr = self.bytes_per_row();
                    let start_row = self.viewport_height - new_rows;
                    let start = start_row * bpr;
                    let end = start + new_rows * bpr;
                    self.canvas.extend_from_slice(&frame[start..end]);
                    self.height += new_rows;
                    added += new_rows;
                }
                // Scrolling back up means the content below the current
                // viewport is no longer part of the intended capture. Trim
                // that tail; scrolling down again will append it back.
                let mut trimmed = 0usize;
                if viewport_bottom >= 0 && (viewport_bottom as usize) < self.height {
                    let new_height = viewport_bottom as usize;
                    let bpr = self.bytes_per_row();
                    self.canvas.truncate(new_height * bpr);
                    trimmed = self.height - new_height;
                    self.height = new_height;
                }
                self.last_gray = Some(gray);
                StitchStep::Stitched { shift_rows: rows, added_rows: added, trimmed_rows: trimmed }
            }
        }
    }

    /// Change the bottom of the viewport. The frame must have the same width
    /// and a new height; only the rows that become visible below the old
    /// viewport are appended, or the rows that are no longer visible are
    /// trimmed. This is used by the "adjust bottom edge" editor.
    pub fn resize_viewport(&mut self, new_height: usize, frame: &[u8], gray: GrayFrame) -> Option<(usize, usize)> {
        if self.width == 0 || new_height == 0 || frame.len() < self.width * new_height * 4 {
            return None;
        }
        if gray.width == 0 || gray.height != new_height {
            return None;
        }
        let target_bottom = self.viewport_top + new_height as isize;
        if target_bottom < 0 { return None; }
        let target = target_bottom as usize;
        if target > self.max_rows() {
            self.limit_reached = true;
            return None;
        }
        let bpr = self.bytes_per_row();
        let mut added = 0usize;
        let mut trimmed = 0usize;
        if target > self.height {
            added = target - self.height;
            if added > new_height { return None; }
            let start_row = new_height - added;
            let start = start_row * bpr;
            let end = start + added * bpr;
            self.canvas.extend_from_slice(&frame[start..end]);
            self.height = target;
        } else if target < self.height {
            trimmed = self.height - target;
            self.canvas.truncate(target * bpr);
            self.height = target;
        }
        self.viewport_height = new_height;
        self.last_gray = Some(gray);
        Some((added, trimmed))
    }

    pub fn make_image(&self) -> Option<RgbaImage> {
        if self.width == 0 || self.height == 0 || self.canvas.len() < self.bytes_per_row() * self.height { return None; }
        RgbaImage::from_raw(self.width as u32, self.height as u32, self.canvas.clone())
    }

    pub fn screen_count(&self) -> usize {
        (self.height as f64 / self.viewport_height.max(1) as f64).ceil().max(1.0) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 760;
    const H: usize = 620;
    const PH: usize = 5400;

    fn page() -> Vec<u8> {
        let mut bytes = vec![255u8; W * PH * 4];
        for y in 0..PH {
            let section = y / 180;
            let base = (235 + (section * 7) % 18) as u8;
            let accent = ((section * 37) % 120 + 40) as u8;
            for x in 0..W {
                let i = (y * W + x) * 4;
                let mut r = base; let mut g = base; let mut b = base;
                if x < 90 { r = accent; g = accent.saturating_add(30); b = 70; }
                let row_hash = (y.wrapping_mul(1103515245).wrapping_add(12345)) & 0x7fffffff;
                if y % 4 == 0 && row_hash % 9 < 6 {
                    let start = 120 + row_hash % 400;
                    let width = 40 + (row_hash >> 8) % 170;
                    if x >= start && x < (start + width).min(W) { r = 25; g = 30; b = 38; }
                }
                if y % 46 == 0 && x > 100 { r = 180; g = 185; b = 190; }
                if (y / 180) % 3 == 0 && y % 180 < 26 && x > 110 && x < 140 + (y % 180) * 6 { r = 45; g = 100; b = 210; }
                bytes[i] = r; bytes[i+1] = g; bytes[i+2] = b; bytes[i+3] = 255;
            }
        }
        bytes
    }

    fn viewport(page: &[u8], y: usize, header: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; W * H * 4];
        for row in 0..H {
            let src = y + row;
            if src < PH { bytes[row*W*4..(row+1)*W*4].copy_from_slice(&page[src*W*4..(src+1)*W*4]); }
        }
        if header > 0 {
            for row in 0..header {
                let band = row / 12;
                let color = [((22 + (band % 2) * 8) as u8), ((32 + (band % 2) * 8) as u8), ((48 + (band % 2) * 8) as u8), 255];
                for x in 0..W {
                    let i = (row*W+x)*4;
                    bytes[i..i+4].copy_from_slice(&color);
                }
            }
        }
        bytes
    }

    fn run_case(name: &str, positions: &[usize], header: usize, exp_start: usize, exp_end: usize) -> bool {
        let page = page();
        let mut stitcher = LongImageStitcher::new(100*1024*1024);
        for (idx, &pos) in positions.iter().enumerate() {
            let frame = viewport(&page, pos, header);
            let gray = GrayFrame::new(&frame, W, H, 128).unwrap();
            let step = if idx == 0 { stitcher.begin(&frame, W, H, gray) } else { stitcher.ingest(&frame, W, H, gray) };
            if let StitchStep::Failed | StitchStep::LimitReached = step { println!("{} failed at {} step={:?}", name, pos, step); return false; }
        }
        if stitcher.height != exp_end - exp_start { println!("{} height {} != {}", name, stitcher.height, exp_end-exp_start); return false; }
        let cmp_start = header;
        let actual = &stitcher.canvas[cmp_start*W*4..];
        let expected = &page[(exp_start+cmp_start)*W*4..exp_end*W*4];
        if actual != expected {
            let pos = actual.iter().zip(expected.iter()).position(|(a,b)| a != b).unwrap_or(0);
            println!("{} pixel mismatch at byte {} (row {})", name, pos, cmp_start + (pos/4)/W);
            return false;
        }
        println!("{} OK height={} count={}", name, stitcher.height, stitcher.screen_count());
        true
    }

    #[test]
    fn down_and_mixed() {
        assert!(run_case("down-small", &[0,20,45,95,170,290,430], 0, 0, 430+H));
        assert!(run_case("down-big", &[0,120,260,410,620,850], 0, 0, 850+H));
        assert!(run_case("mixed", &[1200,1100,900,950,1300,1700], 0, 900, 1700+H));
        assert!(run_case("fixed-header", &[0,100,240,390,550], 64, 0, 550+H));
        assert!(run_case("large-fixed-header", &[0,90,210,350,500], 110, 0, 500+H));
    }

    #[test]
    fn scroll_back_trims_tail() {
        assert!(run_case("back-trim", &[0, 300, 100], 0, 0, 100+H));
        assert!(run_case("back-trim-mid", &[1200, 1500, 1300], 0, 1200, 1300+H));
        assert!(run_case("back-above-start", &[1200, 900, 600], 0, 600, 600+H));
    }

    fn tall_viewport(page: &[u8], y: usize, height: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; W * height * 4];
        for row in 0..height {
            let src = y + row;
            if src < PH { bytes[row*W*4..(row+1)*W*4].copy_from_slice(&page[src*W*4..(src+1)*W*4]); }
        }
        bytes
    }

    #[test]
    fn resize_bottom_extends_and_trims() {
        let page = page();
        let mut stitcher = LongImageStitcher::new(100*1024*1024);
        let f0 = viewport(&page, 0, 0);
        let g0 = GrayFrame::new(&f0, W, H, 128).unwrap();
        let _ = stitcher.begin(&f0, W, H, g0);
        let f300 = viewport(&page, 300, 0);
        let g300 = GrayFrame::new(&f300, W, H, 128).unwrap();
        let _ = stitcher.ingest(&f300, W, H, g300);
        assert_eq!(stitcher.height, H + 300);

        let tall = tall_viewport(&page, 300, H + 100);
        let tall_gray = GrayFrame::new(&tall, W, H + 100, 128).unwrap();
        let (added, trimmed) = stitcher.resize_viewport(H + 100, &tall, tall_gray).unwrap();
        assert_eq!(added, 100);
        assert_eq!(trimmed, 0);
        assert_eq!(stitcher.height, H + 400);
        assert_eq!(&stitcher.canvas[..], &page[..(H + 400) * W * 4]);

        let short = tall_viewport(&page, 300, H - 100);
        let short_gray = GrayFrame::new(&short, W, H - 100, 128).unwrap();
        let (added, trimmed) = stitcher.resize_viewport(H - 100, &short, short_gray).unwrap();
        assert_eq!(added, 0);
        assert_eq!(trimmed, 200);
        assert_eq!(stitcher.height, H + 200);
        assert_eq!(&stitcher.canvas[..], &page[..(H + 200) * W * 4]);
    }

    #[test]
    fn false_motion_and_overlarge() {
        let page = page();
        let mut stitcher = LongImageStitcher::new(100*1024*1024);
        let mut frame = viewport(&page, 0, 0);
        let gray = GrayFrame::new(&frame, W, H, 128).unwrap();
        let _ = stitcher.begin(&frame, W, H, gray);
        for i in 0..8 {
            let x = 250 + i*30;
            frame = viewport(&page, 0, 0);
            for row in 300..314 { for col in x..x+14 { let p=(row*W+col)*4; frame[p]=10; frame[p+1]=10; frame[p+2]=10; } }
            let gray = GrayFrame::new(&frame, W, H, 128).unwrap();
            if let StitchStep::Stitched { .. } = stitcher.ingest(&frame, W, H, gray) { panic!("cursor blink caused stitch"); }
        }
        assert_eq!(stitcher.height, H);

        let mut stitcher = LongImageStitcher::new(100*1024*1024);
        let frame0 = viewport(&page, 0, 0);
        let gray0 = GrayFrame::new(&frame0, W, H, 128).unwrap();
        let _ = stitcher.begin(&frame0, W, H, gray0);
        let frame1 = viewport(&page, 500, 0);
        let gray1 = GrayFrame::new(&frame1, W, H, 128).unwrap();
        if let StitchStep::Stitched { .. } = stitcher.ingest(&frame1, W, H, gray1) { panic!("overlarge shift stitched"); }
    }
}
