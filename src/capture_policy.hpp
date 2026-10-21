#pragma once
#include <algorithm>
#include <array>
#include <cstdint>

namespace cast {
// Late work starts again immediately. Skipping to the next future slot can halve
// throughput when processing takes only slightly longer than one frame period.
inline double nextFrameDeadline(double previous, double interval, double now) {
    return (std::max)(previous + interval, now);
}
inline double timerDelayMs(double deadline, double now, bool precision) {
    return (std::max)(0.0, deadline - now - (precision ? 0.5 : 0.0));
}
inline bool temporaryDxgiFailure(uint32_t result) {
    switch (result) {
    case 0x887a0005u: // DXGI_ERROR_DEVICE_REMOVED
    case 0x887a0007u: // DXGI_ERROR_DEVICE_RESET
    case 0x887a0022u: // DXGI_ERROR_NOT_CURRENTLY_AVAILABLE
    case 0x887a0026u: // DXGI_ERROR_ACCESS_LOST
    case 0x887a0028u: // DXGI_ERROR_SESSION_DISCONNECTED
    case 0x887a0025u: // DXGI_ERROR_MODE_CHANGE_IN_PROGRESS
    case 0x80070005u: // E_ACCESSDENIED (for example, the secure desktop)
        return true;
    default:
        return false;
    }
}
class CaptureRetry {
    bool pending_ = false;
    double deadline_ = 0;
public:
    void failed(double now) { pending_ = true; deadline_ = now + 1000.0; }
    void recovered() { pending_ = false; }
    bool pending() const { return pending_; }
    bool due(double now) const { return pending_ && now >= deadline_; }
    double deadline() const { return deadline_; }
};

// Only one SDK buffer may be in flight. A changed image uses the other slot;
// a duplicate reuses the current slot without writing to it. BGRA caches belong
// to an image generation, including when preview is enabled after direct NV12.
class LatestFrameSlots {
    bool available_ = false;
    unsigned current_ = 0;
    uint64_t generation_ = 0;
    std::array<uint64_t, 2> bgra_{};
public:
    struct Plan { unsigned slot; bool refresh; };
    Plan prepare(bool changed) {
        bool refresh = changed || !available_;
        if (refresh) ++generation_;
        return {refresh && available_ ? current_ ^ 1u : current_, refresh};
    }
    void submitted(unsigned slot) { current_ = slot; available_ = true; }
    void storedBgra(unsigned slot) { bgra_[slot] = generation_; }
    int cachedBgra() const {
        for (unsigned i = 0; i < bgra_.size(); ++i)
            if (generation_ != 0 && bgra_[i] == generation_) return int(i);
        return -1;
    }
};
}
