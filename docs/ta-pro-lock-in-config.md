# TA pro saturation-spectroscopy lock-in preset: permissions and manual review

Configuration: [`../digilock-config.ta-pro-lock-in.json`](../digilock-config.ta-pro-lock-in.json).

**Status: an unapproved operating preset, not safe hardware limits or a hardware interlock.** The root preset uses allowlist mode with **70 DUI commands, 25 numeric limit entries, no discrete `allowed_values` restrictions, and no startup writes**. The other 45 allowlisted commands have no operator-specific value constraints; documented boolean checks and firmware ranges still apply. These permissions and numeric ranges describe the file, not an authorization to exercise them on a connected laser. No hardware was contacted for this review.

The preset preserves the existing numeric ranges and permissions; the documented scan-enable spelling is `scan:enable`, not `scan:enabble`. This guide describes the corrected spelling. Check the actual loaded policy with `digilock_info` after deploying the correction. Loading the file with an empty `initial_values` list sends no startup assignments, but permitted tool calls can subsequently change the experiment when writes are enabled. Review it with the [general safety checklist](safety-config-review.md) before enabling writes.

## Intended signal paths — manually established, not enforced routing

The following is the intended apparatus arrangement, **not a set of routing restrictions implemented by this JSON**:

| Function | Intended input or waveform | Intended destination |
| --- | --- | --- |
| Spectroscopy scan and DC offset | Reviewed triangle scan | Scan Out → existing piezo input and current feedforward |
| Lock-in discriminator | Main In photodiode | Modulation on Main Out → TA pro Mod DC |
| PID 1, fast correction | LI out | Main Out → TA pro Mod DC |
| PID 2, slow correction | LI out | Scan Out → piezo, including existing feedforward |
| AutoLock | LI out; Main In as the spectroscopy trace | Manually reviewed digital-controller selection and operating mode |

The preset does **not** allow writes to `scan:output`, `offset:output`, `li:input`, `li:modulation:output`, `autolock:input`, or the PID input/output selectors. Denying selector writes does not select the intended route, establish its current value, or stop local DUI/software changes. In particular, `offset:value` applies to whatever output is currently selected; it is not restricted to the piezo or prohibited from affecting Main Out by this preset. AutoLock can also change selected controllers' input and setpoint internally.

Channel labels and physical connectors require verification. The old application manual calls its piezo output **SC 110 out**; do not assume a newer `scan out` label maps to the same connector or scaling. The JSON contains no channel-label restrictions. Use `digilock_query`, `digilock_range`, and `digilock_help` to inspect current selectors, firmware enum labels, units, and help before a separately approved manual setup. The RCI manual demonstrates `main in`, but does not enumerate all channel, waveform, or display labels. Live firmware acceptance is not evidence of physical safety.

## Actual numeric assignment limits

These are inclusive **operator assignment bounds**, additionally subject to firmware acceptance and, for offset/amplitude updates, the combined scan check. Voltages are DigiLock-side values, not final piezo voltage, laser current, or optical frequency. Scan and LI modulation amplitudes are **peak-to-peak**.

| Control | Root JSON bounds | Review point |
| --- | --- | --- |
| `scan:frequency` | 1 to 20 Hz | Waveform is not fixed to triangle by the policy; review waveform and actuator slew. |
| `scan:amplitude` | 0 to 40 Vpp | The combined scan rule further restricts an assignment using the live offset; 40 Vpp is not independently permitted or an electrical capability claim. |
| `offset:value` | 0 to 35 V | The combined scan rule further restricts an assignment using the live amplitude; destination is the currently selected output. |
| `main in:input offset` | 0 to 0.1 V | Does not protect against excessive physical photodiode input voltage. |
| `li:modulation:frequency set` | 1000 to 200000 Hz | Available frequencies are discrete; query `li:modulation:frequency act` after an approved change. |
| `li:modulation:amplitude` | 0 to 0.2 Vpp | Not included in `scan_range`; centered modulation at the maximum corresponds to ±0.1 V before external scaling. |
| `li:phase shift` | 0 to 360 degrees | Changing phase can reverse feedback. |
| `li:offset` | −1000 to +1000, scope-corresponding units | Verify discriminator scaling; not necessarily photodiode-terminal volts. |
| `autolock:setpoint` | −1 to +1 | Not restricted to zero; automatic setpoint adjustment is also writable. |
| `autolock:display:cursor index` | 1 to 1000 | Selecting another spectral feature can still give a wrong lock. |
| `pid1:gain`, `pid2:gain` | Each 0 to 1000 | Native RCI units, not physical-output bounds. |
| `pid1:proportional`, `pid2:proportional` | Each 0 to 10000 | Does not prevent oscillation or saturation. |
| `pid1:integral`, `pid2:integral` | Each 0 to 10000 | Review accumulated correction and controller state. |
| `pid1:differential`, `pid2:differential` | Each 0 to 10000 | Review noise amplification and actuator response. |
| `pid1:integral:cutoff:frequency` | 10 to 1000 Hz | Cutoff-enable writes are denied; the policy does not establish that cutoff is enabled. |
| `pid1:limit:min` / `pid1:limit:max` | −0.1 to 0 V / 0 to +0.1 V | Relative controller-output settings; limit enable is not established by the JSON. |
| `pid2:limit:min` / `pid2:limit:max` | −1 to 0 V / 0 to +1 V | Same caveat, including piezo/feedforward scaling. |
| `pid1:setpoint`, `pid2:setpoint` | Each −1 to +1 | Not restricted to zero; AutoLock can update shared setpoints internally. |

The grouped control rows above account for all **25 numeric limit entries**. The additional `scan_range` object is `{"min": -1, "max": 1}`; it is not another per-command limit entry. PID min/max assignment ranges bracket zero, so they remain ordered **once both live settings satisfy these ranges**. This does not establish enabled clamps, safe intermediate states, or bounded final outputs.

### Nominal scan check and conditional output budgeting

On an `offset:value` or `scan:amplitude` assignment, `scan_range` uses a fresh counterpart query and requires:

```text
-1 <= offset:value - scan:amplitude / 2
      offset:value + scan:amplitude / 2 <= 1
```

Thus the individual offset/amplitude maxima cannot be used independently. For example, at a live offset of 0 V, an amplitude assignment cannot exceed 2 Vpp; at a live amplitude of 0 V, an offset assignment cannot exceed 1 V. These are examples of the arithmetic check, **not recommended hardware settings**. The rule does not verify that offset and scan destinations agree, run on scan enable or lock engagement, or monitor subsequent changes. It does not include PID, LI/PDH, relock, analog, or external Sum In contributions.

Only after independently verifying the intended routes, enabled clamps, actual live settings, common references, source summation, and external scaling could the following be used as **conditional bookkeeping**, not enforced output bounds:

- Main Out: a PID 1 correction restricted to ±0.1 V plus centered LI modulation up to ±0.1 V would contribute up to **±0.2 V about a separately reviewed baseline**. The preset neither bounds that baseline continuously nor proves that these are the only sources.
- Scan Out: a verified nominal scan envelope within ±1 V plus additive PID 2 correction restricted to ±1 V would give an arithmetic **±2 V budget**, only if references align and the contributions can coexist as assumed. Lock capture can change the controller's initial reference; re-evaluate it instead of assuming the scan center remains unchanged.

The application manual describes PID limits relative to the initial output offset. It does not prove that they clip every summed generator, analog path, external Sum In contribution, or transient/reset mode. A disabled clamp, stored out-of-range parameter, different reference, or internally transferred offset invalidates these estimates. They may also exceed the electrical range of the selected output or the attached actuator's safe range. Do not rely on clipping as protection.

Verify actual TA pro electronics rather than assuming the old manual's SC 110 amplification factor of 15 applies. Translate piezo and feedforward excursions into physical units, and combine their current effect with Main Out at worst simultaneous extrema. Include laser base settings, current headroom, other modulation, polarity, and margin. This JSON cannot clamp diode current, enforce slew rates, or certify a voltage budget as safe.

## Activation, observation, and excluded writes

There are **no `allowed_values` entries**. The following permitted actions have no operator-specific discrete restriction; firmware and documented type checks are not hardware safety approval:

- `scan:signal type` can select any firmware-accepted waveform, not triangle only. `scan:enable` and `li:modulation:enable` can take either boolean value. Enabling a stored signal does not revalidate its amplitude, destination, or combined effect.
- `autolock:enable`, `autolock:controller:pid1`, `autolock:controller:pid2`, `autolock:lock:enable`, and `autolock:lock:hold` can enable/disable, select/deselect, engage/disengage, or hold/release as applicable. AutoLock is the permitted direct engagement/hold interface; individual PID lock/hold writes are denied. An enable or hold release does not validate all reachable controller states.
- `autolock:spectrum`, `autolock:spectrum:enable`, cursor track/snap, and `autolock:smart:setpoint` are writable without discrete restrictions. Automatic setpoint adjustment is **not forced off**, and the spectrum source is **not fixed to Main In**.
- PID sign writes accept either boolean value. LI phase, PID sign, gains, and setpoint changes can destabilize a live lock; software permission is not operational authorization.
- `display:view`, `display:sampling`, scope channels/show/averaging/smoothing/XY mode/timescale, and both spectrum channels/show/DC/hold/averaging/smoothing/frequency span are writable without operator value limits. Observation channels are **not fixed to LI out or the wired outputs**. Sampling can be disabled, traces hidden or frozen, and averaging/smoothing enabled; numeric averaging/smoothing counts also lack operator bounds. These actions can conceal oscillation or leave stale data, so independently review observation state.

Routing selectors, both PID limit-enable controls, PID 1 cutoff-enable, PID turn-off-reset controls, all relock controls, window/reset settings, Main In filters/gain/inversion, and PID 2 filters are **not allowlisted**. Their writes are denied, including purportedly protective `true` or `false` assignments. They remain manually established/live-state prerequisites, not JSON-enforced values. Note `pid1:limit enable` (space) versus `pid2:limit:enable`.

`autolock:controller:analog` and direct analog control writes are denied **for both boolean values**; the preset does not permit clearing analog selection. AutoLock may nevertheless activate an already selected analog controller. PDH settings, response/network-analyzer stimulus/start, simulation parameter transfer, external-output/DIO configuration, DMS/module connection, program exit, and access-control writes are also denied. Denial does not turn off existing sources or exclude indirect internal activation. AutoLock mode, strategy, smart-engage, active trace, and supervision settings require separate review.

Queries, range/help queries, and graph/RMS/overload reads do not need write-allowlist entries; ordinary catalog/protocol checks still apply. Graph queries require the appropriate active display and sampling. `display:view` has no preset label restriction; discover actual firmware values rather than assuming `scope`, `autolock`, or `spectrum` labels.

## Required live-state review before approving operations

**Assignment limits constrain future writes, not existing settings or internally generated changes.** No startup sequence sanitizes this apparatus. Review each intended operation, not just whether its control is allowlisted.

1. Identify the actual module, Windows host, DUI port, software/firmware, and wiring. The policy applies to every accessible DUI port, not just the default. Review every reachable module or independently restrict endpoint access.
2. With writes explicitly disabled, inspect every allowlisted control and every dependent route, clamp, filter, supervision, and reset setting using current-value, range, and help queries where supported. Confirm units and firmware labels. Review stored states AutoLock may activate, including unselected controllers.
3. Independently establish the intended scan/offset destination agreement, LI routing, both PID destinations, reviewed Main Out baseline, and piezo/feedforward center. Verify both relied-upon PID clamps and PID 1 cutoff are enabled. The preset cannot write their enable controls. Do not force the experiment to zero or treat the JSON maxima as safe targets. Review intermediate states when changing paired limits or reducing an excessive scan.
4. Verify any unbudgeted analog feedback, PDH modulation, response injection, relock, window-triggered resets, external Sum In, and other sources are inactive or independently covered. Check analog AutoLock selection manually. Review both PIDs' turn-off behavior in manual and AutoLock modes: checked reset returns to the initial offset and can cause a step; unchecked reset transfers correction into the channel offset and can accumulate drift beyond direct assignment limits. Neither behavior is enforced here.
5. Manually configure and verify the intended LI/PDH extremum-lock mode, controller selection, setpoint, polarity, supervision, and internal transitions. Zero setpoint and disabled automatic setpoint adjustment may be appropriate reviewed choices, but are **not policy requirements**. Verify spectroscopy trace and cursor identify the intended feature; phase/sign changes require a separate operation review.
6. Verify photodiode electrical range and ADC overload, gain/filter settings, observation routes and time/frequency spans, sampling, hold, averaging, and smoothing. Input-offset adjustment cannot repair an electrically excessive input; a smooth or small error trace is not proof of a stable lock.
7. Establish apparatus-specific physical limits for both actuators and their combined current effect. Verify source summation, clipping references, capture offsets, electrical capabilities, and transients. Record approved operations and narrower operational limits if needed; this guide does not invent them.

Repeat relevant checks after lock/unlock, controller selection, observation changes, local DUI actions, or other state changes. LI/PDH modulation can activate automatically during locking; captured offsets and offset transfer do not pass through per-command JSON limits. `scan_range` runs only on offset/amplitude assignments. If these assumptions cannot be maintained, do not approve activation under this preset; additional state-aware protection or an independent interlock is needed.

## Optimization workflow — only after separate apparatus approval

1. Preserve working optics, laser base settings, piezo center, and feedforward. Read the existing scan; use only a separately reviewed waveform, frequency, and amplitude, not the preset maxima or historical manual examples as targets.
2. Manually establish intended LI input and modulation output. Choose an approved modulation amplitude/frequency from the calibrated resonance width and query the actual frequency. Optimize phase and discriminator offset under the approved procedure. The manual's phase method is minimum signal, then minus 90 degrees, then a 180-degree correction if needed; use firmware-accepted values within the approved range. Automatic phase-adjust writes are not allowlisted.
3. While disengaged, verify reviewed output limits, clamp/cutoff enable, polarity, setpoints, and turn-off/relock/reset behavior. The manual's slow-PID-2/fast-PID-1 arrangement and approximately 100 Hz I cutoff are examples requiring apparatus review, not enforced settings or universal tuning instructions.
4. Tune only within separately approved operational limits. Observe LI out RMS, overload indicators, FFT, actuator excursions, and clamp saturation. Raw modulated Main In can mislead through aliasing. Verify observation settings remain live and informative; this preset allows changes that hide, smooth, or freeze traces.
5. Stop and re-review instead of interpreting software permission as authorization for deliberate oscillation tests or unsafe gain exploration. Record settings, selected transition, discriminator slope, noise measurements, spans, and actuator headroom. Active response measurements need separate authorization and are not allowlisted here.

Useful read controls include `scope:graph`, `scope:ch1:rms`, `scope:ch1:overload`, `spectrum:graph`, `spectrum:ch1:rms`, `autolock:display:graph`, and `li:modulation:frequency act`. Use `digilock_query` with target `dui`; do not append `?` to that tool's `command` field.

## Loading and reviewing the policy

For explicitly read-only review, **omit `--allow-writes` and set `DIGILOCK_ALLOW_WRITES=false` in the process environment**, including the MCP client's server environment. Merely omitting the flag or environment entry does not clear an inherited write-enabling value. `digilock_info` exposes the loaded policy offline. Add the config path to the existing launch arguments while retaining the reviewed host/port settings:

```text
--config /workspaces/digilock-mcp/digilock-config.ta-pro-lock-in.json
```

Use the corresponding deployed path. Enable writes only after approval of the actual preset and intended operations. The file is loaded once; edits require restart. Keep `initial_values` empty unless a separately reviewed startup procedure is requested. Protect launch arguments and the file from untrusted modification. Tests can validate parsing and policy behavior, not hardware safety or firmware enum membership.

## Documentation basis

For the following bundled manuals, printed page numbers are four less than PDF viewer page numbers:

- [`Digilock_Manual.pdf`](Digilock_Manual.pdf), §§8.2.1.1–.2, p.18: scan Vpp and output-offset units.
- §§8.2.1.3–.5, pp.19–25: AutoLock shared inputs/setpoints, activation, supervision, PID polarity/cutoff, and relative output limits.
- §8.2.1.7, p.27: LI routing, discrete frequencies, Vpp, phase, offset, automatic modulation activation, and LI out observation.
- §8.2.3, p.30: response measurements add an output stimulus with peak-to-peak amplitude.
- §8.2.5.1, p.32: reset versus transferring PID correction into the output-channel offset on turn-off.
- §§9.2–9.6, pp.41–47: phase, polarity/gain tuning, relock/windows, and aliasing.
- §§10.2.1–10.2.2, pp.54–61: slow/fast PID and spectroscopy examples; these are not apparatus safety limits.
- §11.3, p.69: DigiLock electrical capabilities and SC 110 amplification, not TA pro safe operating limits.
- [`DigiLock-RCI_Manual.pdf`](DigiLock-RCI_Manual.pdf), §§3.2.2–3.2.3, pp.8–9, and §4.2.2, pp.13–20: range/help, data types, and control names; cross-checked against [`commands.json`](commands.json).

The supplied documents do not establish this TA pro's Mod DC V-to-mA conversion, operating point/headroom, feedforward gain, safe piezo travel/slew, or exact new-firmware Scan Out mapping. These remain explicit reviewer inputs. Neither this guide nor the preset certifies a safe operating envelope.
