# Tests in real hosts

Automated tests and the formats' official validators run in CI on every
commit. This page records tests done by hand inside real hosts, which CI
cannot do: what was tried, how it was measured, and what was found, good and
bad. Each entry names the commit it tested.

ASIO is a trademark of Steinberg Media Technologies GmbH, registered in Europe and other countries.

## REAPER 7.82, Windows 11, 14 October 2026

**Setup.** REAPER 7.82 (evaluation licence) on Windows 11, audio through
ASIO at 44.1 kHz with 882-sample buffers. Both example plugins,
`plugins/pluck` (an instrument) and `plugins/duck` (a sidechain effect), built
in release from commit 8e16422 with `cargo mangue install --release`, in both
formats, into the per-user plugin folders. Renders were written as 32-bit
float WAV and compared sample by sample with a script; the project files and
renders were kept on the test machine.

**What worked.**

| Test | Result |
|---|---|
| Discovery | REAPER listed all four plugins (CLAP and VST 3 of each) with the right vendor and category, and the instrument as an instrument. The CLAP versions appeared only after a manual "Clear cache and re-scan" in REAPER's CLAP preferences, although the per-user CLAP folder is on its default path |
| VST 3 class id | REAPER's project file records the same class id the wrapper derives (`E3AC906E0FE4AF333672CEE11FA34823` for the pluck) |
| CPU | 0.23 % of the machine for the pluck playing (REAPER's performance meter; the whole of REAPER at 5 %) |
| Render | A 24.8 s render of a recorded MIDI part: no NaN or infinity, no denormals, no DC offset, largest step between consecutive samples 0.18 (a pluck's attack), peak −4.71 dBFS, as REAPER measured too |
| Parameters while playing | Moving the cutoff while playing changed the sound at once, with its value shown in hertz on a logarithmic scale |
| State | All six parameters at their extremes, the project saved, closed and reopened: every value came back exactly, in both formats. The plugin's state is stored as readable text by parameter id |
| Automation | An envelope sweeping the CLAP pluck's cutoff from minimum to maximum and back over 24 s: the spectral centroid of the render rose from 476 Hz to 2231 Hz at 12 s and fell back to 462 Hz, following the envelope |
| Sidechain | REAPER showed the duck's four inputs by the names the wrapper declares (Main 1, Main 2, Sidechain 1, Sidechain 2). With nothing in the sidechain the duck was transparent; with the track routed into it, the level fell by 12.00 dB at every point measured, the value of its depth parameter |
| Sample rate | A render at 96 kHz gave the same pitch for the first note (261.18 Hz) and the same peak as at 44.1 kHz |
| Removing plugins while playing | No crash |

**What was found.** Recorded in [TECH_DEBT.md](TECH_DEBT.md) as items 41 to
43:

- The VST 3 wrapper's hidden MIDI-controller parameters are listed in
  REAPER's generic parameter view (REAPER ignores `kIsHidden`), below the
  plugin's own parameters. The CLAP versions show only the real parameters.
- Two renders of the same project with the same plugin state were identical
  for the first 12.8 s, then differed by one float32 unit in the last place
  (about 134 dB below the signal) once many voices overlapped; the point
  where they began to differ changed between renders, in both formats.
- With every parameter at its maximum the pluck reaches +27 dBFS, and its
  tuning is about 3 cents flat at middle C.

**Not tested here.** Plugin editors (the example plugins had none on that
commit), the LV2 format, and a hardware MIDI keyboard.

## REAPER 7.82, Windows, 19 October 2026

**Setup.** REAPER 7.82 on Windows, with the CLAP builds of `plugins/pluck`
and `plugins/aratu-bridge`, and for the bridge a page in Chromium served
from `http://127.0.0.1`. The commit tested was not recorded.

**What worked.**

| Test | Result |
|---|---|
| Editor | Mangue Pluck's CLAP editor opened in REAPER and showed its title and knobs. Working the knobs was not tested |
| Output ceiling | With every parameter of Mangue Pluck at its maximum, REAPER's master peaked at −1.1 dBFS, under the plugin's ceiling of −1 dBFS (`OUTPUT_CEILING_DB`) |
| Aratu Bridge | The page sent 44 events; the plugin applied 44, with none late and none dropped, following REAPER's tempo |

**What was found.** An overload reported at first was not the plugin's:
the Mangue Pluck installed on the machine was a build from before its
output ceiling. Reinstalling the example plugins before a test in a DAW is
now a step in [RELEASING.md](RELEASING.md).

**Not tested here.** Working an editor's knobs in a host, the VST 3
editors, Mangue Duck's editor, the bridge from an `https://` page or in
another browser or DAW.
