#include "frame_pacing.h"
#include <algorithm>
#include <cmath>
#include <iostream>
#include <utility>

static int failures = 0;
static void check(bool ok, const char* message) {
    if (!ok) { std::cerr << message << '\n'; ++failures; }
}

// A deterministic display clock exposes the missed-VSync failure without
// depending on the CI machine's GPU or scheduler. One paced tick is five DS
// frames at 5x, so a measured ratio of 1 here means the requested 5x is delivered.
static double delivery(double display_hz, double tick_seconds, double render_seconds = 0.0,
                       double skipped_tick_seconds = -1.0, double* max_video_gap = nullptr,
                       double* discarded_time = nullptr, double batch_overhead = 0.0) {
    constexpr double period = 560190.0 / 33513982.0;
    FramePacer pacer;
    double now = 0.0;
    double last_video = 0.0, largest_gap = 0.0;
    double discarded = 0.0;
    unsigned ticks = 0;
    while (now < 60.0) {
        now += batch_overhead;
        const double started = now;
        const double previous_deadline = pacer.next_tick;
        pacer.start(now, period);
        if (previous_deadline >= 0.0) discarded += pacer.next_tick - previous_deadline;
        unsigned rendered = 0;
        for (int batch = 0; pacer.can_tick(now, started, batch, period); ++batch) {
            check(now >= pacer.next_tick, "catch-up started an early tick");
            const bool render_video = pacer.should_render(now, started, batch, period);
            const double cost = render_video || skipped_tick_seconds < 0.0 ? tick_seconds : skipped_tick_seconds;
            now += cost;
            pacer.record_tick(render_video, cost);
            pacer.advance(period);
            ++ticks;
            if (render_video) { ++rendered; break; }
        }
        check(rendered <= 1, "catch-up composed an image that was overwritten in the same batch");
        now += render_seconds;
        if (display_hz > 0.0) {
            now = (std::floor(now * display_hz + 1e-8) + 1.0) / display_hz;
        } else {
            now = std::max(now, pacer.next_tick);
        }
        if (rendered) {
            largest_gap = std::max(largest_gap, now - last_video);
            last_video = now;
            pacer.presented(now);
        }
    }
    if (max_video_gap) *max_video_gap = largest_gap;
    if (discarded_time) *discarded_time = discarded;
    return ticks * period / now;
}

int main() {
    // Slower displays need several ticks per present; faster displays also
    // have zero-tick loops. Neither may change the delivered emulation rate.
    for (auto scenario : {std::pair{60.0, 0.010}, {30.0, 0.010},
                           {144.0, 0.010}, {0.0, 0.010}, {144.0, 0.0005}}) {
        const double actual = delivery(scenario.first, scenario.second);
        check(actual > 0.99 && actual < 1.01, "missed refresh reduced delivery or ran ahead of requested speed");
    }
    // Presenting costs CPU time even without VSync. Near-capacity emulation
    // must amortize it instead of silently discarding the missed deadlines.
    for (double display_hz : {30.0, 60.0, 144.0, 0.0}) {
        const double actual = delivery(display_hz, 0.012, 0.002, 0.006);
        check(actual > 0.99 && actual < 1.01, "presentation cost reduced achievable fast-forward speed");
        // Two skipped ticks and one image cost 48 ms here: enough for 5x
        // without VSync, with regular images instead of a long catch-up gap.
        // A slower display can still make the actual throughput fall short.
        double gap = 0.0, discarded = 0.0;
        const double heavy = delivery(display_hz, 0.020, 0.002, 0.013, &gap, &discarded);
        std::cout << "heavy: display_hz=" << display_hz << " delivery=" << heavy
                  << " max_video_gap_ms=" << 1000.0 * gap << " discarded_s=" << discarded << '\n';
        check(heavy > 0.70 && heavy < 1.01, "heavy visual workload lost progress or ran ahead");
        if (display_hz == 0.0) {
            check(heavy > 0.99, "visual deadline reduced achievable fast-forward speed");
        }
        const double refresh = display_hz > 0.0 ? 1.0 / display_hz : 0.0;
        constexpr double period = 560190.0 / 33513982.0;
        check(gap <= FramePacer::max_video_interval + period + 0.002 + refresh + 0.002,
            "visual deadline allowed a long catch-up gap");

        // A small amount of UI work makes a hard 50 ms video deadline force
        // too many expensive images. One period of grace preserves progress.
        const double near_capacity = delivery(display_hz, 0.025, 0.002, 0.012, &gap, nullptr, 0.002);
        std::cout << "near-capacity: display_hz=" << display_hz << " delivery=" << near_capacity
                  << " max_video_gap_ms=" << 1000.0 * gap << '\n';
        check(near_capacity > 0.95 && near_capacity < 1.01,
            "video deadline discarded useful near-capacity progress or ran ahead");
        if (display_hz == 0.0) {
            check(near_capacity > 0.99, "visual deadline prevented achievable speed with UI overhead");
        }
        check(gap <= FramePacer::max_video_interval + period + 0.002 + refresh + 0.002,
            "video grace allowed a long near-capacity catch-up gap");
    }
    check(delivery(60.0, 0.030) < 0.7, "a slow core must not claim requested speed");

    FramePacer pacer;
    pacer.start(0.0, 0.02);
    pacer.advance(0.02);
    check(!pacer.due(0.019), "tick ran before its deadline");
    pacer.start(10.0, 0.02);
    check(std::abs(pacer.next_tick - 9.92) < 1e-9,
        "long stall must preserve only four periods of catch-up debt");
    pacer.advance(0.02);
    pacer.reset(); // load/reset/resume must forget the old session's deadline
    pacer.start(10.005, 0.02);
    check(pacer.due(10.005), "reset delayed the first tick of a new session");
    check(!pacer.can_tick(10.005, 10.005, FramePacer::max_catchup_ticks, 0.02),
        "catch-up exceeded its tick bound");
    check(!pacer.can_tick(10.205, 10.005, 1, 0.02),
        "catch-up exceeded its work-time bound");
    check(pacer.should_render(10.005, 10.005, 0, 0.02),
        "a tick on time must produce video");
    pacer.record_tick(true, 0.010);
    pacer.record_tick(false, 0.006);
    pacer.presented(10.055);
    check(!pacer.should_render(10.055, 10.055, 0, 0.02),
        "an intermediate overdue tick must skip video");
    check(pacer.should_render(10.055, 10.055, FramePacer::max_catchup_ticks - 1, 0.02),
        "the final permitted tick must produce video");
    check(pacer.should_render(10.155, 10.005, 3, 0.02),
        "the work-time limit must leave a final video tick");
    check(pacer.should_render(10.115, 10.055, 1, 0.02),
        "another skipped tick would leave no time for the next image");
    check(pacer.should_render(10.130, 10.130, 0, 0.02),
        "visual deadline was reset by a new batch with accumulated debt");

    FramePacer learning;
    constexpr double nds_period = 560190.0 / 33513982.0;
    learning.start(0.0, nds_period);
    learning.record_tick(true, 0.020);
    learning.presented(0.030);
    check(!learning.should_render(0.030, 0.030, 0, nds_period),
        "unknown skip cost prevented learning a useful omission");
    learning.record_tick(false, 0.013);
    learning.advance(nds_period);
    check(!learning.should_render(0.043, 0.030, 1, nds_period),
        "visual slack prevented amortizing a second omitted tick");
    learning.advance(nds_period);
    check(!learning.should_render(0.056, 0.030, 2, nds_period),
        "overdue emulation did not receive one period of video grace");
    learning.advance(nds_period);
    check(learning.should_render(0.069, 0.030, 3, nds_period),
        "video grace permitted another omission beyond the image deadline");

    FramePacer capacity;
    capacity.start(0.0, nds_period);
    capacity.record_tick(true, 0.025);
    capacity.presented(0.030);
    for (double skip : {0.0, nds_period, nds_period + 0.002}) {
        capacity.record_tick(false, skip);
        check(capacity.should_render(0.045, 0.030, 0, nds_period),
            "unknown or slow skipped ticks must not receive video grace");
    }
    capacity.record_tick(false, 0.012);
    check(!capacity.should_render(0.045, 0.030, 0, nds_period),
        "a skipped tick faster than the period must retain video grace when late");

    // Before the first image, manual frame skip can suppress the punctual
    // requests. Events must not restart the initial deadline on every batch.
    for (unsigned manual_interval : {4u, 10u}) {
        FramePacer first_image;
        first_image.presented(10.0);
        first_image.reset();
        double now = 0.0;
        bool displayed = false;
        unsigned suppressed_requests = 0;
        for (unsigned tick = 1; tick <= manual_interval; ++tick) {
            const double started = now;
            first_image.start(now, nds_period);
            check(first_image.last_video == 0.0, "input moved the initial video deadline");
            check(first_image.can_tick(now, started, 0, nds_period), "first image stopped progressing");
            const bool requested = first_image.should_render(now, started, 0, nds_period);
            const bool composed = requested && tick % manual_interval == 0;
            if (requested && !composed) ++suppressed_requests;
            const double cost = composed ? 0.025 : 0.012;
            now += cost;
            first_image.record_tick(composed, cost);
            first_image.advance(nds_period);
            // One event ends each batch; present repeats the old image at 30 Hz.
            now = (std::floor((now + 0.002) * 30.0 + 1e-8) + 1.0) / 30.0;
            if (composed) {
                first_image.presented(now);
                displayed = true;
            }
        }
        check(suppressed_requests > 0 && displayed,
            "manual skip and continuous input starved the first eligible image");
    }

    // A mouse event ends every batch after one tick. Images obey elapsed
    // time rather than forcing an expensive composition every second tick.
    FramePacer interrupted;
    constexpr double period = nds_period;
    interrupted.start(0.0, period);
    double now = 2.0 * period;
    interrupted.record_tick(true, 0.025);
    interrupted.record_tick(false, 0.012);
    interrupted.presented(now);
    unsigned missed_video = 0, most_missed = 0;
    for (int batch = 0; batch < 120; ++batch) {
        const double started = now;
        interrupted.start(now, period);
        check(interrupted.can_tick(now, started, 0, period), "interrupted batch stopped progressing");
        const bool video = interrupted.should_render(now, started, 0, period);
        const double cost = video ? 0.025 : 0.012;
        now += cost;
        interrupted.record_tick(video, cost);
        interrupted.advance(period);
        now += 0.002; // presentation; process the incoming event before another tick
        check(now - interrupted.last_video <= FramePacer::max_video_interval + period + 0.004 + 1e-9,
            "continuous input exceeded the visual deadline and presentation allowance");
        if (video) interrupted.presented(now);
        missed_video = video ? 0 : missed_video + 1;
        most_missed = std::max(most_missed, missed_video);
    }
    check(most_missed >= 2, "continuous input forced unnecessary video before its deadline");
    interrupted.reset();
    check(interrupted.last_video < 0.0 && interrupted.render_cost == 0.0 && interrupted.skip_cost == 0.0,
        "reset retained old visual deadlines or cost samples");
    return failures ? 1 : 0;
}
