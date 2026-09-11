#pragma once

// One fast-forward tick still represents one host-frame interval. Carry its
// deadline across presents so a missed VSync is recovered instead of forgotten.
struct FramePacer {
    static constexpr int max_catchup_ticks = 8;
    // One frame of slack beyond 30 Hz lets near-capacity cores amortize video.
    static constexpr double max_video_interval = 0.050;
    double next_tick = -1.0;
    double last_video = -1.0;
    double render_cost = 0.0;
    double skip_cost = 0.0;

    void reset() {
        next_tick = last_video = -1.0;
        render_cost = skip_cost = 0.0;
    }

    void start(double now, double period) {
        // The first image also needs a fixed deadline across interrupted batches.
        if (last_video < 0.0) last_video = now;
        // Keep catch-up bounded after a stall. Clearing all debt repeatedly
        // under sustained load would restart with an expensive video tick.
        if (next_tick < 0.0) next_tick = now;
        else if (now - next_tick > 4.0 * period) next_tick = now - 4.0 * period;
    }

    bool due(double now) const { return now >= next_tick; }
    bool can_tick(double now, double started, int ticks, double period) const {
        return ticks < max_catchup_ticks && due(now) &&
            now - started < max_catchup_ticks * period;
    }
    bool should_render(double now, double started, int ticks, double period) const {
        // Leave enough time for the final image before another skipped tick.
        // Anchor to the last image. Grant one period of grace only when late
        // and an observed skipped tick is fast enough to recover time.
        const double grace = now - next_tick >= period && skip_cost > 0.0 && skip_cost < period
            ? period : 0.0;
        const double deadline = last_video + max_video_interval + grace;
        const double render_budget = render_cost > 0.0 ? render_cost : period;
        const double skip_budget = skip_cost > 0.0 ? skip_cost :
            (period < render_budget ? period : render_budget);
        return now - next_tick < period ||
            now + skip_budget + render_budget >= deadline || ticks + 1 >= max_catchup_ticks ||
            now - started >= (max_catchup_ticks - 1) * period;
    }
    void record_tick(bool rendered_video, double seconds) {
        (rendered_video ? render_cost : skip_cost) = seconds;
    }
    void presented(double now) { last_video = now; }
    void advance(double period) { next_tick += period; }
};
