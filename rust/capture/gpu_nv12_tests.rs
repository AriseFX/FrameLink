use super::*;
use crate::{model::PixelFormat, pixels::FrameBuffers};
use windows::Win32::Foundation::HMODULE;

#[test]
#[ignore = "requires an interactive Windows hardware D3D11 device"]
fn hardware_gpu_nv12_matches_cpu() {
    unsafe {
        let (mut device, mut context) = (None, None);
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .unwrap();
        let device = device.unwrap();
        let context = context.unwrap();
        for (w, h) in [
            (2u32, 2u32),
            (6, 4),
            (18, 34),
            (416, 416),
            (640, 640),
            (722, 4),
            (1922, 4),
            (32, 1082),
        ] {
            let gpu = GpuNv12::new(&device, w, h).unwrap();
            let (tw, th) = (w + 8, h + 4);
            let mut full = vec![0u8; (tw * th * 4) as usize];
            for pattern in 0..3 {
                let mut seed = 0x12345678u32;
                for (i, p) in full.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    let colors = [
                        [0, 0, 0, 255],
                        [255, 255, 255, 0],
                        [0, 0, 255, 255],
                        [0, 255, 0, 255],
                        [255, 0, 0, 255],
                        [255, 255, 0, 255],
                        [255, 0, 255, 255],
                        [0, 255, 255, 255],
                    ];
                    let value = match pattern {
                        0 => seed.to_le_bytes(),
                        1 => colors[(i % tw as usize * 8 / tw as usize).min(7)],
                        _ => {
                            let v = (i % tw as usize) as u8;
                            [v, v, v, seed as u8]
                        }
                    };
                    p.copy_from_slice(&value);
                }
                let mut texture = None;
                device
                    .CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: tw,
                            Height: th,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: D3D11_USAGE_IMMUTABLE,
                            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                            ..Default::default()
                        },
                        Some(&D3D11_SUBRESOURCE_DATA {
                            pSysMem: full.as_ptr().cast(),
                            SysMemPitch: tw * 4,
                            ..Default::default()
                        }),
                        Some(&mut texture),
                    )
                    .unwrap();
                let texture = texture.unwrap();
                let crop = D3D11_BOX {
                    left: 4,
                    top: 2,
                    front: 0,
                    right: w + 4,
                    bottom: h + 2,
                    back: 1,
                };
                gpu.capture(&context, &texture, &crop);
                let mut actual = FrameBuffers::new(w, h, PixelFormat::Nv12).unwrap();
                gpu.read(&context, &mut 0.0, |data, pitch| {
                    actual.copy_nv12(0, data, pitch)
                })
                .unwrap()
                .unwrap();
                let mut expected = FrameBuffers::new(w, h, PixelFormat::Nv12).unwrap();
                expected
                    .convert(0, &full[((2 * tw + 4) * 4) as usize..], tw as usize * 4)
                    .unwrap();
                let (y, uv) = actual.bytes(0).split_at((w * h) as usize);
                assert_eq!(
                    y,
                    &expected.bytes(0)[..(w * h) as usize],
                    "luma {w}x{h} pattern {pattern}"
                );
                let max = uv
                    .iter()
                    .zip(&expected.bytes(0)[(w * h) as usize..])
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap();
                // libyuv SIMD may use two rounded averages instead of one 2x2 average.
                assert!(max <= 1, "chroma {w}x{h} pattern {pattern}: {max}");
                // A preview must read the exact same cropped BGRA pixels,
                // and must not alter NV12 when the next send repeats it.
                let mut preview = None;
                device
                    .CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: w,
                            Height: h,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: D3D11_USAGE_STAGING,
                            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                            ..Default::default()
                        },
                        None,
                        Some(&mut preview),
                    )
                    .unwrap();
                let preview = preview.unwrap();
                gpu.copy_bgra(&context, &preview);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                context
                    .Map(&preview, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .unwrap();
                let guard = super::super::MapGuard {
                    context: &context,
                    texture: &preview,
                };
                let pixels = slice::from_raw_parts(
                    mapped.pData.cast::<u8>(),
                    mapped.RowPitch as usize * h as usize,
                );
                for row in 0..h as usize {
                    let input = ((row + 2) * tw as usize + 4) * 4;
                    let output = row * mapped.RowPitch as usize;
                    assert_eq!(
                        &pixels[output..output + w as usize * 4],
                        &full[input..input + w as usize * 4]
                    );
                }
                drop(guard);
                gpu.read(&context, &mut 0.0, |data, pitch| {
                    actual.copy_nv12(1, data, pitch)
                })
                .unwrap()
                .unwrap();
                assert_eq!(actual.bytes(0), actual.bytes(1));
                println!("PASS {w}x{h} pattern={pattern} Y exact, UV max error={max}");
            }
        }
    }
}
