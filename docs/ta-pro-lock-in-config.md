# TA pro saturation-spectroscopy lock-in optimization policy

Configuration: [`../digilock-config.ta-pro-lock-in.json`](../digilock-config.ta-pro-lock-in.json).

**Status: proposed limits for manual review, not approved hardware limits.** No hardware was contacted to prepare this file. It uses allowlist mode, constrains every allowlisted write, and has **no startup writes**. Loading it does not alter the running experiment. Bounds are deliberately narrow commissioning/search budgets; they may need revision to reach your existing resonance or obtain adequate feedback.

This policy supports the existing spectroscopy apparatus, not laser-current, temperature, optical-power, or feedforward setup. It does not authorize deliberate oscillation tests or active response measurements. Review it with the [general safety checklist](safety-config-review.md) before enabling writes.

## Intended signal paths

| Function | Allowed input | Only allowed physical output |
| --- | --- | --- |
| Spectroscopy scan and its DC offset | Existing triangle scan | Scan Out → existing piezo input and current feedforward |
| Lock-in discriminator | Main In photodiode | Modulation on Main Out → TA pro Mod DC |
| PID 1, fast correction | LI out | Main Out → TA pro Mod DC |
| PID 2, slow correction | LI out | Scan Out → piezo, including existing feedforward |
| AutoLock | LI out; Main In as the spectroscopy trace | Only the selected digital PIDs; analog selection can only be cleared |

**Channel labels require verification.** The JSON uses `main in`, `main out`, `li out`, and `scan out`; the old application manual calls its piezo output **SC 110 out**. The RCI manual demonstrates lowercase `main in`, but does not enumerate all firmware-specific channel, waveform, or display labels. Before writes, use `digilock_range` and `digilock_help` for every enum, particularly `scan:output`, `offset:output`, `pid2:output`, `scan:signal type`, and `display:view`. Verify the physical connector, not just a similar name. If your firmware uses `sc 110 out` instead of `scan out`, replace the label consistently in all routing **and observation** rules only after confirming that it is the wired piezo channel. Do not permit both destinations as a shortcut. Update the routing regression test when deliberately changing a label.

Enum values are exact, including case and spaces. The offline validator verifies control names/types, **not** firmware enum membership. A mismatch is not permission to broaden the list to arbitrary outputs.

## Proposed numeric limits

All voltages are DigiLock-side values, **not** final piezo voltage, laser current, or optical frequency. Scan and LI amplitudes are **peak-to-peak**.

| Control | Permitted assignments | Rationale / review point |
| --- | --- | --- |
| `offset:value` | −0.5 to +0.5 V, Scan Out only | Small tuning region; does not permit Main Out DC-offset adjustment. An existing center outside this region must be reviewed, not automatically recentered. |
| `scan:amplitude` | 0 to 1 Vpp | Narrow piezo/feedforward exploration; intentionally much less than the old manual's approximately 10 Vpp example. |
| `scan_range` | −1 to +1 V | Fresh-state check of `offset ± amplitude/2` when either control is written. Not a final-output clamp. |
| `scan:frequency` | 1 to 20 Hz | Slow triangle spectroscopy scans; 10 Hz is the manual's example. Lower frequencies extend excursions in time; higher frequencies drive the piezo faster. |
| `li:modulation:amplitude` | 0 to 0.01 Vpp | At most ±5 mV centered modulation; 10 mVpp is the manual's example, not a TA pro safety specification. Start smaller if appropriate. |
| `li:modulation:frequency set` | 10 to 200 kHz | Includes the example near 100 kHz. Choose approximately one-tenth of the observed linewidth **in frequency units**; a time-domain trace alone does not supply that calibration. The firmware rounds to an available frequency: query `li:modulation:frequency act`. |
| `li:phase shift` | 0 to 360 degrees | Full phase search. Wrap negative phase adjustments modulo 360 and intersect with the live firmware range. Changing phase can reverse feedback. |
| `li:offset` | −0.1 to +0.1, scope-corresponding units | Small discriminator baseline adjustment. Verify scaling; this is not necessarily photodiode-terminal volts. |
| `main in:input offset` | −0.5 to +0.5 V | Limited input centering. This does not protect against excessive physical photodiode input voltage. |
| PID 1 relative minimum / maximum | −0.05 to 0 V / 0 to +0.05 V | At most ±50 mV fast correction relative to the initial output offset. |
| PID 2 relative minimum / maximum | −0.25 to 0 V / 0 to +0.25 V | At most ±250 mV slow correction relative to the initial output offset; include its current-feedforward effect. |
| Each PID overall / P / I / D gain | 0–10 / 0–100 / 0–10000 / 0–1 | Provisional bounded search ranges in **native RCI parameter units**. No universal physical normalization was established from these manuals. These caps alone do not prevent oscillation or saturation; review against current values, live ranges, error-signal slope, and actuator response. |
| PID 1 I cutoff | 10 to 1000 Hz | Must remain enabled; about 100 Hz is the manual's example for separating fast and slow integral action. |
| AutoLock and individual PID setpoints | Exactly zero | Lock to the zero crossing of an offset-corrected LI discriminator, not an arbitrary raw photodiode level. Automatic setpoint adjustment can only be disabled. |
| AutoLock cursor index | 1 to 1000 | Select a point in the captured scan. Select the intended peak/dip; another transition can still be a wrong lock. The wrapper does not enforce integer-valued numeric controls. |

PID min/max assignment ranges always bracket zero, so permitted assignments cannot invert the limit ordering **once both live limits satisfy the policy**. Both PID limit-enable controls accept only `true`. Note the catalog spelling `pid1:limit enable` (space) versus `pid2:limit:enable`.

### Combined-output budget: conditional, not enforced as an interlock

If the stated sources add as centered signals, their references and scaling are verified, and no other source contributes:

- **Main Out:** PID 1 correction ±50 mV plus LI modulation ±5 mV gives **±55 mV about the reviewed Main Out baseline**. With a baseline within ±20 mV, the nominal total is within ±75 mV. The ±20 mV baseline is a **manual precondition**, not a JSON-enforced limit: Main Out offset writes are intentionally unavailable.
- **Scan Out:** the reviewed scan envelope ±1 V plus PID 2 correction ±0.25 V gives a conservative **±1.25 V total budget** if both can be present. Do not assume they are mutually exclusive. When acquiring a lock, verify where the captured scan position becomes the controller's initial offset and budget correction around that reference.

These are budgeting estimates, **not proven final-output bounds**. The manual says PID limits are relative to the initial offset; it does not establish that they clip all summed generators, analog paths, external Sum In, or transient/reset behavior. If this is an SC 110 path, the old manual specifies an amplification factor of 15; verify your actual TA pro electronics instead of assuming this factor. Translate the scan/PID 2 budget through both piezo scaling and feedforward. Add its maximum current change to the current change from Main Out at the worst simultaneous extrema, with margin below the diode's permissible current and above its minimum operating current. Include laser base settings and any other modulation.

The JSON cannot clamp the laser diode current or enforce slew rates. Small voltage bounds are not sufficient when external conversion gains or current headroom are unknown.

## Discrete restrictions and excluded paths

- Scan output and offset selection are fixed to the same piezo channel; scan waveform is triangle only. LI and PID 1 cannot select the piezo, current-set backplane, temperature-set backplane, Aux Out, or AIO outputs. PID 2 cannot select Main Out.
- AutoLock is the only permitted lock-engagement/hold interface. Individual `pid1:lock:*` and `pid2:lock:*` writes are denied. AutoLock can only be enabled, not switched back to manual mode through this policy. PID selection can be changed for staged tuning; do so only while disengaged.
- PID polarity can take either boolean value because the actual actuator polarity cannot be determined from the model name. Determine it with the reviewed setup while disengaged; do not reverse sign or LI phase on a live lock. PID slope writes are denied: AutoLock supplies the slope for the chosen extremum.
- PID clipping and PID 1 I cutoff cannot be disabled. The Main In high/low-pass controls can only be bypassed, consistent with avoiding added phase lag; their orders/frequencies, Main In gain and inversion, PID 2 filters, and LI notch/waveform remain manually reviewed settings, not remotely tunable here.
- All AutoLock/PID relock-enable controls and PID window-reset-enable controls accept only `false`. Relock amplitude/frequency/output writes are denied. Existing supervision windows remain unchanged; do not interpret window thresholds as actuator clamps.
- All four PID turn-off-reset controls accept only `true`. The manual states that `false` transfers the current correction into the output offset, which can accumulate drift outside the direct offset-write limits on repeated lock/unlock cycles. `true` returns to the initial offset and can itself cause a voltage step: review the reset transient before permitting unlock.
- Automatic setpoint adjustment can only be disabled. AutoLock mode, strategy, smart-engage setting, active trace, and window thresholds are **not** writable. Configure the intended **LI/PDH: Lock to extremum** behavior and suitable loss-of-lock supervision manually before engagement. Do not assume the context-menu wording is an RCI enum value.
- Analog-controller activation, PDH settings, response/network-analyzer stimulus and start, simulation parameter transfer, external-output/DIO configuration, DMS/module selection/connection, process exit, and access-control writes are all denied. This does not turn off an already active source.
- Scope channels are restricted to spectroscopy, error, and the wired outputs. FFT channel 1 is restricted to `li out`. Averaging/smoothing can only be disabled, and FFT hold can only be released, so these writes cannot conceal oscillations in stale/smoothed traces. Review existing trace states too. Scope timescale and FFT span are configured manually because the bundled catalog does not list exact enum values.

Queries, range/help queries, graph/RMS/overload reads remain available without adding them to the write allowlist. This policy restricts **writes**, not access to all read commands. Graph acquisition may require selecting the corresponding view; `display:view` is limited to the proposed `scope`, `autolock`, and `spectrum` labels, which require live verification.

## Required live-state review before allowing activation

**A limits entry restricts future assignments, not existing settings.** In particular, enabling scan/modulation/AutoLock, selecting a PID, or releasing hold does not revalidate all stored values. No startup sequence silently sanitizes this running apparatus.

1. Identify the actual module, Windows host, DUI port, software/firmware, and wiring. The policy applies to **every accessible DUI port**, not just the default. Restrict network access to the reviewed module or review every reachable module; port overrides are not blocked by this JSON.
2. With writes disabled, inspect every allowed control's current value, `.range?`, and `.help?`. Confirm enum labels and units, scan/offset destination agreement, Main In and LI routes, both PID destinations, limits, and cutoff. Review all stored values that AutoLock may activate, including controllers not presently selected.
3. Establish a reviewed Main Out baseline, the safe absolute piezo/feedforward center, both enabled PID clamps, and a valid scan envelope. **Do not force the existing experiment to zero to fit this template.** If its center or gains are outside these budgets, revise and re-review the policy. Take account of safe intermediate states when setting two limits or reducing an existing excessive scan.
4. Verify analog feedback, PDH modulation, response injection, all relock paths, and window-triggered PID resets are inactive; verify external Sum In and other outputs/contributions. Verify turn-off-reset is `true` for both PIDs in both modes. Denied writes do not establish these states.
5. Configure AutoLock for the intended LI extremum, zero setpoint, appropriate trace/cursor behavior, and no automatic setpoint change. Verify its chosen controller set, error-signal polarity, supervision thresholds/channel, reset behavior, and all internal transitions. Use Main In to confirm the selected spectral feature and LI out to characterize the discriminator.
6. Verify photodiode input is within the documented electrical range and has no ADC overload; confirm gain/filter settings and both observation time/frequency spans. Input-offset adjustment cannot repair an electrically excessive photodiode signal.
7. Review bounds in physical units for **both** actuators and their combined current effect. Independently verify clipping references, source summation, lock-capture offsets, and reset transients. Only then approve activation writes.

Repeat the relevant checks after lock/unlock, controller selection, output-selector changes, local DUI actions, or another client's changes. Firmware-generated changes such as AutoLock capture and offset transfer do **not** pass through per-command JSON limits. `scan_range` runs only on `offset:value`/`scan:amplitude` assignments, not on `scan:enable`, routing changes, or lock engagement. It is not continuous monitoring. If these manual-state assumptions cannot be maintained, do not enable activation writes with this policy; a state-aware guard or independent hardware interlock is needed.

## Optimization workflow after approval

1. Preserve the working optics, laser base current/temperature/piezo settings, and feedforward. Read the existing scan rather than assuming the old manual's example amplitude is suitable. Use the reviewed triangle scan near 10 Hz to isolate the desired feature on Main In.
2. Set up LI input Main In and modulation Main Out with a small reviewed amplitude, no larger than 10 mVpp. Choose the modulation frequency from the resonance width and query its actual value. Manually optimize phase and LI offset to obtain a steep derivative with its zero crossing at the desired extremum. The manual's phase method is minimum signal, then minus 90 degrees, then a 180-degree correction if needed; wrap into the approved range. Automatic phase-adjust writes are intentionally not allowlisted.
3. With controllers disengaged, establish output limits, polarity, cutoff, zero setpoints, safe turn-off behavior, and disabled relock/reset paths. Acquire first with PID 2 and PID 1 gain zero, then introduce PID 1 with its I cutoff around 100 Hz. Use only the reviewed AutoLock extremum mode to engage the selected feature.
4. Tune conservatively within the gain budgets. Observe **LI out RMS, overload indicators, and FFT**; raw modulated Main In is not a reliable residual-noise metric because of aliasing. Also monitor controller/output excursions and clamp saturation: a small error signal or a clipped controller is not proof of a good lock. Do not make live routing, sign, phase, or input-filter changes.
5. Stop and re-review rather than increasing caps to overcome a failure. The manual suggests deliberately approaching oscillation during tuning; that is not authorized by this configuration and requires a separate hardware-risk decision. Record baseline/optimized settings, selected transition, error-signal slope, FFT spans and noise, and actuator headroom.

Useful read controls include `scope:graph`, `scope:ch1:rms`, `scope:ch1:overload`, `spectrum:graph`, `spectrum:ch1:rms`, `autolock:display:graph`, and `li:modulation:frequency act`. Use `digilock_query` with target `dui`; do not append `?` to that tool's `command` field.

## Loading and reviewing the policy

Start by loading it **without** `--allow-writes`. `digilock_info` exposes the loaded policy offline; loading with the empty startup list makes no hardware writes. Add `--config` and the absolute path to the existing MCP launch arguments:

```text
--config /workspaces/digilock-mcp/digilock-config.ta-pro-lock-in.json
```

Use the corresponding path if deployed elsewhere; retain the correct existing host/port settings. Add `--allow-writes` only after approval. The file is loaded once, so changes require a server restart. Keep `initial_values` empty unless a separate reviewed startup procedure is requested. Tests validate policy parsing and acceptance/rejection offline, not hardware safety or firmware enum membership.

## Documentation basis

Printed page numbers below are four less than the PDF viewer's page numbers:

- [`Digilock_Manual.pdf`](Digilock_Manual.pdf), §8.2.1.1–.2, p.18: scan Vpp and output-offset units.
- §8.2.1.3–.5, pp.19–25: AutoLock shared inputs/setpoints, activation, supervision, PID polarity/cutoff, and **relative** output limits.
- §8.2.1.7, p.27: LI routing, discrete frequencies, Vpp, phase, offset, automatic modulation activation, and using LI out for optimization.
- §8.2.5.1, p.32: reset versus transferring PID output into the channel offset at turn-off.
- §§9.2–9.6, pp.41–47: phase, gain/polarity tuning, relock/windows, and aliasing.
- §§10.2.1–10.2.2, pp.54–61: slow PID 2 plus fast PID 1, approximately 100 Hz I cutoff, top-of-fringe routing and 100 kHz / 0.01 Vpp examples.
- §11.3, p.69: DigiLock electrical capabilities and SC 110 amplification; these are **not TA pro safe operating limits**.
- [`DigiLock-RCI_Manual.pdf`](DigiLock-RCI_Manual.pdf), §§3.2.2–3.2.3, pp.8–9, and §4.2.2, pp.13–20: live range/help queries, exact data types, and control names; also cross-checked against [`commands.json`](commands.json).

The supplied documents do not establish your TA pro's Mod DC V-to-mA conversion, current/temperature/piezo operating point and headroom, feedforward gain, safe piezo travel/slew, or exact new-firmware Scan Out mapping. These remain explicit reviewer inputs.
