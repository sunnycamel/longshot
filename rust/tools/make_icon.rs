//! 生成应用图标（纯 Rust 版，替代原 scripts/make_icon.swift，不依赖 Swift/CoreGraphics）。
//!
//! 画出 1024×1024 的 AppIcon.png：蓝色渐变圆角底 + 三层叠页 + 内容条 + 向下箭头徽章，
//! 并可选直接打包 .icns（容器内嵌多尺寸 PNG，无需 iconutil）。
//!
//! 仅用于设计迭代，不参与常规构建，需显式启用 icon-tool feature：
//!
//! ```bash
//! cargo run --release --manifest-path rust/Cargo.toml \
//!     --features icon-tool --bin make-icon -- assets/AppIcon.png --icns assets/AppIcon.icns
//! ```

use std::env;
use std::io::Cursor;
use std::path::Path;

use image::{ImageFormat, RgbaImage};

const SIZE: u32 = 1024;
const SS: u32 = 4; // 超采样倍数，用于抗锯齿

#[derive(Clone, Copy)]
struct Rgba {
    r: f32,
    g: f32,
    b: f32,
    a: f32, // 非预乘，0..1
}

impl Rgba {
    const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    fn premul(self) -> [f32; 4] {
        [self.r * self.a, self.g * self.a, self.b * self.a, self.a]
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn clamp01(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

/// 圆角矩形有符号距离（<= 0 在内部），坐标系与 CoreGraphics 一致：y 轴向上。
fn rounded_rect_sdf(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    let qx = (px - cx).abs() - (w * 0.5 - r);
    let qy = (py - cy).abs() - (h * 0.5 - r);
    let dx = qx.max(0.0);
    let dy = qy.max(0.0);
    (dx * dx + dy * dy).sqrt() + qx.max(qy).min(0.0) - r
}

/// 线段最近距离（用于带圆帽的描边）
fn seg_dist(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let vx = bx - ax;
    let vy = by - ay;
    let wx = px - ax;
    let wy = py - ay;
    let t = ((wx * vx + wy * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    let dx = wx - t * vx;
    let dy = wy - t * vy;
    (dx * dx + dy * dy).sqrt()
}

struct Renderer {
    /// 外层圆角矩形裁剪掩码，每个输出像素 SS*SS 个采样点各一字节（0/1）
    clip: Vec<u8>,
    /// 画布，预乘 alpha，y 轴向上，索引 = (y * SIZE + x)
    canvas: Vec<[f32; 4]>,
}

impl Renderer {
    fn new() -> Self {
        let n = (SIZE * SIZE) as usize;
        let mut clip = vec![0u8; n * (SS * SS) as usize];
        for py in 0..SIZE {
            for px in 0..SIZE {
                for s in 0..(SS * SS) {
                    let sx = ((px * SS + s % SS) as f32 + 0.5) / SS as f32;
                    let sy = ((py * SS + s / SS) as f32 + 0.5) / SS as f32;
                    let inside = rounded_rect_sdf(sx, sy, 34.0, 34.0, 956.0, 956.0, 220.0) <= 0.0;
                    clip[((py * SIZE + px) as usize) * (SS * SS) as usize + s as usize] =
                        inside as u8;
                }
            }
        }
        Self {
            clip,
            canvas: vec![[0.0; 4]; n],
        }
    }

    /// 以 source-over 叠加一层填充。shape 返回采样点的预乘颜色，None 表示不覆盖。
    fn fill(&mut self, shape: impl Fn(f32, f32) -> Option<[f32; 4]>) {
        let ssq = (SS * SS) as usize;
        let ssf = SS as f32;
        for py in 0..SIZE {
            for px in 0..SIZE {
                let mut acc = [0.0f32; 4];
                let base = ((py * SIZE + px) as usize) * ssq;
                for s in 0..ssq {
                    if self.clip[base + s] == 0 {
                        continue;
                    }
                    let sx = ((px * SS + (s as u32 % SS)) as f32 + 0.5) / ssf;
                    let sy = ((py * SS + (s as u32 / SS)) as f32 + 0.5) / ssf;
                    if let Some(c) = shape(sx, sy) {
                        for k in 0..4 {
                            acc[k] += c[k];
                        }
                    }
                }
                if acc == [0.0; 4] {
                    continue;
                }
                for k in 0..4 {
                    acc[k] /= ssq as f32;
                }
                let d = &mut self.canvas[(py * SIZE + px) as usize];
                let sa = acc[3];
                for k in 0..4 {
                    d[k] = acc[k] + d[k] * (1.0 - sa);
                }
            }
        }
    }

    fn fill_solid(&mut self, inside: impl Fn(f32, f32) -> bool, color: Rgba) {
        let c = color.premul();
        self.fill(move |x, y| if inside(x, y) { Some(c) } else { None });
    }
}

fn render() -> Vec<[f32; 4]> {
    let mut r = Renderer::new();

    // 1. 背景线性渐变：沿 (180,900) -> (840,80)，端点之外取最近端颜色（与原图一致）
    let (ax, ay) = (180.0f32, 900.0f32);
    let (dx, dy) = (660.0f32, -820.0f32);
    let len2 = dx * dx + dy * dy;
    let c0 = Rgba::new(0.10, 0.27, 0.72, 1.0).premul();
    let c1 = Rgba::new(0.035, 0.065, 0.17, 1.0).premul();
    r.fill(move |x, y| {
        let t = clamp01(((x - ax) * dx + (y - ay) * dy) / len2);
        Some([
            lerp(c0[0], c1[0], t),
            lerp(c0[1], c1[1], t),
            lerp(c0[2], c1[2], t),
            1.0,
        ])
    });

    // 2. 页面后面的柔光（径向渐变，(512,560) 处最亮，alpha 0.38 -> 0）
    r.fill(|x, y| {
        let d = ((x - 512.0) * (x - 512.0) + (y - 560.0) * (y - 560.0)).sqrt();
        let t = clamp01((d - 10.0) / 450.0);
        let a = lerp(0.38, 0.0, t);
        if a <= 0.0 {
            None
        } else {
            Some(Rgba::new(0.30, 0.62, 1.0, a).premul())
        }
    });

    // 3. 三张叠放的长页面
    let sheets = [
        (250.0, 190.0, 540.0, 690.0, 44.0, Rgba::new(0.12, 0.28, 0.58, 0.85)),
        (218.0, 238.0, 548.0, 700.0, 46.0, Rgba::new(0.20, 0.43, 0.85, 0.92)),
        (188.0, 286.0, 566.0, 700.0, 48.0, Rgba::new(0.96, 0.98, 1.0, 1.0)),
    ];
    for (x, y, w, h, rad, col) in sheets {
        r.fill_solid(
            move |px, py| rounded_rect_sdf(px, py, x, y, w, h, rad) <= 0.0,
            col,
        );
    }

    // 4. 页面深色内容条（长短交替）
    let dark = Rgba::new(0.12, 0.31, 0.68, 1.0);
    let mut y = 832.0f32;
    let mut i = 0usize;
    while y >= 500.0 {
        let w = if i % 3 == 1 { 250.0 } else { 420.0 };
        r.fill_solid(|px, py| rounded_rect_sdf(px, py, 244.0, y, w, 24.0, 12.0) <= 0.0, dark);
        y -= 56.0;
        i += 1;
    }

    // 5. 页面浅色内容条
    let light = Rgba::new(0.77, 0.83, 0.93, 1.0);
    let mut y = 452.0f32;
    while y >= 348.0 {
        r.fill_solid(|px, py| rounded_rect_sdf(px, py, 244.0, y, 400.0, 16.0, 8.0) <= 0.0, light);
        y -= 44.0;
    }

    // 6. 向下箭头徽章（圆形底 + 白色描边箭头）
    r.fill_solid(
        |px, py| {
            let dx = (px - 722.0) / 130.0;
            let dy = (py - 300.0) / 130.0;
            dx * dx + dy * dy <= 1.0
        },
        Rgba::new(0.17, 0.48, 1.0, 1.0),
    );
    let white = Rgba::new(1.0, 1.0, 1.0, 1.0);
    let half = 15.0f32; // 线宽 30 的一半
    let segments = [
        (722.0, 344.0, 722.0, 222.0), // 竖干
        (668.0, 272.0, 722.0, 218.0), // 左箭头半边
        (722.0, 218.0, 776.0, 272.0), // 右箭头半边
    ];
    for (ax, ay, bx, by) in segments {
        r.fill_solid(
            move |px, py| seg_dist(px, py, ax, ay, bx, by) <= half,
            white,
        );
    }

    r.canvas
}

/// 预乘 f32 画布 -> straight RGBA8 图像（y 翻转为图像坐标）。
/// 输入按 size*size 索引；输出图像 y 轴向下。
fn to_image(buf: &[[f32; 4]], size: u32) -> RgbaImage {
    let mut img = RgbaImage::new(size, size);
    for iy in 0..size {
        let src_y = size - 1 - iy;
        for ix in 0..size {
            let p = buf[(src_y * size + ix) as usize];
            let a = p[3].clamp(0.0, 1.0);
            let ch = |v: f32| -> u8 {
                let v = if a > 0.0 { v / a } else { 0.0 };
                (v.clamp(0.0, 1.0) * 255.0).round() as u8
            };
            img.put_pixel(
                ix,
                iy,
                image::Rgba([ch(p[0]), ch(p[1]), ch(p[2]), (a * 255.0).round() as u8]),
            );
        }
    }
    img
}

/// 整数倍盒式降采样（预乘域，边缘无暗晕），y 轴向上索引保持不变。
fn downscale(buf: &[[f32; 4]], from: u32, to: u32) -> Vec<[f32; 4]> {
    let f = (from / to) as usize;
    let f2 = (f * f) as f32;
    let mut out = vec![[0.0f32; 4]; (to * to) as usize];
    for ty in 0..to {
        for tx in 0..to {
            let mut acc = [0.0f32; 4];
            for dy in 0..f {
                for dx in 0..f {
                    let idx = ((ty * f as u32 + dy as u32) * from + tx * f as u32 + dx as u32)
                        as usize;
                    for k in 0..4 {
                        acc[k] += buf[idx][k];
                    }
                }
            }
            for k in 0..4 {
                acc[k] /= f2;
            }
            out[(ty * to + tx) as usize] = acc;
        }
    }
    out
}

fn png_bytes(img: &RgbaImage) -> Vec<u8> {
    let mut cur = Cursor::new(Vec::new());
    img.write_to(&mut cur, ImageFormat::Png)
        .expect("PNG 编码失败");
    cur.into_inner()
}

/// 直接打包 .icns：容器 = "icns" + 总长度，随后每项 = 类型 + 长度 + PNG 数据。
/// 类型与尺寸对应关系来自 Apple 官方 iconset 规范（ic07=128 … ic10=1024，ic11..ic14 为 @2x）。
fn write_icns(path: &str, base: &[[f32; 4]]) {
    let entries = [
        ("ic10", 1024u32),
        ("ic09", 512),
        ("ic08", 256),
        ("ic07", 128),
        ("ic14", 512),
        ("ic13", 256),
        ("ic12", 64),
        ("ic11", 32),
    ];
    let mut chunks = Vec::with_capacity(entries.len());
    for (ty, size) in entries {
        let img = to_image(&downscale(base, SIZE, size), size);
        let data = png_bytes(&img);
        let mut chunk = Vec::with_capacity(8 + data.len());
        chunk.extend_from_slice(ty.as_bytes());
        chunk.extend_from_slice(&((8 + data.len()) as u32).to_be_bytes());
        chunk.extend_from_slice(&data);
        chunks.push(chunk);
    }
    let total: usize = 8 + chunks.iter().map(|c| c.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(total as u32).to_be_bytes());
    for c in chunks {
        out.extend_from_slice(&c);
    }
    std::fs::write(path, &out).unwrap_or_else(|e| panic!("写入 {path} 失败: {e}"));
}

fn print_usage() {
    println!(
        "用法: make-icon [输出.png] [--icns 输出.icns]\n\
         默认输出 assets/AppIcon.png（相对当前目录）。"
    );
}

fn main() {
    let mut png_out = "assets/AppIcon.png".to_string();
    let mut icns_out: Option<String> = None;
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--icns" => match it.next() {
                Some(p) => icns_out = Some(p),
                None => {
                    eprintln!("错误: --icns 需要一个路径参数");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                print_usage();
                return;
            }
            _ => png_out = arg,
        }
    }

    let canvas = render();
    let img = to_image(&canvas, SIZE);
    img.save(&png_out)
        .unwrap_or_else(|e| panic!("写入 {} 失败: {e}", Path::new(&png_out).display()));
    println!("icon written to {}", Path::new(&png_out).display());

    if let Some(p) = icns_out {
        write_icns(&p, &canvas);
        println!("icns written to {}", Path::new(&p).display());
    }
}
