# S3 Audio backend: `pipewire` crate vs `cpal`

Date: 2026-11-18. Code: `spikes/s3-audio/`. Raw output of every run: `spikes/s3-audio/results/*.txt`
(first pass with weaker instrumentation: `results-v1/`).

Question: which backend makes shotgun a low-latency PipeWire node with zero user
configuration, and what latency is achievable on this machine?

Short answer: the `pipewire` crate (pipewire-rs 0.10.1) with a `pw_stream` in `RT_PROCESS`
mode. It was granted every quantum it asked for (32 to 1024 frames at 48 kHz), its callback
ran on a `SCHED_RR` thread without any configuration, and PipeWire reports exactly one
quantum of output delay (2.67 ms at 128). `cpal` 0.18.2 has a native PipeWire host now, but it
reports twice the delay, needs an extra feature for realtime priority, and hides the clock.
Details and caveats below; the test machine was shared with other workloads during the
measurements, which matters for how far the xrun numbers can be trusted.

## 1. Inventory of the machine

Commands: `pw-metadata -n settings`, `pw-top -b -n 2`, `wpctl status`, `pw-dump | jq ...`,
`ulimit -r`, `systemctl status rtkit-daemon`, `journalctl -u rtkit-daemon`,
`ps -eLo pid,tid,cls,rtprio,ni,comm`, `cat /proc/asound/card1/pcm0p/sub0/hw_params`.

| Item | Found |
|---|---|
| PipeWire daemon | 1.6.9 (`pipewire --version`), WirePlumber session manager, pipewire-pulse running |
| Dev-shell headers/libs | libpipewire 1.6.11, libspa 0.2 (`pkg-config --modversion`); the client library 1.6.11 talked to the 1.6.9 daemon without problems |
| Graph settings | `clock.rate` 48000, `clock.allowed-rates` [48000], `clock.quantum` 1024, `clock.min-quantum` 32, `clock.max-quantum` 2048, `clock.force-quantum` 0. Sink property `clock.quantum-limit` 8192 |
| Output devices | `alsa_output.pci-0000_0d_00.4.analog-stereo` (HDA analog, default, S32LE 2ch 48 kHz); `alsa_output.pci-0000_0b_00.1.hdmi-stereo-extra1` (HDMI, not tested) |
| Input devices | `alsa_input.pci-0000_0d_00.4.analog-stereo` (HDA analog, default); C920 webcam microphone (not tested) |
| Sink ALSA setup | `api.alsa.period-size` 1024, `api.alsa.period-num` 32, `api.alsa.headroom` 0; hw_params while running: period 1024, buffer 32768 (timer-based scheduling, the quantum is not the ALSA period) |
| Realtime | `RLIMIT_RTPRIO` = 0 and `RLIMIT_NICE` = 0 for the user session and for the pipewire service, so direct `sched_setscheduler` is not allowed. `rtkit-daemon` is active. `libpipewire-module-rt` is loaded in the daemon and (through the default `client.conf`) in every client. Daemon data loops run `SCHED_RR` 20 |
| How clients get RT | through RTKit: the journal shows one `Successfully made thread N of process M owned by '1000' RT at priority 20` line per engine start |
| Kernel | 7.2.15-zen1, `PREEMPT_DYNAMIC`, preempt mode `full` (`/sys/kernel/debug/sched/preempt`: `(full) lazy`) |
| JACK layer | Not installed for the user: no `pw-jack` on `PATH`, no `libjack.so` in `/run/current-system/sw/lib`. PipeWire's JACK library exists in the nix store (`/nix/store/7y28...-pipewire-1.6.9-jack/lib/libjack.so.0`) and works when put on `LD_LIBRARY_PATH` by hand |
| ALSA layer | pipewire-alsa plugin is configured system-wide (`/etc/alsa/conf.d/50-pipewire.conf`, `99-pipewire-default.conf`), so the ALSA `default` PCM is PipeWire |

## 2. What was built

Standalone Cargo project `spikes/s3-audio` with one shared measurement library and three
engines. Every engine renders a 440/441 Hz sine at -40 dBFS (gain 0.01); the control thread
pushes a frequency change through an `rtrb` SPSC queue every 250 ms. The callback does no
allocation, locking or logging. It does call `clock_gettime(CLOCK_MONOTONIC)` (vDSO) for the
measurements and `gettid()` once in the first callback (measurement aid only).

| Binary | Backend |
|---|---|
| `pw_engine` | `pipewire` crate, `pw_stream`, F32LE interleaved stereo, flags `AUTOCONNECT \| MAP_BUFFERS \| RT_PROCESS`, properties `node.latency = "<q>/48000"`, `node.rate = "1/48000"`, `media.role = "Production"`. `--capture` adds a second stream with direction Input. `--no-rt` drops `RT_PROCESS` |
| `pw_filter_engine` | `pw_filter` with two planar DSP output ports (and two input ports with `--duplex`), through raw `pipewire::sys` FFI because the crate has no filter wrapper |
| `cpal_engine` | cpal 0.18.2, host selectable: `pipewire`, `jack`, `alsa`. `BufferSize::Fixed(q)` (JACK: `BufferSize::Default` plus `PIPEWIRE_LATENCY=<q>/48000`, see failures) |
| `burn` | load generator: N busy-loop threads at normal priority |

Per run the library records: frames per callback, start-to-start interval of every callback
(deviation from the nominal period, percentiles after skipping the first 0.5 s), longest
callback duration, and timestamped xrun-like events:

- `late`: interval longer than 1.5 periods. A late wake-up is not necessarily a dropout.
- `position gap`: the driver's `spa_io_clock.position` advanced by more than the previous
  `duration` (a cycle was skipped for this node). PipeWire backends only.
- `recover flag`: `SPA_IO_CLOCK_FLAG_XRUN_RECOVER` seen. PipeWire backends only.
- driver `clock.xrun` delta: `spa_io_clock.xrun` ("estimated accumulated xrun duration", frames)
  at the last callback minus the first. PipeWire backends only.

PipeWire's own view is sampled by `run.sh`: `pw-top -b` runs for the whole run (ERR column of
our node and of the sink), `pw-dump` at t = 3 s (node `Latency` / `ProcessLatency` params),
`ps -L` for thread scheduling classes.

### Commands

```sh
# build (about 1 min 25 s clean, release)
nix develop --command bash -c 'cd spikes/s3-audio && source env.sh && cargo build --release'

# hosts and devices cpal sees
nix develop --command bash -c 'cd spikes/s3-audio && source env.sh && $CARGO_TARGET_DIR/release/cpal_engine list'

# one run: ./run.sh <tag> <secs> <binary> <binary args...>  -> results/<tag>.txt
nix develop --command spikes/s3-audio/run.sh pw-stream-128 30 pw_engine 128 30
nix develop --command spikes/s3-audio/run.sh cpal-pipewire-128 30 cpal_engine pipewire 128 30
S3_LINK=1    nix develop --command spikes/s3-audio/run.sh pw-filter-128 30 pw_filter_engine 128 30
S3_LOAD=burn nix develop --command spikes/s3-audio/run.sh load-pw-stream-128 30 pw_engine 128 30

# everything in the table below (38 runs, about 25 min), then the table itself
nix develop --command spikes/s3-audio/matrix.sh 30
spikes/s3-audio/summarize.sh 154     # argument: the sink's cumulative pw-top ERR before the first run
```

`env.sh` sets `CARGO_TARGET_DIR=/disk/shotgun-dev/target-s3`, adds a stub `pkgconfig/jack.pc`
(jack-sys' build script insists on finding `jack` via pkg-config) and puts PipeWire's
`libjack.so.0` on `LD_LIBRARY_PATH`. `flake.nix` was not changed. System PipeWire settings were
not changed; `pw-metadata -n settings` showed the same values after the last run as before the
first.

## 3. Measured numbers

All runs are 30 s, 48 kHz stereo float, default sink (HDA analog). Every backend was granted
the requested quantum in every run of this table (`frames/callback` and the `QUANT` column of
`pw-top` agree). Times in microseconds unless stated.

Important context for reading the table:

- **The machine was not quiet.** Other spikes were building and running on it at the same
  time. The 1-minute load average before each run is in the table; it was between 13 and 28
  from 13:08 on, before any load this spike added. "Idle" rows are therefore idle only as far
  as this spike is concerned.
- **Unexplained xrun bursts.** Three runs between 13:04 and 13:06 (`cpal-pipewire-64`,
  `cpal-alsa-64`, `pw-stream-128`) each contain one burst of 0.3 to 1 s in which the whole graph
  stalled every 10 to 20 ms (the sink's ERR counter jumped by 96, 62 and 10). The load average
  was low (2 to 5) at that time. The first pass had two more such bursts, both during
  `cpal-jack-256`, where a browser stream on the same sink counted xruns too. The bursts hit
  whichever backend happened to be running, were not reproduced by 16 busy-loop threads, and
  their cause was not identified. Repeats of the affected configurations (`pw-stream-128-r2`,
  `cpal-pipewire-64-r2`) did not show a burst.
- `late` events alone (no position gap, no ERR) mean the driver woke up late but no cycle was
  lost.
- Sink ERR is cumulative in `pw-top`; the per-run delta is computed against the previous run.

| run (start time) | loadavg 1m before | frames/callback min/max | period deviation p50 / p99 / max (us) | callback max (us) | own: late / pos gaps / recover flags (steady-state events) | driver clock.xrun delta (frames) | pw-top ERR delta node / sink | reported output latency | callback thread |
|---|---|---|---|---|---|---|---|---|---|
| pw-stream-64 (13:02:56) | 11.73 | 64/64 | 1.1 / 9.9 / 359.9 | 29.2 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 64 fr = 1.33 ms | data-loop.0 SCHED_RR 20 |
| pw-filter-64 (13:03:30) | 7.40 | 64/64 | 0.7 / 14.4 / 379.1 | 43.8 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 0 fr = 0.00 ms | data-loop.0 SCHED_RR 20 |
| cpal-pipewire-64 (13:04:04) | 4.77 | 64/64 | 1.0 / 32.0 / 10048.9 | 6568.3 | 61 / 0 / 0 (61) | n/a | 3 / 96 | cpal playback-callback 2.67 ms | pw_out SCHED_RR 10 |
| cpal-jack-64 (13:04:38) | 3.26 | 64/64 | 0.8 / 74.6 / 528.8 | 42.9 | 0 / 0 / 0 (0) | n/a | 0 / 0 | cpal playback-callback 1.33 ms | data-loop.0 SCHED_RR 20 |
| cpal-alsa-64 (13:05:12) | 3.22 | 64/64 | 1.3 / 25.7 / 12072.7 | 35.0 | 50 / 0 / 0 (0) | n/a | 0 / 62 | cpal playback-callback 3.98 ms | cpal_alsa_out SCHED_OTHER 0 |
| pw-stream-128 (13:05:46) | 2.41 | 128/128 | 1.5 / 33.2 / 297621.5 | 7160.9 | 19 / 14 / 6 (48) | 1152 | 20 / 10 | pw_time.delay 128 fr = 2.67 ms | data-loop.0 SCHED_RR 20 |
| pw-filter-128 (13:06:20) | 2.03 | 128/128 | 0.8 / 16.9 / 186.1 | 44.3 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 0 fr = 0.00 ms | data-loop.0 SCHED_RR 20 |
| cpal-pipewire-128 (13:06:54) | 1.58 | 128/128 | 0.6 / 22.1 / 574.2 | 31.3 | 0 / 0 / 0 (0) | n/a | 0 / 0 | cpal playback-callback 5.33 ms | pw_out SCHED_RR 10 |
| cpal-jack-128 (13:07:28) | 1.51 | 128/128 | 3.5 / 110.7 / 319.4 | 40.4 | 0 / 0 / 0 (0) | n/a | 0 / 0 | cpal playback-callback 2.48 ms | data-loop.0 SCHED_RR 20 |
| cpal-alsa-128 (13:08:03) | 3.42 | 128/128 | 7.7 / 1639.1 / 4196.4 | 35.6 | 157 / 0 / 0 (140) | n/a | 0 / 1 | cpal playback-callback 7.94 ms | cpal_alsa_out SCHED_OTHER 0 |
| pw-stream-256 (13:08:37) | 13.39 | 256/256 | 6.6 / 55.8 / 99.6 | 37.1 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 256 fr = 5.33 ms | data-loop.0 SCHED_RR 20 |
| pw-filter-256 (13:09:11) | 21.45 | 256/256 | 6.2 / 53.3 / 93.1 | 30.5 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 0 fr = 0.00 ms | data-loop.0 SCHED_RR 20 |
| cpal-pipewire-256 (13:09:45) | 27.73 | 256/256 | 7.3 / 77.3 / 428.5 | 37.0 | 1 / 0 / 0 (0) | n/a | 0 / 0 | cpal playback-callback 10.67 ms | pw_out SCHED_RR 10 |
| cpal-jack-256 (13:10:19) | 25.69 | 256/256 | 23.4 / 192.8 / 5296.4 | 61.2 | 4 / 0 / 0 (2) | n/a | 0 / 1 | cpal playback-callback 5.19 ms | data-loop.0 SCHED_RR 20 |
| cpal-alsa-256 (13:10:54) | 23.45 | 256/256 | 51.9 / 3601.4 / 14143.3 | 2462.4 | 47 / 0 / 0 (43) | n/a | 0 / 0 | cpal playback-callback 14.44 ms | cpal_alsa_out SCHED_OTHER 0 |
| pw-stream-1024 (13:11:28) | 22.27 | 1024/1024 | 25.1 / 192.3 / 3024.6 | 109.2 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 1024 fr = 21.33 ms | data-loop.0 SCHED_RR 20 |
| pw-filter-1024 (13:12:03) | 23.23 | 1024/1024 | 28.4 / 193.6 / 243.0 | 52.5 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 0 fr = 0.00 ms | data-loop.0 SCHED_RR 20 |
| cpal-pipewire-1024 (13:12:37) | 23.53 | 1024/1024 | 24.5 / 215.1 / 697.5 | 36.9 | 0 / 0 / 0 (0) | n/a | 0 / 0 | cpal playback-callback 42.67 ms | pw_out SCHED_RR 10 |
| cpal-jack-1024 (13:13:11) | 23.92 | 1024/1024 | 11.0 / 130.5 / 240.9 | 76.9 | 0 / 0 / 0 (0) | n/a | 0 / 0 | cpal playback-callback 21.27 ms | data-loop.0 SCHED_RR 20 |
| cpal-alsa-1024 (13:13:46) | 23.55 | 1024/1024 | 24.2 / 2542.4 / 4788.5 | 35.7 | 0 / 0 / 0 (0) | n/a | 0 / 0 | cpal playback-callback 63.42 ms | cpal_alsa_out SCHED_OTHER 0 |
| pw-stream-32 (13:14:20) | 23.40 | 32/32 | 2.0 / 54.1 / 384.2 | 65.1 | 2 / 0 / 0 (4) | 96 | 0 / 3 | pw_time.delay 32 fr = 0.67 ms | data-loop.0 SCHED_RR 20 |
| pw-stream-128-nort (13:14:54) | 22.81 | 128/128 | 51.2 / 2216.2 / 16099.1 | 6267.7 | 351 / 83 / 0 (354) | 0 | 217 / 0 | pw_time.delay 256 fr = 5.33 ms | pw_engine SCHED_OTHER 0 |
| pw-stream-128-capture (13:15:29) | 24.52 | 128/128 | 9.1 / 109.8 / 681.8 | 137.7 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 128 fr = 2.67 ms | data-loop.0 SCHED_RR 20 |
| pw-filter-128-duplex (13:16:03) | 25.29 | 128/128 | 14.6 / 208.3 / 923.8 | 70.3 | 1 / 1 / 0 (0) | n/a | 0 / 0 | pw_time.delay 0 fr = 0.00 ms | data-loop.0 SCHED_RR 20 |
| cpal-pipewire-128-capture (13:16:37) | 18.82 | 128/128 | 23.9 / 245.0 / 1497.5 | 95.5 | 1 / 0 / 0 (1) | n/a | 0 / 0 | cpal playback-callback 5.33 ms | pw_out SCHED_RR 10 |
| load-pw-stream-64 (13:17:11) | 20.75 | 64/64 | 2.3 / 44.2 / 663.8 | 40.7 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 64 fr = 1.33 ms | data-loop.0 SCHED_RR 20 |
| load-pw-stream-128 (13:17:48) | 24.19 | 128/128 | 4.0 / 57.9 / 724.5 | 56.3 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 128 fr = 2.67 ms | data-loop.0 SCHED_RR 20 |
| load-pw-stream-256 (13:18:24) | 26.05 | 256/256 | 10.0 / 98.7 / 232.0 | 47.6 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 256 fr = 5.33 ms | data-loop.0 SCHED_RR 20 |
| load-pw-filter-128 (13:19:00) | 25.97 | 128/128 | 2.4 / 44.5 / 909.0 | 37.4 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 0 fr = 0.00 ms | data-loop.0 SCHED_RR 20 |
| load-cpal-pipewire-128 (13:19:36) | 24.95 | 128/128 | 1.2 / 88.2 / 258.3 | 373.9 | 0 / 0 / 0 (0) | n/a | 0 / 0 | cpal playback-callback 5.33 ms | pw_out SCHED_RR 10 |
| load-cpal-jack-128 (13:20:12) | 24.31 | 128/128 | 1.2 / 85.2 / 445.1 | 24.4 | 0 / 0 / 0 (0) | n/a | 0 / 0 | cpal playback-callback 2.67 ms | data-loop.0 SCHED_RR 20 |
| load-cpal-alsa-128 (13:20:49) | 25.33 | 128/128 | 1.4 / 965.2 / 2646.7 | 207.7 | 10 / 0 / 0 (9) | n/a | 0 / 0 | cpal playback-callback 7.98 ms | cpal_alsa_out SCHED_OTHER 0 |
| cargoload-pw-stream-128 (13:21:25) | 26.12 | 128/128 | 20.6 / 195.8 / 5006.3 | 143.7 | 3 / 2 / 0 (6) | 129 | 4 / 1 | pw_time.delay 128 fr = 2.67 ms | data-loop.0 SCHED_RR 20 |
| pw-stream-128-r2 (13:22:37) | 17.54 | 128/128 | 17.4 / 192.8 / 1818.6 | 87.3 | 1 / 0 / 0 (1) | 0 | 0 / 0 | pw_time.delay 128 fr = 2.67 ms | data-loop.0 SCHED_RR 20 |
| cpal-pipewire-64-r2 (13:23:11) | 16.30 | 64/64 | 8.1 / 144.7 / 1390.2 | 75.9 | 5 / 0 / 0 (5) | n/a | 0 / 2 | cpal playback-callback 2.67 ms | pw_out SCHED_RR 10 |
| cargoload-pw-stream-256 (13:23:45) | 15.93 | 256/256 | 30.3 / 253.9 / 491.7 | 65.2 | 0 / 0 / 0 (0) | 0 | 0 / 0 | pw_time.delay 256 fr = 5.33 ms | data-loop.0 SCHED_RR 20 |
| cargoload-pw-stream-128-r2 (13:24:21) | 17.67 | 128/128 | 25.5 / 221.7 / 2031.3 | 139.1 | 1 / 0 / 0 (1) | 0 | 0 / 0 | pw_time.delay 128 fr = 2.67 ms | data-loop.0 SCHED_RR 20 |
| cargoload-pw-stream-64 (13:24:58) | 22.22 | 64/64 | 9.2 / 145.5 / 992.8 | 189.0 | 2 / 0 / 0 (1) | 65 | 0 / 1 | pw_time.delay 64 fr = 1.33 ms | data-loop.0 SCHED_RR 20 |

Row groups: `pw-stream` = `pipewire` crate stream; `pw-filter` = raw-FFI filter; `cpal-*` = cpal
host; `-nort` = stream without `RT_PROCESS`; `load-*` = 16 busy-loop threads on the 16 cores
(`burn 16`); `cargoload-*` = a parallel `cargo build --release -j 16` of this spike into a
scratch target directory (still running at engine exit in all four runs).

### Reading the numbers

Quantum and latency PipeWire reports (`pw_stream_get_time_n().delay`, or cpal's
`playback - callback` timestamp difference):

| Requested quantum | `pipewire` stream | cpal PipeWire host | cpal JACK host | cpal ALSA host (pipewire-alsa) |
|---|---|---|---|---|
| 32 | 32 frames = 0.67 ms | not run | not run | not run |
| 64 | 64 = 1.33 ms | 2.67 ms | 1.33 ms | 3.98 ms |
| 128 | 128 = 2.67 ms | 5.33 ms | 2.48 ms | 7.94 ms |
| 256 | 256 = 5.33 ms | 10.67 ms | 5.19 ms | 14.44 ms |
| 1024 | 1024 = 21.33 ms | 42.67 ms | 21.27 ms | 63.42 ms |

- The stream's delay is exactly one quantum at every size; `buffered` and `queued` were 0.
  The sink's `Latency` param is `minQuantum = maxQuantum = 1.0` with zero rate/ns parts and its
  `ProcessLatency` is all zeros: the HDA codec/DAC delay is **not** reported by the device, so
  the real acoustic delay is larger by an unknown amount (unverified, no loopback measurement).
- The driver's own `spa_io_clock.delay` (measured device fill) stayed just under one quantum
  (e.g. 123 to 125 frames at 128).
- The cpal PipeWire host reports two quanta. `pw-top` marks its node with `=` (asynchronously
  scheduled) where the `RT_PROCESS` stream shows `+`; `pw_engine --no-rt` shows the same `=`
  and the same doubled delay (256 frames at quantum 128).
- The filter node's `pw_filter_get_time()` returned `delay = 0` at every quantum; the filter
  does not compute the path latency for us.

Callback timing of the `pipewire` stream (runs without lost cycles): period deviation p50 1 to
30 us, p99 10 to 250 us; callback duration 1 to 18 us on average (it grows with the block
size), worst case 30 to 190 us. With 16 busy-loop
threads the p99 stayed below 100 us at 64, 128 and 256 and there were no xruns of any kind.
The filter node performs the same (same data-loop thread, same numbers within noise).

Xruns of the `pipewire` stream/filter over all runs of the table:

| Quantum | Runs | Runs with a lost cycle (position gap, `clock.xrun` or ERR above 0) |
|---|---|---|
| 32 | 1 | 1 (3 driver xruns of 32 frames in 30 s) |
| 64 | 4 | 1 (`cargoload`: one 65-frame xrun 2 s after start) |
| 128 | 9 | 2 (`pw-stream-128`: the unexplained burst; `cargoload-pw-stream-128`: one 129-frame xrun; the repeat of each was clean). The duplex run logged one position jump 25 ms after start, when the driver changed from sink to source; not counted |
| 256 | 4 | 0 |
| 1024 | 2 | 0 |

This is too few runs on a shared machine to give a rate. What it supports: 64 and 128 are
usable and survive pure CPU saturation; a compile workload produced an occasional single xrun
at 64 and 128 and none at 256.

Realtime priority of the callback thread:

| Backend | Thread | Scheduling |
|---|---|---|
| `pipewire` stream/filter with `RT_PROCESS` | `data-loop.0` | `SCHED_RR` 20, obtained by libpipewire's `module-rt` through RTKit, no code in the app |
| `pipewire` stream without `RT_PROCESS` | main loop thread | `SCHED_OTHER`; 351 late callbacks and 217 node xruns in 30 s on the loaded machine |
| cpal PipeWire host, feature `realtime-dbus` | `pw_out` | `SCHED_RR` 10 (cpal asks RTKit itself through `audio_thread_priority` + libdbus) |
| cpal PipeWire host, feature `realtime` without dbus | `pw_out` | `SCHED_OTHER`. One 7 s trial at quantum 128: 82 late callbacks, period deviation p99 2.2 ms, max 18.9 ms (result file not kept) |
| cpal JACK host | `data-loop.0` | `SCHED_RR` 20 (PipeWire's libjack); cpal additionally reports `RealtimeDenied` because its own promotion attempt fails, harmless |
| cpal ALSA host | `cpal_alsa_out` | `SCHED_OTHER` always: cpal only promotes threads of hardware PCMs, the PipeWire plugin PCM is not eligible. Period deviation p99 1 to 3.6 ms, dozens of late callbacks per run |

## 4. Clock and transport for A/V sync

API used (all verified in `pw_engine`):

1. In the `process` callback call `stream.time()` (`pipewire::stream::Stream::time`, wraps
   `pw_stream_get_time_n`, needs crate feature `v0_3_50` or later). Fields of the returned
   `pw_time`:
   - `now()`: `CLOCK_MONOTONIC` nanoseconds of the start of this graph cycle. It was identical
     to `spa_io_clock.nsec` in every snapshot, and 11 to 270 us before our callback entry.
   - `rate()`: fraction, here 1/48000; unit of `ticks` and `delay`.
   - `ticks()`: the graph's sample counter for this stream.
   - `delay()`: frames until the next sample we write is presented by the device.
   - `buffered()`: frames held in the stream's resampler (0 here, same rate). `queued()`: 0 in
     `RT_PROCESS` mode.
2. Keep the engine's own frame counter `N` (frames rendered before this callback). It is the
   master clock of the spec. Measured: `ticks - N` was constant over every clean 30 s run
   (e.g. 128 in all 28 snapshots at quantum 128), so no cycle was dropped or duplicated.
3. The first frame rendered in this callback (index `N`) is heard at
   `now + (delay + buffered) / rate`. Publish `(N, now, delay + buffered)` from the callback
   through a lock-free slot. Any other thread computes the frame being heard at monotonic time
   `T` as `N + (T - now) * 48000 / 1e9 - (delay + buffered)`.
4. Optional rate correction: `spa_io_position.clock.rate_diff` (pointer delivered in the
   stream's `io_changed` callback with id `SPA_IO_Position`). Measured effective rate against
   `CLOCK_MONOTONIC` over a run: 48000.03 to 48001.65 Hz. Extrapolating linearly with one
   rate over a whole clean 30 s run was off by at most 0.2 to 11.2 frames (under 0.25 ms);
   over the few milliseconds between two callbacks the correction is irrelevant.
5. Discontinuities: a jump of `clock.position` beyond the previous `clock.duration`, the
   `SPA_IO_CLOCK_FLAG_XRUN_RECOVER` flag (value 2 in `clock.flags`) and growth of `clock.xrun`
   were all observed in the xrun runs and are usable as xrun counters for the performance
   overlay. `pw-top`'s ERR column for our node is the external equivalent.

Also available but not used: `pw_stream_get_nsec()` exists in `pipewire::sys` but has no safe
wrapper (marked TODO in the crate); `clock_gettime(CLOCK_MONOTONIC)` is the same clock.

Not verified: that `now + delay` matches what is acoustically heard. The device reports no
internal latency (see above), so the product needs a user-adjustable A/V offset, and other
sinks (HDMI, USB, Bluetooth) were not tested and will report different delays.

With cpal the only clock is `OutputCallbackInfo::timestamp()` (`callback` and `playback`
instants). The cpal PipeWire host derives `playback` from the same `pw_time.delay`; there is no
access to ticks, position, quantum changes or xrun flags.

## 5. Capture

- `pw_engine 128 30 --capture`: a second stream with `Direction::Input` and the same
  `node.latency` opened at quantum 128 (128 frames per callback, `SCHED_RR` 20, 0 events).
  Reported capture delay `pw_time.delay` = 160 frames = 3.33 ms. Reported round trip =
  128 + 160 = 288 frames = **6.0 ms** at quantum 128 (sum of what PipeWire reports; codec
  latency not included; no loopback measurement). The two streams were scheduled in two
  separate driver groups (sink drives playback, source drives capture).
- `pw_filter_engine 128 30 --duplex`: one node with two input and two output ports, linked by
  hand to the default source and sink, received input buffers in the same cycle (input peak
  0.042, steady state 0 events). The source became the driver and the sink followed it. The
  same test in the first pass had 26 late callbacks and 37 position gaps while the sink's ERR
  rose from 21 to 93; whether that was the duplex topology or one of the unexplained bursts is
  not known.
- `cpal_engine pipewire 128 30 --capture`: separate cpal input stream, 128 frames, reported
  capture latency 3.33 ms, playback 5.33 ms.

## 6. Does the `pipewire` crate build and what is missing

Builds cleanly against the PipeWire 1.6.11 headers of the dev shell and runs against the
1.6.9 daemon. Versions from `Cargo.lock`: pipewire 0.10.1, pipewire-sys 0.10.1, libspa 0.10.1,
libspa-sys 0.10.1, bindgen 0.72.1; cpal 0.18.2 (which itself depends on pipewire 0.10), jack
0.13.5, jack-sys 0.5.1, alsa 0.11.0, audio_thread_priority 0.35.1, dbus 0.6.5, libdbus-sys
0.2.7, rtrb 0.4.1. Declared MSRV: pipewire 1.80, cpal 1.85.

Build needs: `pkg-config` finding `libpipewire-0.3` and `libspa-0.2`, and libclang for bindgen
(`LIBCLANG_PATH`, already set by the flake). Bindings are generated at build time from the
installed headers. Highest version feature flag is `v1_2_0` for `pipewire` (`v1_6_0` for
`libspa`); the spike used `features = ["v1_2_0"]`.

Gaps that force `unsafe` or raw FFI (found by reading the crate source and by writing the
engines):

| Gap | Consequence |
|---|---|
| No wrapper for `pw_filter` at all | Filter nodes are written against `pipewire::sys` (29 `pw_filter_*` functions are bound). In `pw_filter_engine.rs` the port setup and the whole process callback are `unsafe` |
| `io_changed` hands out `*mut c_void` | `spa_io_position` must be cast and dereferenced by hand to read quantum, position, xrun data |
| Buffer data is `&mut [u8]` | cast to `&mut [f32]` by hand |
| `pw_stream_get_nsec` not wrapped | use `clock_gettime` |
| Latency params | `param_changed` gives a raw `Pod`; `spa_latency_parse` is bound in `libspa-sys` but has no safe wrapper. Not exercised in this spike |
| Format negotiation | `connect()` takes raw pods built with `PodSerializer`; marked FIXME in the crate |
| Loop objects are `Rc`-based and not `Send`; `ThreadLoop::new` is `unsafe` | run the PipeWire main loop on a dedicated thread and talk to it through `pipewire::channel` (uses a mutex, fine outside the RT thread) and lock-free queues |

None of these blocked the stream engine: `pw_engine.rs` has three small `unsafe` blocks.

## 7. Plugin hosting, on paper only (nothing built)

Crates that exist on crates.io today (`cargo info`, 2026-11-18):

| Format | Crate | Notes |
|---|---|---|
| CLAP | `clack-host` 0.2.0, `clack-extensions` 0.2.0 (MIT/Apache-2.0, MSRV 1.85) | safe host-side wrappers; `clap-sys` 0.5.0 for raw bindings |
| VST3 | `vst3` 0.3.0 (coupler-rs, MIT/Apache-2.0) | raw COM bindings only, the host logic would be ours |
| LV2 | `livi` 0.9.0 (MIT), on `lilv` 0.3.1 / `lilv-sys` 0.3.0 | needs the C `lilv` library. `lv2` 0.6.0 is for writing plugins, not hosting |
| several | `rack` 0.4.8 | describes itself as a plugin hosting library; not evaluated |

What hosting needs from the backend choice (not verified by code):

- Plugins are activated with a sample rate and a maximum block size and then processed on the
  realtime thread with planar float buffers. PipeWire can change the quantum at runtime
  (observed: another client asking for 512 forced a "1024" cpal stream down to 512 mid-run), so
  the engine should activate processors with the graph's `clock.quantum-limit` (8192 here) as
  `max_block` and never assume a fixed block. That is independent of stream vs filter.
- The `pw_stream` callback already runs on the thread PipeWire made realtime, so hosted
  plugins inherit it. Worker threads for a parallel graph would have to be promoted by us
  (RTKit); not tested.
- An audio `pw_stream` carries one interleaved audio format. MIDI/event ports, sidechain
  inputs as separate ports and per-port latency reporting are what `pw_filter` is for. Moving
  to a filter node later costs: raw FFI, creating our own links (WirePlumber did not link the
  filter), and reading latency from port params ourselves.
- cpal would hide the node entirely (no extra ports, no properties, no latency params), which
  is a second reason not to build the DAW on it.

## 8. What failed or surprised

- **`stress` is not the CPU stressor here.** The `stress` on this machine's `PATH` is Go's
  test-stress tool and exits immediately. The first set of "load" runs therefore had no load
  and was discarded; the spike carries its own `burn` binary.
- **`pw-top` ERR needs care.** It is cumulative per node, shows 0 while a node is suspended and
  the first batch iteration is empty. The first pass computed wrong sink deltas; the final
  table derives them from consecutive runs.
- **WirePlumber does not auto-link a `pw_filter` node.** With `media.class =
  Stream/Output/Audio` and `node.autoconnect = true` the filter stayed unlinked and never got
  a callback; the journal shows `m-si-audio-adapter: no usable format found for node 80`. The
  runs in the table link it with `pw-link`.
- **cpal JACK host rejects `BufferSize::Fixed(128)`** ("does not match the server buffer size
  1024"). It only works with `BufferSize::Default` plus the environment variable
  `PIPEWIRE_LATENCY`, needs `jack.pc` at build time and `libjack.so.0` at run time, neither of
  which this system provides without manual paths. Not zero-configuration.
- **cpal `realtime-dbus` needs libdbus headers**, which the dev shell does not have; the spike
  builds `libdbus-sys` with its `vendored` feature instead of changing the flake.
- **Other clients change the quantum.** `node.latency` is a request; the graph runs at the
  smallest request. In the first pass an audio visualiser asking for 512 turned two "1024"
  runs into 512-frame runs.
- **Unexplained xrun bursts** on the shared machine, see section 3.
- Not done: acoustic or loopback latency measurement; sinks other than the HDA analog output;
  sample rates other than 48 kHz; device hot-switch while streaming; pausing the stream when
  the transport stops; behaviour without RTKit; Flatpak/portal realtime path.

## Decision

**Backend:** native PipeWire client with the `pipewire` crate 0.10.1 (`libspa` 0.10.1,
features `v1_2_0`), a single `pw_stream` per direction, flags
`AUTOCONNECT | MAP_BUFFERS | RT_PROCESS`, format F32LE interleaved at 48 kHz, behind the
engine's backend trait. Lock-free queues with `rtrb` 0.4.1. Not cpal: its PipeWire host
reports and schedules two quanta instead of one, needs `realtime-dbus` (libdbus) to get
realtime priority, and exposes no sample position, quantum, xrun or node properties; its ALSA
host never gets realtime priority through the PipeWire plugin; its JACK host needs a JACK
library this system does not install. `pw_filter` is the upgrade path for the DAW stage
(separate ports, MIDI, same-cycle duplex) and performs identically, but costs raw FFI, manual
linking and manual latency handling, so it is not the stage 1 choice.

**Default quantum:** request `node.latency = 256/48000` for the editor (5.33 ms reported
output delay), configurable from 64 to 1024. Reason: 256 had no lost cycle in any of its runs
including the build-load run, while 128 and 64 each lost a single cycle under build load and
128 was hit by one burst. Playback sync does not depend on the quantum because the delay is
compensated. 128 (2.67 ms) and 64 (1.33 ms) worked and are the settings to offer for
monitoring and instruments in the DAW stage. This differs from the 128 target in
`docs/spec/06-audio.md`; the evidence for preferring 256 is a handful of runs on a busy
machine, so treat it as a conservative default, not a proven limit. Whatever is requested, the
engine must take the block size from each callback (`pw_buffer.requested` /
`spa_io_clock.duration`) because other clients can lower the graph quantum.

**Realtime scheduling on NixOS:** nothing to configure in the app. With `RT_PROCESS`
libpipewire's `module-rt` asked RTKit and the callback thread ran `SCHED_RR` 20 in every run,
although the user's `RLIMIT_RTPRIO` is 0. The one system requirement is a running RTKit
daemon (`security.rtkit.enable = true;` in the NixOS configuration, the setting the NixOS
PipeWire module documentation asks for; it is active on this machine, whether through that
option was not checked). Without RTKit and with `RLIMIT_RTPRIO` 0 the thread would stay
`SCHED_OTHER`; the `--no-rt` run shows what that costs. The performance overlay should show
the callback thread's policy so a missing RTKit is visible. The packaged app must find
`libpipewire-0.3.so` at run time (the dev shell does this through `LD_LIBRARY_PATH`).

**Clock/latency API for A/V sync:** engine frame counter as master clock; in every callback
`Stream::time()` gives `now` (`CLOCK_MONOTONIC` ns), `delay`, `buffered`, `rate`; frame heard
at time `T` = `N + (T - now) * rate - (delay + buffered)`. Quantum, position and xrun
detection from `spa_io_position.clock` (`duration`, `position`, `flags`, `xrun`) obtained in
`io_changed`. Add a user-adjustable offset because the device reports no internal latency.

**Gotchas:**

- Always set `RT_PROCESS`; without it the node is scheduled asynchronously (one extra quantum)
  on a non-realtime thread.
- `node.latency` is a request. Handle any block size up to `clock.quantum-limit`.
- While the stream is active the whole desktop graph runs at our quantum. Pause or deactivate
  the stream when the transport is stopped (not tested here).
- The reported delay is one quantum and excludes the codec; do not present it as measured
  round-trip latency.
- `pw-top` ERR is cumulative and reads 0 for suspended nodes; use `spa_io_clock` for in-app
  counters.
- PipeWire objects are not `Send`; one dedicated PipeWire thread, lock-free queues to the
  rest of the app.
- The client library in the dev shell (1.6.11) is newer than the daemon (1.6.9); that worked,
  other combinations are untested.
- Latency numbers and xrun behaviour are for one HDA analog output on one machine that was
  running other workloads; repeat `matrix.sh` on a quiet system before tightening the default.
