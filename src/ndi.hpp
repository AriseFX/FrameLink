#pragma once
#include <windows.h>
#include <cstdint>
#include <cstring>
#include <iterator>
#include <stdexcept>
#include <string>

namespace cast {
struct Source { const char* name; const char* address; };
struct SendSettings { const char* name; const char* groups; bool clockVideo; bool clockAudio; };
struct VideoFrame {
    int width, height, fourcc, rateN, rateD;
    float aspect;
    int format;
    int64_t timecode;
    uint8_t* data;
    int stride;
    const char* metadata;
    int64_t timestamp;
};
struct RecvSettings { Source source; int color, bandwidth; bool fields; const char* name; };
static_assert(sizeof(VideoFrame) == 72, "NDI x64 video ABI");
static_assert(sizeof(SendSettings) == 24, "NDI x64 sender ABI");
static_assert(sizeof(RecvSettings) == 40, "NDI x64 receiver ABI");
constexpr int BGRX = 0x58524742;
inline std::string utf8(const std::wstring& value) {
    int length = WideCharToMultiByte(CP_UTF8, 0, value.data(), int(value.size()), nullptr, 0, nullptr, nullptr);
    std::string text(length, '\0');
    if (length) WideCharToMultiByte(CP_UTF8, 0, value.data(), int(value.size()), &text[0], length, nullptr, nullptr);
    return text;
}
inline std::wstring wide(const std::string& value) {
    int length = MultiByteToWideChar(CP_UTF8, 0, value.data(), int(value.size()), nullptr, 0);
    std::wstring text(length, L'\0');
    if (length) MultiByteToWideChar(CP_UTF8, 0, value.data(), int(value.size()), &text[0], length);
    return text;
}
class Ndi {
    HMODULE module_ = nullptr;
    bool initialized_ = false;
    template<class T> void bind(T& function, const char* symbol) {
        FARPROC address = GetProcAddress(module_, symbol);
        static_assert(sizeof(function) == sizeof(address), "Function pointer ABI");
        std::memcpy(&function, &address, sizeof(function));
        if (!function) throw std::runtime_error(std::string("Missing NDI API: ") + symbol);
    }
public:
    bool (*initialize)() = nullptr;
    void (*destroy)() = nullptr;
    const char* (*version)() = nullptr;
    void* (*sendCreate)(const SendSettings*) = nullptr;
    void (*sendDestroy)(void*) = nullptr;
    void (*sendAsync)(void*, const VideoFrame*) = nullptr;
    int (*connections)(void*, uint32_t) = nullptr;
    const Source* (*sourceName)(void*) = nullptr;
    void* (*recvCreate)(const RecvSettings*) = nullptr;
    void (*recvDestroy)(void*) = nullptr;
    int (*recvCapture)(void*, VideoFrame*, void*, void*, uint32_t) = nullptr;
    void (*recvFreeVideo)(void*, const VideoFrame*) = nullptr;
    Ndi() {
        wchar_t path[32768]; DWORD length = GetModuleFileNameW(nullptr, path, DWORD(std::size(path)));
        if (!length || length >= std::size(path)) throw std::runtime_error("Executable path unavailable");
        std::wstring dll(path, length); dll.resize(dll.find_last_of(L"\\/") + 1); dll += L"Processing.NDI.Lib.x64.dll";
        module_ = LoadLibraryExW(dll.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
        if (!module_) throw std::runtime_error("Cannot load Processing.NDI.Lib.x64.dll. Keep it beside the EXE; install VC++ x64 runtime if needed.");
        try {
            bind(initialize, "NDIlib_initialize"); bind(destroy, "NDIlib_destroy"); bind(version, "NDIlib_version");
            bind(sendCreate, "NDIlib_send_create"); bind(sendDestroy, "NDIlib_send_destroy");
            bind(sendAsync, "NDIlib_send_send_video_async_v2"); bind(connections, "NDIlib_send_get_no_connections");
            bind(sourceName, "NDIlib_send_get_source_name"); bind(recvCreate, "NDIlib_recv_create_v3");
            bind(recvDestroy, "NDIlib_recv_destroy"); bind(recvCapture, "NDIlib_recv_capture_v2"); bind(recvFreeVideo, "NDIlib_recv_free_video_v2");
            initialized_ = initialize(); if (!initialized_) throw std::runtime_error("NDI initialization failed");
        } catch (...) { FreeLibrary(module_); module_ = nullptr; throw; }
    }
    ~Ndi() { if (initialized_) destroy(); if (module_) FreeLibrary(module_); }
    Ndi(const Ndi&) = delete;
    Ndi& operator=(const Ndi&) = delete;
};
}
