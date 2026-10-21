#pragma once
#include "ndi.hpp"
#include "capture_policy.hpp"
#include <d3d11.h>
#include <dxgi1_2.h>
#include <mmsystem.h>
#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>
#include <libyuv/convert_from_argb.h>

namespace cast {
template<class T> class Com {
    T* p_ = nullptr;
public:
    ~Com() { reset(); }
    Com() = default;
    Com(const Com&) = delete;
    Com& operator=(const Com&) = delete;
    T* get() const { return p_; }
    T* operator->() const { return p_; }
    T** put() { reset(); return &p_; }
    void reset() { if (p_) p_->Release(); p_ = nullptr; }
};
class DxgiUnavailable : public std::runtime_error {
public:
    using std::runtime_error::runtime_error;
};
class DxgiUnsupported : public std::runtime_error {
public:
    using std::runtime_error::runtime_error;
};
class GdiCaptureFailure : public std::runtime_error {
public:
    using std::runtime_error::runtime_error;
};
inline void check(HRESULT hr, const char* operation) {
    if (FAILED(hr)) {
        char message[240]; snprintf(message, sizeof(message), "%s failed (0x%08lX)", operation, (unsigned long)hr);
        if (temporaryDxgiFailure(uint32_t(hr))) throw DxgiUnavailable(message);
        throw std::runtime_error(message);
    }
}
inline double milliseconds() {
    static const double scale = [] { LARGE_INTEGER frequency; QueryPerformanceFrequency(&frequency); return 1000.0 / double(frequency.QuadPart); }();
    LARGE_INTEGER value; QueryPerformanceCounter(&value); return double(value.QuadPart) * scale;
}
inline int64_t utcTime() {
    FILETIME time; GetSystemTimePreciseAsFileTime(&time);
    ULARGE_INTEGER value; value.LowPart = time.dwLowDateTime; value.HighPart = time.dwHighDateTime;
    return int64_t(value.QuadPart - 116444736000000000ULL);
}
inline RECT desktopBounds() {
    int x = GetSystemMetrics(SM_XVIRTUALSCREEN), y = GetSystemMetrics(SM_YVIRTUALSCREEN);
    return {x, y, x + GetSystemMetrics(SM_CXVIRTUALSCREEN), y + GetSystemMetrics(SM_CYVIRTUALSCREEN)};
}
inline bool contains(RECT outer, RECT inner) {
    return inner.left >= outer.left && inner.top >= outer.top && inner.right <= outer.right && inner.bottom <= outer.bottom;
}
inline RECT centeredRegion(RECT monitor,int size) {
    if(size<2 || size>monitor.right-monitor.left || size>monitor.bottom-monitor.top)
        throw std::runtime_error("Center region does not fit the selected monitor");
    LONG x=monitor.left+(monitor.right-monitor.left-size)/2;
    LONG y=monitor.top+(monitor.bottom-monitor.top-size)/2;
    return {x,y,x+size,y+size};
}
enum class Backend { Auto, Dxgi, Gdi };
enum class PixelFormat { Bgrx, Nv12 };
struct Config {
    std::wstring name = L"Screen Region";
    int x = 0, y = 0, width = 1280, height = 720, fps = 60;
    bool cursor = true, unlimited = false, precision = false;
    Backend backend = Backend::Auto;
    PixelFormat pixel = PixelFormat::Nv12;
    RECT region() const { return {x, y, x + width, y + height}; }
    void validate() const {
        if (name.empty() || utf8(name).size() > 180 || name.find_first_of(L"\\/:*?\"<>|") != std::wstring::npos)
            throw std::runtime_error("Source name is empty, too long, or contains invalid characters");
        if (width < 2 || width > 32768 || (width & 1) || height < 2 || height > 32768 || int64_t(width) * height > 33177600)
            throw std::runtime_error("Width must be even; total pixels cannot exceed 33177600");
        if (fps < 1 || fps > 1000) throw std::runtime_error("Locked FPS must be 1-1000");
        if (int(backend) < 0 || int(backend) > 2) throw std::runtime_error("Invalid capture backend");
        if (pixel != PixelFormat::Bgrx && pixel != PixelFormat::Nv12) throw std::runtime_error("Invalid pixel format");
        if (pixel == PixelFormat::Nv12 && (height & 1)) throw std::runtime_error("NV12 height must be even");
        if (x < -100000 || x > 100000 || y < -100000 || y > 100000 || !contains(desktopBounds(), region()))
            throw std::runtime_error("Capture region is outside the desktop");
        if (unlimited && backend == Backend::Gdi) throw std::runtime_error("Unlimited mode requires DXGI");
    }
};
class Buffer {
    HDC screen_ = nullptr, dc_ = nullptr;
    HBITMAP bitmap_ = nullptr;
    HGDIOBJ previous_ = nullptr;
public:
    uint8_t* data = nullptr;
    int width, height, stride;
    Buffer(int w, int h) : width(w), height(h), stride(w * 4) {
        screen_ = GetDC(nullptr); dc_ = CreateCompatibleDC(screen_);
        BITMAPINFO info{}; info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER); info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h; info.bmiHeader.biPlanes = 1; info.bmiHeader.biBitCount = 32;
        void* pixels = nullptr; bitmap_ = CreateDIBSection(screen_, &info, DIB_RGB_COLORS, &pixels, nullptr, 0);
        if (!screen_ || !dc_ || !bitmap_) { release(); throw std::runtime_error("Desktop buffer allocation failed"); }
        data = static_cast<uint8_t*>(pixels); previous_ = SelectObject(dc_, bitmap_);
        if (!previous_ || previous_ == HGDI_ERROR) { release(); throw std::runtime_error("Desktop buffer selection failed"); }
    }
    ~Buffer() { release(); }
    Buffer(const Buffer&) = delete;
    void release() {
        if (dc_ && previous_ && previous_ != HGDI_ERROR) SelectObject(dc_, previous_);
        if (bitmap_) DeleteObject(bitmap_); if (dc_) DeleteDC(dc_); if (screen_) ReleaseDC(nullptr, screen_);
        bitmap_ = nullptr; dc_ = screen_ = nullptr; previous_ = nullptr;
    }
    void drawCursor(int x, int y) {
        CURSORINFO cursor{sizeof(CURSORINFO)};
        if (!GetCursorInfo(&cursor) || cursor.flags != CURSOR_SHOWING) return;
        ICONINFO icon{}; if (!GetIconInfo(cursor.hCursor, &icon)) return;
        DrawIconEx(dc_, cursor.ptScreenPos.x - x - int(icon.xHotspot), cursor.ptScreenPos.y - y - int(icon.yHotspot), cursor.hCursor, 0, 0, 0, nullptr, DI_NORMAL);
        if (icon.hbmMask) DeleteObject(icon.hbmMask); if (icon.hbmColor) DeleteObject(icon.hbmColor); GdiFlush();
    }
    void capture(const Config& c) {
        if (!BitBlt(dc_, 0, 0, width, height, screen_, c.x, c.y, SRCCOPY | CAPTUREBLT))
            throw GdiCaptureFailure("Desktop capture failed; check display and locked desktop");
        if (c.cursor) drawCursor(c.x, c.y); GdiFlush();
    }
    void copy(const void* pixels, int pitch) {
        if (pitch < stride) throw std::runtime_error("Invalid image pitch");
        for (int row = 0; row < height; ++row) memcpy(data + row * stride, static_cast<const uint8_t*>(pixels) + row * pitch, stride);
    }
    HBITMAP preview(int& w, int& h) {
        double ratio = (std::min)(1.0, (std::min)(640.0 / width, 360.0 / height));
        w = (std::max)(1, int(width * ratio)); h = (std::max)(1, int(height * ratio));
        HDC target = CreateCompatibleDC(screen_); if (!target) return nullptr;
        BITMAPINFO info{}; info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER); info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h; info.bmiHeader.biPlanes = 1; info.bmiHeader.biBitCount = 32;
        void* pixels = nullptr; HBITMAP result = CreateDIBSection(screen_, &info, DIB_RGB_COLORS, &pixels, nullptr, 0);
        if (result) {
            HGDIOBJ old = SelectObject(target, result); SetStretchBltMode(target, HALFTONE); SetBrushOrgEx(target, 0, 0, nullptr);
            StretchBlt(target, 0, 0, w, h, dc_, 0, 0, width, height, SRCCOPY); GdiFlush(); SelectObject(target, old);
        }
        DeleteDC(target); return result;
    }
};
class Nv12Buffer {
    int width_, height_;
    std::vector<uint8_t> pixels_;
public:
    Nv12Buffer(int width, int height) : width_(width), height_(height) {
        if (width < 2 || height < 2 || (width & 1) || (height & 1) || int64_t(width) * height > 33177600)
            throw std::runtime_error("Invalid NV12 buffer dimensions");
        pixels_.resize(size_t(width) * height * 3 / 2);
    }
    uint8_t* data() { return pixels_.data(); }
    uint8_t* convert(const uint8_t* input, int pitch, int width, int height) {
        if (!input || width != width_ || height != height_ || pitch < width * 4)
            throw std::runtime_error("Invalid NV12 conversion input");
        const auto* matrix = width > 1920 || height > 1080 ? &libyuv::kArgbU2020Constants :
            width > 720 || height > 576 ? &libyuv::kArgbH709Constants : &libyuv::kArgbI601Constants;
        if (libyuv::ARGBToNV12Matrix(input, pitch, pixels_.data(), width,
            pixels_.data() + size_t(width) * height, width, matrix, width, height) != 0)
            throw std::runtime_error("NV12 conversion failed");
        return pixels_.data();
    }
    uint8_t* convert(const Buffer& input) { return convert(input.data, input.stride, input.width, input.height); }
};
struct AcquireResult { bool available, changed; };
class Capture {
public:
    virtual ~Capture() = default;
    virtual AcquireResult acquire() = 0;
    virtual void copyTo(Buffer&) = 0;
    virtual bool directNv12() const { return false; }
    // Return the readback completion time so direct conversion retains the
    // same capture/conversion timing boundary as the BGRA path.
    virtual double convertTo(Nv12Buffer&) { throw std::runtime_error("Direct NV12 is unavailable for this backend"); }
    bool grab(Buffer& buffer) {
        if (!acquire().available) return false;
        copyTo(buffer); return true;
    }
    virtual int nominalFps() const = 0;
};
class GdiCapture : public Capture {
    Config config_;
public:
    explicit GdiCapture(Config config) : config_(config) {}
    AcquireResult acquire() override { return {true, true}; }
    void copyTo(Buffer& buffer) override { buffer.capture(config_); }
    int nominalFps() const override { return config_.fps; }
};
class DxgiCapture : public Capture {
    Config config_;
    Com<ID3D11Device> device_;
    Com<ID3D11DeviceContext> context_;
    Com<IDXGIOutputDuplication> duplication_;
    Com<ID3D11Texture2D> staging_;
    RECT bounds_{};
    int nominal_ = 60;
    bool hasImage_ = false, separateCursor_ = false;
    double lastFrame_ = 0;
public:
    explicit DxgiCapture(Config config) : config_(config) {
        Com<IDXGIFactory1> factory;
        check(CreateDXGIFactory1(__uuidof(IDXGIFactory1), reinterpret_cast<void**>(factory.put())), "CreateDXGIFactory1");
        for (UINT i = 0;; ++i) {
            Com<IDXGIAdapter1> adapter; HRESULT hr = factory->EnumAdapters1(i, adapter.put());
            if (hr == DXGI_ERROR_NOT_FOUND) break; check(hr, "EnumAdapters1");
            for (UINT j = 0;; ++j) {
                Com<IDXGIOutput> output; hr = adapter->EnumOutputs(j, output.put());
                if (hr == DXGI_ERROR_NOT_FOUND) break; check(hr, "EnumOutputs");
                DXGI_OUTPUT_DESC description{}; check(output->GetDesc(&description), "Output description");
                if (!description.AttachedToDesktop || !contains(description.DesktopCoordinates, config.region())) continue;
                if (description.Rotation != DXGI_MODE_ROTATION_IDENTITY && description.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED)
                    throw DxgiUnsupported("Rotated monitor needs GDI capture");
                check(D3D11CreateDevice(adapter.get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0, D3D11_SDK_VERSION, device_.put(), nullptr, context_.put()), "D3D11CreateDevice");
                Com<IDXGIOutput1> output1; check(output->QueryInterface(__uuidof(IDXGIOutput1), reinterpret_cast<void**>(output1.put())), "IDXGIOutput1");
                hr = output1->DuplicateOutput(device_.get(), duplication_.put());
                // Unsupported *display modes* may become available after a mode
                // switch. Unsupported APIs and invalid arguments remain fatal.
                if (hr == DXGI_ERROR_UNSUPPORTED) throw DxgiUnavailable("DXGI display mode is temporarily unsupported");
                check(hr, "DuplicateOutput"); bounds_ = description.DesktopCoordinates;
                DEVMODEW mode{}; mode.dmSize = sizeof(mode); nominal_ = config.fps;
                if (config.unlimited && EnumDisplaySettingsW(description.DeviceName, ENUM_CURRENT_SETTINGS, &mode) && mode.dmDisplayFrequency > 1 && mode.dmDisplayFrequency <= 1000) nominal_ = int(mode.dmDisplayFrequency);
                D3D11_TEXTURE2D_DESC texture{}; texture.Width = config.width; texture.Height = config.height;
                texture.MipLevels = texture.ArraySize = 1; texture.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
                texture.SampleDesc.Count = 1; texture.Usage = D3D11_USAGE_STAGING; texture.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
                check(device_->CreateTexture2D(&texture, nullptr, staging_.put()), "Create staging texture"); return;
            }
        }
        throw DxgiUnsupported("DXGI region must fit inside a single connected monitor");
    }
    int nominalFps() const override { return nominal_; }
    AcquireResult acquire() override {
        DXGI_OUTDUPL_FRAME_INFO info{}; Com<IDXGIResource> resource;
        HRESULT hr = duplication_->AcquireNextFrame(config_.unlimited ? 20 : 0, &info, resource.put());
        bool changed = false;
        if (SUCCEEDED(hr)) {
            try {
                if (info.LastMouseUpdateTime.QuadPart) {
                    separateCursor_ = info.PointerPosition.Visible;
                    changed = config_.cursor;
                }
                if (config_.cursor && info.PointerShapeBufferSize) changed = true;
                if (info.LastPresentTime.QuadPart || !hasImage_) {
                    Com<ID3D11Texture2D> texture; check(resource->QueryInterface(__uuidof(ID3D11Texture2D), reinterpret_cast<void**>(texture.put())), "Desktop texture");
                    UINT x = config_.x - bounds_.left, y = config_.y - bounds_.top;
                    D3D11_BOX box{x, y, 0, x + UINT(config_.width), y + UINT(config_.height), 1};
                    context_->CopySubresourceRegion(staging_.get(), 0, 0, 0, 0, texture.get(), 0, &box); hasImage_ = true; changed = true;
                }
            } catch (...) { duplication_->ReleaseFrame(); throw; }
            check(duplication_->ReleaseFrame(), "Release desktop frame");
        } else if (hr != DXGI_ERROR_WAIT_TIMEOUT) check(hr, "Acquire desktop frame");
        if (!hasImage_) return {false, false};
        double now = milliseconds();
        if (config_.unlimited && !changed && now - lastFrame_ < 1000) return {false, false};
        lastFrame_ = now; return {true, changed};
    }
    void copyTo(Buffer& target) override {
        if (!hasImage_) throw std::runtime_error("No DXGI image is available");
        if (target.width != config_.width || target.height != config_.height) throw std::runtime_error("Capture buffer dimensions do not match");
        D3D11_MAPPED_SUBRESOURCE mapped{}; check(context_->Map(staging_.get(), 0, D3D11_MAP_READ, 0, &mapped), "Readback map");
        try { target.copy(mapped.pData, int(mapped.RowPitch)); } catch (...) { context_->Unmap(staging_.get(), 0); throw; }
        context_->Unmap(staging_.get(), 0); if (config_.cursor && separateCursor_) target.drawCursor(config_.x, config_.y);
    }
    bool directNv12() const override { return !config_.cursor; }
    double convertTo(Nv12Buffer& target) override {
        if (!hasImage_ || config_.cursor) throw std::runtime_error("Direct DXGI NV12 conversion is unavailable");
        D3D11_MAPPED_SUBRESOURCE mapped{}; check(context_->Map(staging_.get(), 0, D3D11_MAP_READ, 0, &mapped), "NV12 readback map");
        double readbackEnd = milliseconds();
        try { target.convert(static_cast<const uint8_t*>(mapped.pData), int(mapped.RowPitch), config_.width, config_.height); }
        catch (...) { context_->Unmap(staging_.get(), 0); throw; }
        context_->Unmap(staging_.get(), 0); return readbackEnd;
    }
};
class Pacer {
    HANDLE timer_ = nullptr, stop_;
    bool resolution_ = false, precision_ = false;
public:
    explicit Pacer(HANDLE stop, bool precision = false) : stop_(stop), precision_(precision) {
        timer_ = CreateWaitableTimerExW(nullptr, nullptr, 2, TIMER_ALL_ACCESS);
        if (!timer_) { timer_ = CreateWaitableTimerExW(nullptr, nullptr, 0, TIMER_ALL_ACCESS); resolution_ = timeBeginPeriod(1) == TIMERR_NOERROR; }
        if (!timer_) { if (resolution_) timeEndPeriod(1); throw std::runtime_error("Cannot create frame timer"); }
    }
    ~Pacer() { CloseHandle(timer_); if (resolution_) timeEndPeriod(1); }
    bool until(double deadline) {
        if (WaitForSingleObject(stop_, 0) == WAIT_OBJECT_0) return false;
        // A normal wait never spins. Precision mode reserves at most the final
        // 0.5 ms; if a timer wakes early, wait again before entering that tail.
        for (;;) {
            double now = milliseconds(); if (now >= deadline) return true;
            double delay = timerDelayMs(deadline, now, precision_);
            if (delay > 0) {
                LARGE_INTEGER due; due.QuadPart = -(std::max)(1LL, (long long)(delay * 10000));
                if (!SetWaitableTimer(timer_, &due, 0, nullptr, nullptr, FALSE)) throw std::runtime_error("Frame timer failed");
                HANDLE events[]{stop_, timer_}; DWORD result = WaitForMultipleObjects(2, events, FALSE, INFINITE);
                if (result == WAIT_FAILED) throw std::runtime_error("Frame wait failed");
                if (result == WAIT_OBJECT_0) return false;
            } else {
                if (WaitForSingleObject(stop_, 0) == WAIT_OBJECT_0) return false;
                YieldProcessor();
            }
        }
    }
};
struct Status { std::wstring source, backend, error; double captureMs = 0, conversionMs = 0, submitMs = 0; };
class Engine {
    Ndi& ndi_;
    HANDLE stop_ = nullptr;
    std::thread thread_;
    mutable std::mutex mutex_;
    Status status_;
    HBITMAP preview_ = nullptr;
    int previewW_ = 0, previewH_ = 0;
    void setBackend(std::wstring value) { std::lock_guard<std::mutex> lock(mutex_); status_.backend = std::move(value); }
    void run(Config config, void* sender) noexcept {
        try {
            std::unique_ptr<Capture> capture;
            CaptureRetry retry;
            const bool allowFallback = config.backend == Backend::Auto && !config.unlimited;
            auto waitForDxgi = [&](const char* reason) {
                capture.reset(); retry.failed(milliseconds());
                if (allowFallback) {
                    capture = std::make_unique<GdiCapture>(config);
                    setBackend(L"GDI · 等待恢复 DXGI · " + wide(reason));
                } else setBackend(L"DXGI · 等待恢复 · " + wide(reason));
            };
            auto createDxgi = [&] {
                try {
                    auto replacement = std::make_unique<DxgiCapture>(config);
                    capture = std::move(replacement); retry.recovered(); setBackend(L"DXGI");
                } catch (const DxgiUnavailable& ex) { waitForDxgi(ex.what()); }
                catch (const DxgiUnsupported& ex) {
                    // Auto supports regions that DXGI cannot represent, such as
                    // rotated or multiple monitors. Retry also detects layout
                    // changes that make the same region usable by DXGI later.
                    if (!allowFallback && !retry.pending()) throw;
                    waitForDxgi(ex.what());
                }
            };
            if (config.backend == Backend::Gdi) { capture = std::make_unique<GdiCapture>(config); setBackend(L"GDI"); }
            else createDxgi();
            Buffer first(config.width, config.height), second(config.width, config.height); Pacer pacer(stop_, config.precision);
            std::unique_ptr<Nv12Buffer> nv12[2];
            if(config.pixel==PixelFormat::Nv12) for(auto& buffer:nv12) buffer=std::make_unique<Nv12Buffer>(config.width,config.height);
            Buffer* buffers[]{&first, &second}; LatestFrameSlots slots;
            VideoFrame frame{config.width, config.height, BGRX, capture ? capture->nominalFps() : config.fps, 1, float(config.width)/config.height, 1, INT64_MAX, nullptr, first.stride, nullptr, 0};
            if(config.pixel==PixelFormat::Nv12) { frame.fourcc=0x3231564e; frame.stride=config.width; }
            double interval = 1000.0/config.fps, next = milliseconds(), lastStats = next, lastPreview = 0;
            uint64_t lastCount = 0; double captureTotal = 0, conversionTotal = 0, submitTotal = 0; unsigned measurements = 0;
            try {
                while (config.unlimited ? WaitForSingleObject(stop_, 0) != WAIT_OBJECT_0 : pacer.until(next)) {
                    if (!contains(desktopBounds(), config.region())) throw std::runtime_error("Display layout changed; reselect capture region");
                    if (retry.due(milliseconds())) createDxgi();
                    if (!capture) {
                        // Forced DXGI and unlimited mode retain the source and
                        // wait on the stop event between bounded retry attempts.
                        actualFps = 0; receivers = ndi_.connections(sender, 0);
                        { std::lock_guard<std::mutex> lock(mutex_); status_.captureMs = status_.conversionMs = status_.submitMs = 0; }
                        captureTotal = conversionTotal = submitTotal = 0; measurements = 0;
                        if (!pacer.until(retry.deadline())) break;
                        next = milliseconds(); lastStats = next; lastCount = frames.load(); continue;
                    }
                    try {
                        double begin = milliseconds(); bool previewDue = previewEnabled.load() && begin - lastPreview >= 500;
                        AcquireResult acquired = capture->acquire();
                        if (acquired.available) {
                            auto plan = slots.prepare(acquired.changed); unsigned index = plan.slot;
                            double grabbed = milliseconds(), converted = grabbed;
                            if (plan.refresh) {
                                if (nv12[index] && capture->directNv12() && !previewDue) {
                                    // Convert directly from the mapped, pitched
                                    // GPU readback; no intermediate BGRA memcpy.
                                    grabbed = capture->convertTo(*nv12[index]); converted = milliseconds();
                                } else {
                                    capture->copyTo(*buffers[index]); slots.storedBgra(index); grabbed = milliseconds();
                                    if (nv12[index]) nv12[index]->convert(*buffers[index]);
                                    converted = milliseconds();
                                }
                            }
                            frame.data = nv12[index] ? nv12[index]->data() : buffers[index]->data;
                            frame.rateN = capture->nominalFps(); frame.timecode = utcTime();
                            // The current slot stays immutable until the next
                            // async call returns. Repeats reuse its same pointer;
                            // fresh pixels are written only into the other slot.
                            ndi_.sendAsync(sender, &frame); double submitted = milliseconds(); slots.submitted(index); ++frames;
                            if (plan.refresh) ++imageUpdates; else ++repeatedFrames;
                            captureTotal += grabbed - begin; conversionTotal += converted - grabbed; submitTotal += submitted - converted; ++measurements;
                            if (previewEnabled && submitted - lastPreview >= 500) {
                                int previewIndex = slots.cachedBgra();
                                if (previewIndex < 0) {
                                    // Only direct NV12 can lack a BGRA cache.
                                    // These DIBs are never submitted in NV12 mode.
                                    capture->copyTo(first); slots.storedBgra(0); previewIndex = 0;
                                }
                                int w, h; HBITMAP image = buffers[previewIndex]->preview(w, h);
                                if (image) { std::lock_guard<std::mutex> lock(mutex_); if (preview_) DeleteObject(preview_); preview_ = image; previewW_ = w; previewH_ = h; }
                                lastPreview = submitted;
                            }
                        }
                    } catch (const DxgiUnavailable& ex) {
                        if (!dynamic_cast<DxgiCapture*>(capture.get())) throw;
                        waitForDxgi(ex.what());
                    } catch (const GdiCaptureFailure& ex) {
                        // A secure/locked desktop may reject both backends.
                        // Only an existing Auto recovery can wait through this;
                        // explicit GDI failures and all other errors terminate.
                        if (!allowFallback || !retry.pending() || !dynamic_cast<GdiCapture*>(capture.get())) throw;
                        capture.reset(); retry.failed(milliseconds());
                        setBackend(L"DXGI · 等待恢复 · " + wide(ex.what()));
                    }
                    double now = milliseconds();
                    if (now - lastStats >= 1000) {
                        uint64_t count = frames.load(); actualFps = (count - lastCount) * 1000.0/(now-lastStats); receivers = ndi_.connections(sender, 0);
                        { std::lock_guard<std::mutex> lock(mutex_); status_.captureMs = measurements ? captureTotal/measurements : 0; status_.conversionMs = measurements ? conversionTotal/measurements : 0; status_.submitMs = measurements ? submitTotal/measurements : 0; }
                        captureTotal = conversionTotal = submitTotal = 0; measurements = 0; lastCount = count; lastStats = now;
                    }
                    if (!config.unlimited) next = nextFrameDeadline(next, interval, now);
                }
            } catch (...) { ndi_.sendAsync(sender, nullptr); throw; }
            ndi_.sendAsync(sender, nullptr);
        } catch (const std::exception& ex) { std::lock_guard<std::mutex> lock(mutex_); status_.error = wide(ex.what()); }
        catch (...) { std::lock_guard<std::mutex> lock(mutex_); status_.error = L"Unexpected capture failure"; }
        ndi_.sendDestroy(sender); running = false;
    }
public:
    std::atomic<bool> running{false}, previewEnabled{true};
    std::atomic<double> actualFps{0};
    std::atomic<int> receivers{0};
    std::atomic<uint64_t> frames{0}, imageUpdates{0}, repeatedFrames{0};
    explicit Engine(Ndi& ndi) : ndi_(ndi) {}
    ~Engine() { stop(); if (stop_) CloseHandle(stop_); if (preview_) DeleteObject(preview_); }
    void start(Config config) {
        config.validate(); stop();
        if (stop_) CloseHandle(stop_); stop_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (!stop_) throw std::runtime_error("Cannot create stop event");
        frames = 0; imageUpdates = 0; repeatedFrames = 0; actualFps = 0; receivers = 0;
        std::string name = utf8(config.name); SendSettings settings{name.c_str(), nullptr, false, false};
        void* sender = ndi_.sendCreate(&settings); if (!sender) throw std::runtime_error("Cannot create NDI source");
        try {
            { std::lock_guard<std::mutex> lock(mutex_); status_ = {}; const Source* source = ndi_.sourceName(sender); if (source && source->name) status_.source = wide(source->name); if (preview_) DeleteObject(preview_); preview_ = nullptr; }
            running = true; thread_ = std::thread(&Engine::run, this, config, sender);
        } catch (...) { running = false; ndi_.sendDestroy(sender); throw; }
    }
    void requestStop() { if (stop_) SetEvent(stop_); }
    void stop() { requestStop(); if (thread_.joinable()) thread_.join(); }
    Status status() const { std::lock_guard<std::mutex> lock(mutex_); return status_; }
    void drawPreview(HDC dc, RECT area) {
        std::lock_guard<std::mutex> lock(mutex_); if (!preview_) return;
        double ratio = (std::min)(double(area.right-area.left)/previewW_, double(area.bottom-area.top)/previewH_);
        int w = int(previewW_*ratio), h = int(previewH_*ratio);
        HDC source = CreateCompatibleDC(dc); HGDIOBJ old = SelectObject(source, preview_);
        SetStretchBltMode(dc, HALFTONE); SetBrushOrgEx(dc, 0, 0, nullptr);
        StretchBlt(dc, area.left+(area.right-area.left-w)/2, area.top+(area.bottom-area.top-h)/2, w,h,source,0,0,previewW_,previewH_,SRCCOPY);
        SelectObject(source,old); DeleteDC(source);
    }
};
}
