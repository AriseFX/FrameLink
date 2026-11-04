//! Same-size BGRA to NV12 before CPU readback. No extra frame queue.
//! Raw buffer rows are DWORD-aligned; FrameBuffers removes that padding.
use super::{CaptureError, error};
use crate::pixels::{ColorMatrix, color_matrix};
use std::{slice, time::Instant};
use windows::{
    Win32::Graphics::{
        Direct3D::{Fxc::*, *},
        Direct3D11::*,
        Dxgi::Common::*,
    },
    core::s,
};

pub(super) struct GpuNv12 {
    input: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    output: ID3D11Buffer,
    uav: ID3D11UnorderedAccessView,
    staging: ID3D11Buffer,
    shader: ID3D11ComputeShader,
    width: u32,
    height: u32,
    stride: u32,
}

fn created<T>(value: Option<T>, operation: &str) -> Result<T, CaptureError> {
    value.ok_or_else(|| CaptureError::Fatal(format!("{operation}未返回D3D11资源")))
}

impl GpuNv12 {
    pub fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self, CaptureError> {
        crate::pixels::frame_len(width, height, crate::model::PixelFormat::Nv12)
            .map_err(CaptureError::Fatal)?;
        let stride = width.next_multiple_of(4);
        let length = stride * (height + height / 2);
        let (yc, uc, vc) = match color_matrix(width as usize, height as usize) {
            ColorMatrix::Bt601 => ("66,129,25", "-38,-74,112", "112,-94,-18"),
            ColorMatrix::Bt709 => ("47,157,16", "-26,-86,112", "112,-102,-10"),
            ColorMatrix::Bt2020 => ("59,148,13", "-31,-81,112", "112,-103,-9"),
        };
        let hlsl = format!(
            "#define WIDTH {width}\n#define HEIGHT {height}\n#define STRIDE {stride}\n#define YC int3({yc})\n#define UC int3({uc})\n#define VC int3({vc})\n{}",
            include_str!("nv12.hlsl")
        );
        unsafe {
            if device.GetFeatureLevel().0 < D3D_FEATURE_LEVEL_11_0.0 {
                return Err(CaptureError::Unsupported(
                    "GPU NV12需要D3D功能级别11.0或更高".into(),
                ));
            }
            let (mut code, mut messages) = (None, None);
            if let Err(e) = D3DCompile(
                hlsl.as_ptr().cast(),
                hlsl.len(),
                s!("nv12.hlsl"),
                None,
                None,
                s!("main"),
                s!("cs_5_0"),
                D3DCOMPILE_OPTIMIZATION_LEVEL3,
                0,
                &mut code,
                Some(&mut messages),
            ) {
                let detail = messages
                    .map(|m| {
                        String::from_utf8_lossy(slice::from_raw_parts(
                            m.GetBufferPointer().cast::<u8>(),
                            m.GetBufferSize(),
                        ))
                        .into_owned()
                    })
                    .unwrap_or_default();
                return Err(CaptureError::Fatal(format!(
                    "编译NV12着色器失败：{e} {detail}"
                )));
            }
            let code = created(code, "编译NV12着色器")?;
            let mut shader = None;
            device
                .CreateComputeShader(
                    slice::from_raw_parts(
                        code.GetBufferPointer().cast::<u8>(),
                        code.GetBufferSize(),
                    ),
                    None,
                    Some(&mut shader),
                )
                .map_err(|e| error("创建NV12着色器", e))?;
            let mut input = None;
            device
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: width,
                        Height: height,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut input),
                )
                .map_err(|e| error("创建GPU区域纹理", e))?;
            let input = created(input, "创建GPU区域纹理")?;
            let mut srv = None;
            device
                .CreateShaderResourceView(&input, None, Some(&mut srv))
                .map_err(|e| error("创建GPU区域视图", e))?;
            let mut output = None;
            device
                .CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: length,
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32,
                        MiscFlags: D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut output),
                )
                .map_err(|e| error("创建GPU NV12缓冲", e))?;
            let output = created(output, "创建GPU NV12缓冲")?;
            let mut uav = None;
            device
                .CreateUnorderedAccessView(
                    &output,
                    Some(&D3D11_UNORDERED_ACCESS_VIEW_DESC {
                        Format: DXGI_FORMAT_R32_TYPELESS,
                        ViewDimension: D3D11_UAV_DIMENSION_BUFFER,
                        Anonymous: D3D11_UNORDERED_ACCESS_VIEW_DESC_0 {
                            Buffer: D3D11_BUFFER_UAV {
                                FirstElement: 0,
                                NumElements: length / 4,
                                Flags: D3D11_BUFFER_UAV_FLAG_RAW.0 as u32,
                            },
                        },
                    }),
                    Some(&mut uav),
                )
                .map_err(|e| error("创建GPU NV12写入视图", e))?;
            let mut staging = None;
            device
                .CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: length,
                        Usage: D3D11_USAGE_STAGING,
                        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut staging),
                )
                .map_err(|e| error("创建NV12读取缓冲", e))?;
            Ok(Self {
                input,
                srv: created(srv, "创建GPU区域视图")?,
                output,
                uav: created(uav, "创建GPU NV12写入视图")?,
                staging: created(staging, "创建NV12读取缓冲")?,
                shader: created(shader, "创建NV12着色器")?,
                width,
                height,
                stride,
            })
        }
    }

    pub fn capture(
        &self,
        context: &ID3D11DeviceContext,
        source: &ID3D11Texture2D,
        crop: &D3D11_BOX,
    ) {
        unsafe {
            context.CopySubresourceRegion(&self.input, 0, 0, 0, 0, source, 0, Some(crop));
            context.CSSetShader(&self.shader, None);
            context.CSSetShaderResources(0, Some(&[Some(self.srv.clone())]));
            context.CSSetUnorderedAccessViews(0, 1, Some([Some(self.uav.clone())].as_ptr()), None);
            context.Dispatch(self.width.div_ceil(32), self.height.div_ceil(16), 1);
            context.CSSetUnorderedAccessViews(0, 1, Some([None].as_ptr()), None);
            context.CSSetShaderResources(0, Some(&[None]));
            context.CopyResource(&self.staging, &self.output);
        }
    }

    pub fn copy_bgra(&self, context: &ID3D11DeviceContext, staging: &ID3D11Texture2D) {
        unsafe { context.CopyResource(staging, &self.input) };
    }

    pub fn read<T>(
        &self,
        context: &ID3D11DeviceContext,
        map_ms: &mut f64,
        callback: impl FnOnce(&[u8], usize) -> T,
    ) -> Result<T, CaptureError> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let start = Instant::now();
        let result = unsafe { context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) };
        *map_ms += start.elapsed().as_secs_f64() * 1000.0;
        result.map_err(|e| error("映射GPU NV12缓冲", e))?;
        struct Unmap<'a>(&'a ID3D11DeviceContext, &'a ID3D11Buffer);
        impl Drop for Unmap<'_> {
            fn drop(&mut self) {
                unsafe { self.0.Unmap(self.1, 0) }
            }
        }
        let _guard = Unmap(context, &self.staging);
        if mapped.pData.is_null() {
            return Err(CaptureError::Fatal("NV12映射地址无效".into()));
        }
        let data = unsafe {
            slice::from_raw_parts(
                mapped.pData.cast::<u8>(),
                (self.stride * (self.height + self.height / 2)) as usize,
            )
        };
        Ok(callback(data, self.stride as usize))
    }
}

#[cfg(test)]
#[path = "gpu_nv12_tests.rs"]
mod tests;
