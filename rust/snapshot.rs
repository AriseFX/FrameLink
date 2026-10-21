//! Export the actual egui layout without a native window or a capture device.
//! This small software rasterizer is a development tool, never the live UI path.
use eframe::egui::{
    self, Color32,
    epaint::{ImageData, Primitive, Vertex},
};
use std::{collections::HashMap, path::Path};

#[derive(Clone)]
struct Texture {
    width: usize,
    height: usize,
    pixels: Vec<Color32>,
}

pub fn write(path: &Path, width: u32, height: u32) -> Result<(), String> {
    if !(640..=2400).contains(&width) || !(480..=1800).contains(&height) {
        return Err("界面快照尺寸超出范围".into());
    }
    let ctx = egui::Context::default();
    let mut app = crate::ui::DesktopApp::for_snapshot(&ctx, width < 1100);
    let mut textures = HashMap::new();
    let mut final_output = None;
    for frame in 0..16 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width as f32, height as f32),
            )),
            time: Some(frame as f64 / 10.0),
            max_texture_side: Some(4096),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.draw(ctx));
        for (id, delta) in &output.textures_delta.set {
            let ImageData::Color(image) = &delta.image;
            if let Some([x, y]) = delta.pos {
                let texture: &mut Texture = textures.get_mut(id).ok_or("纹理更新缺少原图")?;
                for row in 0..image.size[1] {
                    let start = (row + y) * texture.width + x;
                    texture.pixels[start..start + image.size[0]].copy_from_slice(
                        &image.pixels[row * image.size[0]..(row + 1) * image.size[0]],
                    );
                }
            } else {
                textures.insert(
                    *id,
                    Texture {
                        width: image.size[0],
                        height: image.size[1],
                        pixels: image.pixels.clone(),
                    },
                );
            }
        }
        final_output = Some(output);
    }
    let output = final_output.unwrap();
    let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
    let mut canvas = vec![250u8; width as usize * height as usize * 4];
    for alpha in canvas.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
    for primitive in primitives {
        let Primitive::Mesh(mesh) = primitive.primitive else {
            continue;
        };
        let texture = textures
            .get(&mesh.texture_id)
            .ok_or_else(|| format!("未知纹理 {:?}", mesh.texture_id))?;
        let clip = primitive.clip_rect;
        let scissor = [
            clip.min.x.max(0.0) as u32,
            clip.min.y.max(0.0) as u32,
            clip.max.x.ceil().min(width as f32).max(0.0) as u32,
            clip.max.y.ceil().min(height as f32).max(0.0) as u32,
        ];
        for triangle in mesh.indices.chunks_exact(3) {
            let vertices = [
                mesh.vertices[triangle[0] as usize],
                mesh.vertices[triangle[1] as usize],
                mesh.vertices[triangle[2] as usize],
            ];
            rasterize(&mut canvas, width, &vertices, texture, scissor);
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    image::save_buffer(path, &canvas, width, height, image::ColorType::Rgba8)
        .map_err(|e| e.to_string())
}

fn edge(a: egui::Pos2, b: egui::Pos2, p: egui::Pos2) -> f32 {
    (p.x - a.x) * (b.y - a.y) - (p.y - a.y) * (b.x - a.x)
}
fn rasterize(canvas: &mut [u8], width: u32, v: &[Vertex; 3], texture: &Texture, clip: [u32; 4]) {
    let area = edge(v[0].pos, v[1].pos, v[2].pos);
    if area.abs() < 0.0001 {
        return;
    }
    let min_x = v
        .iter()
        .map(|v| v.pos.x)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(clip[0] as f32) as u32;
    let min_y = v
        .iter()
        .map(|v| v.pos.y)
        .fold(f32::INFINITY, f32::min)
        .floor()
        .max(clip[1] as f32) as u32;
    let max_x = v
        .iter()
        .map(|v| v.pos.x)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min(clip[2] as f32)
        .max(0.0) as u32;
    let max_y = v
        .iter()
        .map(|v| v.pos.y)
        .fold(f32::NEG_INFINITY, f32::max)
        .ceil()
        .min(clip[3] as f32)
        .max(0.0) as u32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let p = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
            let a = edge(v[1].pos, v[2].pos, p) / area;
            let b = edge(v[2].pos, v[0].pos, p) / area;
            let c = 1.0 - a - b;
            if a < 0.0 || b < 0.0 || c < 0.0 {
                continue;
            }
            let u = v[0].uv.x * a + v[1].uv.x * b + v[2].uv.x * c;
            let vv = v[0].uv.y * a + v[1].uv.y * b + v[2].uv.y * c;
            let tex = sample(texture, u, vv);
            let color: [f32; 4] = std::array::from_fn(|i| {
                (v[0].color[i] as f32 * a + v[1].color[i] as f32 * b + v[2].color[i] as f32 * c)
                    / 255.0
                    * tex[i]
            });
            let index = (y as usize * width as usize + x as usize) * 4;
            for i in 0..3 {
                canvas[index + i] = (color[i] + canvas[index + i] as f32 * (1.0 - color[3] / 255.0))
                    .clamp(0.0, 255.0)
                    .round() as u8;
            }
            canvas[index + 3] = 255;
        }
    }
}
fn sample(texture: &Texture, u: f32, v: f32) -> [f32; 4] {
    let x = (u * texture.width as f32 - 0.5).clamp(0.0, (texture.width - 1) as f32);
    let y = (v * texture.height as f32 - 0.5).clamp(0.0, (texture.height - 1) as f32);
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = (
        (x0 + 1).min(texture.width - 1),
        (y0 + 1).min(texture.height - 1),
    );
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    std::array::from_fn(|i| {
        let a = texture.pixels[y0 * texture.width + x0][i] as f32 * (1.0 - fx)
            + texture.pixels[y0 * texture.width + x1][i] as f32 * fx;
        let b = texture.pixels[y1 * texture.width + x0][i] as f32 * (1.0 - fx)
            + texture.pixels[y1 * texture.width + x1][i] as f32 * fx;
        a * (1.0 - fy) + b * fy
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rasterizer_clips_and_blends_real_mesh() {
        let texture = Texture {
            width: 1,
            height: 1,
            pixels: vec![Color32::WHITE],
        };
        let make = |x, y| Vertex {
            pos: egui::pos2(x, y),
            uv: egui::pos2(0.5, 0.5),
            color: Color32::RED,
        };
        let mut canvas = vec![255; 4 * 4 * 4];
        rasterize(
            &mut canvas,
            4,
            &[make(0.0, 0.0), make(4.0, 0.0), make(0.0, 4.0)],
            &texture,
            [1, 1, 4, 4],
        );
        assert_eq!(&canvas[..4], &[255, 255, 255, 255]);
        assert_eq!(&canvas[20..24], &[255, 0, 0, 255]);
    }
}
