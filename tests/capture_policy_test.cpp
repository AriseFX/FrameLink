#include "../src/capture_policy.hpp"
#include <cassert>
#include <cmath>
#include <iostream>

int main() {
    using namespace cast;
    constexpr double interval = 1000.0 / 240.0;
    double now = 0, next = 0;
    for (int frame = 0; frame < 1000; ++frame) {
        now = (std::max)(now, next);
        now += 4.2; // Slightly slower than 240 fps: do not fall to 120 fps.
        next = nextFrameDeadline(next, interval, now);
        assert(next >= now && next <= now + interval);
    }
    assert(std::abs(1000000.0 / now - 238.095238) < 0.001);
    assert(nextFrameDeadline(10, interval, 500) == 500);
    assert(std::abs(nextFrameDeadline(500, interval, 500) - (500 + interval)) < 0.0001);
    assert(timerDelayMs(10, 5, false) == 5);
    assert(timerDelayMs(10, 5, true) == 4.5);
    assert(timerDelayMs(10, 9.75, true) == 0);
    assert(timerDelayMs(10, 11, false) == 0);

    for (uint32_t code : {0x887a0005u, 0x887a0007u, 0x887a0022u, 0x887a0025u, 0x887a0026u, 0x887a0028u, 0x80070005u})
        assert(temporaryDxgiFailure(code));
    for (uint32_t code : {0u, 0x887a0001u, 0x887a0006u, 0x887a0020u, 0x8007000eu, 0x887a0027u})
        assert(!temporaryDxgiFailure(code)); // Invalid use, hung driver, OOM and timeout are distinct.
    CaptureRetry retry;
    assert(!retry.pending() && !retry.due(10000));
    retry.failed(100);
    assert(retry.pending() && retry.deadline() == 1100 && !retry.due(1099) && retry.due(1100));
    retry.failed(1100);
    assert(!retry.due(1101) && retry.due(2100));
    retry.recovered(); assert(!retry.pending() && !retry.due(5000));

    LatestFrameSlots slots;
    auto initial = slots.prepare(false); // A first image always needs pixels.
    assert(initial.refresh && initial.slot == 0 && slots.cachedBgra() == -1);
    slots.storedBgra(initial.slot); slots.submitted(initial.slot);
    for (int repeat = 0; repeat < 1000; ++repeat) {
        auto duplicate = slots.prepare(false);
        assert(!duplicate.refresh && duplicate.slot == 0 && slots.cachedBgra() == 0);
        slots.submitted(duplicate.slot);
    }
    auto fresh = slots.prepare(true);
    assert(fresh.refresh && fresh.slot == 1 && slots.cachedBgra() == -1);
    slots.submitted(fresh.slot); // Direct NV12 left both BGRA caches stale.
    assert(slots.cachedBgra() == -1);
    slots.storedBgra(0); // Preview enabled later; DIB is separate from NV12 SDK buffers.
    assert(slots.cachedBgra() == 0);
    auto duplicate = slots.prepare(false);
    assert(!duplicate.refresh && duplicate.slot == 1 && slots.cachedBgra() == 0);
    slots.submitted(duplicate.slot);
    auto changedAgain = slots.prepare(true);
    assert(changedAgain.refresh && changedAgain.slot == 0 && slots.cachedBgra() == -1);
    slots.storedBgra(changedAgain.slot); slots.submitted(changedAgain.slot);
    assert(slots.cachedBgra() == 0);
    std::cout << "PASS: frame pacing, precision tail, DXGI error classification, recovery and immutable double buffering\n";
}
