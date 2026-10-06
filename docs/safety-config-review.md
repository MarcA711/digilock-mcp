# Configuration safety review: combined effects and limit bypass paths

Use this checklist after every change to the operator JSON configuration, MCP launch arguments, wrapper version, DigiLock software/firmware, output routing, or physical wiring.

**This is a manual review checklist, not an automated safety validator or certification.** Valid JSON, successful startup, and firmware-accepted parameter ranges do not prove that an attached laser is safe. A rule marked as unverified must not be treated as protection.

## Scope and evidence

The command names and descriptions below come from [the bundled RCI catalog](commands.json). The current enforcement is implemented in [`src/policy.rs`](../src/policy.rs) and [`src/server.rs`](../src/server.rs). All command families in the findings below use target `dui`.

The bundled [DigiLock application manual](Digilock_Manual.pdf), §§8.2.1.1, 8.2.1.7, and 8.2.1.8, describes scan, LI, and PDH modulation amplitudes as **peak-to-peak**. Response/network-analyzer stimulus amplitude is also explicitly **peak-to-peak**, verified in §8.2.3, printed p. 30 (PDF p. 34). PID limits are described as relative to the initial output offset (§8.2.1.4, printed p. 23). These conventions do not verify actual output summation/routing, clipping location and applicability, response-stimulus scaling through the selected output and actuator, or relock amplitude/center semantics. Those combined-output details remain **unverified for the apparatus**. Consult the manuals and actual software/firmware before relying on a combined-output formula. This list is not an exhaustive assessment of every firmware-specific command or hardware failure mode.

Here, a "bypass" includes an otherwise authorized command that changes hardware indirectly or produces an unsafe combined result. It does not necessarily bypass command authorization itself.

## What the wrapper currently enforces

- Writes require `--allow-writes` or its environment equivalent, `DIGILOCK_ALLOW_WRITES=true`. An inherited environment value can enable writes even when the flag is omitted. For explicitly read-only review, omit the flag and set `DIGILOCK_ALLOW_WRITES=false` in the launched process's environment.
- With a configuration file loaded, sets and raw assignments obey its allowlist/denylist and per-command value limits. Startup assignments obey the same checks.
- Optional `write_policy.scan_range` checks only changes to `offset:value` and `scan:amplitude`, using a fresh counterpart query and:

  ```text
  lower = offset:value - scan:amplitude / 2
  upper = offset:value + scan:amplitude / 2
  ```

- The query/check/write sequence cannot be interleaved by other requests sharing this wrapper's RCI client and port.
- The scan check does **not** verify output selection, account for other signal contributions, run on enable/routing commands, continuously monitor outputs, or correct existing unsafe state.
- Queries are unaffected by the write policy; existing protocol/catalog validation still applies.

A JSON value limit constrains assignments to that command. It is not a clamp on physical output voltage or on changes the DigiLock software makes internally.

## Review procedure after each change

Perform document/configuration review first. Do not exercise a dangerous bypass on a connected laser to test whether protection works. Use mock endpoints or a separately authorized, safely isolated test setup for behavioral tests. Even a process launch can write hardware if `initial_values` is nonempty.

- [ ] Record the configuration revision, wrapper version, DMS/DUI software version, module identity, host/ports, and wiring being reviewed.
- [ ] Define the actual safe voltage or other physical limits for **each output and attached actuator**, including units, external scaling, polarity, and any required margin.
- [ ] Confirm the MCP launch loads the intended configuration file and that clients cannot modify the file or launch arguments. Inspect the effective environment, especially inherited `DIGILOCK_ALLOW_WRITES`; absence of the flag alone does not ensure read-only operation. Restart is required to load edits; review startup effects before restarting.
- [ ] Identify every writable command. In allowlist mode this starts with `commands`; in denylist mode it includes **all commands not listed**, including firmware-specific ones. A `limits` entry neither grants allowlist permission nor implicitly denies other commands.
- [ ] Review every permitted value, not just the current value. For each permitted command, ask whether it can change routing, activate stored settings, disable protection, reset an output, or update another parameter indirectly.
- [ ] Review all findings below. For each, record either that the relevant actions are denied, or the independently verified mitigation and evidence. "Not used normally" is not a mitigation if the action is allowed.
- [ ] Verify relevant initial/live states: denying a write does not ensure that its current hardware value is safe. Disabled sources, inactive controller settings, and settings changed in the DUI still matter if another allowed action can activate them.
- [ ] Review startup order and every intermediate state, including partial failure and repeated process launch. There is no rollback or automatic read-back verification.
- [ ] Review every reachable DUI port. The same target-level policy applies to all DUI ports, but attached modules and safe physical ranges may differ. A default port does not restrict requests to that one DUI.
- [ ] Identify external change paths: local DUI actions, another wrapper/direct RCI client, automatic controller behavior, and other hardware/software updates.
- [ ] Test policy acceptance/rejection and fresh-state behavior in mocks. Separately record any operator-authorized hardware verification. Passing wrapper tests is not evidence that the apparatus itself is safe.

## Finding 1: output selection and scan activation

**Commands:** `offset:output`, `scan:output`, `scan:enable`.

**Path:** `offset:value` is described as the offset of the selected output channel. The current scan check does not confirm that this selected channel is the scan destination. It can therefore check the wrong offset. Changing the scan destination or enabling an existing scan also does not invoke the combined-range check.

**Review:**

- [ ] The offset selection and scan destination refer to the intended common physical output whenever relying on `scan_range`.
- [ ] Output selection cannot be changed to an unreviewed destination, or a verified destination-aware guard exists. Fixed `allowed_values` restrict writes, but do not establish the current selection.
- [ ] Scan activation is denied unless the current destination and complete resulting envelope have independently verified protection. A previous successful amplitude write is not proof of the current state.
- [ ] Every output that can be selected has reviewed safe settings, not just the output selected during configuration review.

**Current status:** no routing/activation-aware scan guard. Use a narrow policy and independently verified initial routing; do not claim the scan rule protects arbitrary output selection.

## Finding 2: LI/PDH modulation can add another excursion

**Commands:**

- `li:modulation:amplitude`, `li:modulation:output`, `li:modulation:enable`
- `pdh:modulation:amplitude`, `pdh:modulation:output`, `pdh:modulation:enable`

**Path:** where scan and modulation are summed on a common output, individually permitted amplitudes can exceed the physical limit together. Enabling modulation or moving it to another output can activate this combined effect without changing the scan parameters.

For a **verified additive path with centered peak-to-peak generators**, a conservative envelope includes:

```text
lower = offset - scan_amplitude / 2 - modulation_amplitude / 2
upper = offset + scan_amplitude / 2 + modulation_amplitude / 2
```

Include other contributions only according to verified hardware semantics. Do not assume that LI and PDH are simultaneously active, mutually exclusive, or always summed; verify the actual mode/routing rules.

**Review:**

- [ ] Modulation amplitude, activation, and destination changes are denied unless their full combined effect is bounded.
- [ ] Any shared-output calculation includes all sources that can become active, with verified units and amplitude conventions.
- [ ] Controller/locking actions that may activate modulation internally have also been reviewed; checking only explicit modulation enable writes is insufficient if another action can activate it.

**Current status:** not included in `scan_range`.

## Finding 3: PID correction, relative limits, and disabled clamps

**Commands:** PID1/PID2 `:output`, `:gain`, `:proportional`, `:integral`, `:differential`, `:input`, `:setpoint`, `:sign`, `:slope`, `:lock:enable`, and `:lock:hold`; plus:

- `pid1:limit:min`, `pid1:limit:max`, **`pid1:limit enable`**
- `pid2:limit:min`, `pid2:limit:max`, `pid2:limit:enable`

The spelling of the PID1 limit-enable command above is the catalog spelling; it contains a space.

**Path:** bounded gains do not guarantee bounded output. Feedback depends on input and controller state, and can evolve without another RCI write. The catalog describes PID min/max as **relative output level** limits, not final-output clamps. A PID correction can combine with offset, scan, modulation, or another controller where the hardware permits it. Disabling limits, enabling/releasing a controller, or changing its destination can remove an assumed bound.

**Review:**

- [ ] Verify the PID limit reference, units, clipping location, and whether limits apply in every permitted operating/reset/relock mode.
- [ ] Any relied-upon limit enable remains active; its current state is verified and allowed actions cannot disable it indirectly.
- [ ] Min/max remain ordered and their entire permitted range fits the reviewed output budget, accounting for other contributions.
- [ ] Output destination, input, polarity, gains, setpoint, lock activation, and hold release cannot select an unreviewed operating state.
- [ ] Protection covers the controller's possible future output, not just a fresh instantaneous output reading. A live reading alone cannot bound continuous feedback.

**Current status:** no combined PID/output-envelope guard. Do not treat per-gain limits or a one-time PID output query as a physical-output guarantee.

## Finding 4: relock uses separate scan controls

**Commands:** each of `autolock:relock:`, `pid1:relock:`, and `pid2:relock:` has `amplitude`, `output`, `enable`, and `frequency` controls.

**Path:** these controls configure a separate relock scan. The ordinary `scan:amplitude` check does not validate them, and an enabled automatic relock can start later without another MCP assignment.

**Review:**

- [ ] Relock is disabled and cannot be activated directly or indirectly unless its envelope has verified protection.
- [ ] Before permitting relock, verify amplitude units/convention, scan center/reference, destination, controller interactions, and whether the relied-upon PID limits actually apply.
- [ ] Protection covers automatic transitions and any permitted changes of relock settings or destination, not only the state at the instant a command is accepted.

**Current status:** no relock-envelope guard. Do not reuse `offset ± amplitude / 2` without verifying relock semantics.

## Finding 5: bulk PID parameter transfer avoids destination checks

**Commands:** `simulation:pid:send parameters`, `simulation:selected pid`, and the staged `simulation:pid:*` settings.

**Path:** the catalog explicitly describes `simulation:pid:send parameters` as sending chosen parameters to the selected PID. The wrapper validates the assignment to the transfer command, not each destination PID parameter that the software updates. Restrictions on direct PID gain/input/output writes therefore do not necessarily cover this route.

**Review:**

- [ ] `simulation:pid:send parameters` is denied unless a dedicated, verified guard checks the selected destination and every transferred setting against the destination policy.
- [ ] Do not assume simulation-related commands are all hardware-neutral, or that restricting staged values alone is equivalent to validating the complete destination policy.

**Current status:** no destination-policy expansion for this bulk action. Denying the transfer command is the straightforward mitigation.

## Finding 6: indirect activation through AutoLock, analog control, and measurements

**Commands:**

- `autolock:controller:pid1`, `autolock:controller:pid2`, `autolock:controller:analog`
- `autolock:enable`, `autolock:lock:enable`, `autolock:lock:hold`, `autolock:lock:mode`, `autolock:lock:strategy`
- `analog:lock:enable`, `analog:proportional`, `analog:sign`, `analog:slope`
- `response:modulation:amplitude`, `response:modulation:output`, `response:start`

**Path:** AutoLock can activate selected controllers internally, so denying only direct PID/analog enable writes is insufficient. Analog gain limits do not establish an output envelope; no analog-specific min/max clamp is identified in the bundled catalog. Response measurements add a stimulus to the selected output, so starting a measurement is not passive observation. The application manual §8.2.3, printed p. 30, explicitly specifies peak-to-peak stimulus amplitude; this verifies the amplitude convention, not the complete physical excursion or its scaling and summation with other sources.

**Review:**

- [ ] Controller activation, hold release, operating modes, and automatic strategies are denied unless all reachable states have reviewed protection.
- [ ] Review indirect modulation/relock activation as part of locking, not just the directly exposed enable controls.
- [ ] Analog control has independently verified bounds and routing before it can be activated.
- [ ] Measurement start, stimulus amplitude, and stimulus destination are denied unless the stimulus's scaling and combined output effect are verified and bounded.

**Current status:** no state-aware guard for these actions. Response stimulus **peak-to-peak convention is document-verified**; exact automatic transitions, selected-output/actuator scaling, clipping, and combined physical-output effects still require apparatus-specific verification.

## Additional checks: supervision, reset, and external access

- **Supervision is not necessarily clipping.** `autolock:window:*`, `pid1:window:*`, and `pid2:window:*` descriptions concern a monitored channel, controller halt, and reset/relock. Do not interpret `minout`/`maxout` as final-output voltage clamps merely because of their names. Review the detection channel, enable state, threshold relationships, and subsequent actions.
- **Turning off can change output.** `pid1:turnoff reset:manual`, `pid1:turnoff reset:autolock`, and the PID2 equivalents can configure resets on controller turn-off. Review window reset controls too. Do not assume a permitted `false`/disable write holds the output unchanged or is inherently safe.
- **No configuration file means no operator limits.** With writes enabled and no file loaded, raw access retains the unrestricted legacy escape hatch. The launch command is part of the safety configuration.
- **Denylist mode is open-ended.** New/unknown firmware commands remain available unless denied. Prefer a narrow allowlist for an auditable set of permitted actions.
- **External changes bypass wrapper policy entirely.** Local DUI actions, independent automation, and untrusted access to the launch/configuration can invalidate reviewed assumptions. The wrapper's lock does not make the hardware state immutable.

## Review record

Keep a completed record with each approved configuration revision. Mark findings as **blocked**, **mitigated with verified evidence**, **unverified**, or **not applicable with justification**. Unverified findings affecting a reachable action leave the safety review incomplete.

| Item | Record |
| --- | --- |
| Review date and reviewer | |
| Configuration path and revision/hash | |
| Wrapper version/revision and launch arguments/environment | |
| DMS/DUI software/firmware and module identity | |
| Reachable host/ports, output wiring, and external scaling | |
| Safe physical bounds and their source | |
| Initial-state assumptions and how they are established | |
| Finding 1: routing and scan activation | |
| Finding 2: LI/PDH modulation | |
| Finding 3: PID correction and clamps | |
| Finding 4: relock | |
| Finding 5: bulk parameter transfer | |
| Finding 6: indirect activation and stimulus | |
| Supervision/reset and external-access review | |
| Startup ordering, partial failure, and restart review | |
| Mock test results and separately authorized hardware evidence | |
| Remaining uncertainties and required actions | |
| Review outcome and permitted operating assumptions | |

Re-review when any recorded assumption changes. This document records known gaps; it does not add enforcement to the wrapper.
